use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::ipc::Response;
use tauri::Manager;

#[derive(Serialize)]
struct DirEntry {
    name: String,
    path: String,
    is_dir: bool,
}

#[derive(Serialize)]
struct Listing {
    path: String,
    parent: Option<String>,
    entries: Vec<DirEntry>,
}

#[derive(Serialize)]
#[serde(tag = "kind")]
enum FilePreview {
    Text { content: String, truncated: bool },
    Binary,
}

const PREVIEW_LIMIT: u64 = 64 * 1024;

struct VfsInner {
    root: vfs::VfsPath,           // current OverlayFS, used by read commands
    memory: vfs::VfsPath,         // live MemoryFS = overlay layer 0 = write target
    persisted: Vec<vfs::VfsPath>, // frozen snapshot layers, newest-first
}

struct VfsState {
    inner: Mutex<VfsInner>,
    persist_dir: PathBuf, // ~/.nofs
    home: vfs::VfsPath,   // PhysicalFS(home), read-only base layer
}

fn build_overlay(
    memory: &vfs::VfsPath,
    persisted: &[vfs::VfsPath],
    home: &vfs::VfsPath,
) -> vfs::VfsPath {
    let mut layers = vec![memory.clone()];
    layers.extend(persisted.iter().cloned());
    layers.push(home.clone());
    vfs::VfsPath::new(vfs::OverlayFS::new(&layers))
}

fn is_dirty(mem: &vfs::VfsPath) -> bool {
    mem.read_dir().map(|mut it| it.next().is_some()).unwrap_or(false)
}

fn vfs_resolve(root: &vfs::VfsPath, path: &str) -> Result<vfs::VfsPath, String> {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        Ok(root.clone())
    } else {
        root.join(rel).map_err(|e| e.to_string())
    }
}

/// Compress every file in a MemoryFS layer into a single .tar.gz archive.
fn archive_memory(mem: &vfs::VfsPath, out: &Path) -> Result<(), String> {
    use std::io::Read;
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(enc);
    for entry in mem.walk_dir().map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        if meta.file_type != vfs::VfsFileType::File {
            continue;
        }
        let rel = entry.as_str().trim_start_matches('/');
        if rel.is_empty() {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .open_file()
            .map_err(|e| e.to_string())?
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, rel, &bytes[..])
            .map_err(|e| e.to_string())?;
    }
    let enc = builder.into_inner().map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// Rebuild a MemoryFS layer from a .tar.gz archive.
fn load_archive(path: &Path) -> Result<vfs::VfsPath, String> {
    use std::io::{Read, Write};
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    let root = vfs::VfsPath::new(vfs::MemoryFS::new());
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let rel = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
        if rel.is_empty() {
            continue;
        }
        let dest = root.join(&rel).map_err(|e| e.to_string())?;
        dest.parent().create_dir_all().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        dest.create_file()
            .map_err(|e| e.to_string())?
            .write_all(&bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(root)
}

fn archive_numbers(dir: &Path) -> Vec<u32> {
    let mut nums = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            if let Some(stem) = name.to_string_lossy().strip_suffix(".tar.gz") {
                if let Ok(n) = stem.parse::<u32>() {
                    nums.push(n);
                }
            }
        }
    }
    nums
}

fn next_layer_number(dir: &Path) -> u32 {
    archive_numbers(dir).into_iter().max().unwrap_or(0) + 1
}

/// Load all persisted archives as layers, newest-first.
fn load_all_archives(dir: &Path) -> Vec<vfs::VfsPath> {
    let mut nums = archive_numbers(dir);
    nums.sort_unstable_by(|a, b| b.cmp(a));
    nums.into_iter()
        .filter_map(|n| load_archive(&dir.join(format!("{n:05}.tar.gz"))).ok())
        .collect()
}

#[tauri::command]
fn stream_file(path: String, vfs: tauri::State<'_, VfsState>) -> Result<Response, String> {
    use std::io::Read;
    let root = vfs.inner.lock().unwrap().root.clone();
    let vfs_path = vfs_resolve(&root, &path)?;
    let mut file = vfs_path.open_file().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

#[tauri::command]
fn read_file(path: String, vfs: tauri::State<'_, VfsState>) -> Result<FilePreview, String> {
    use std::io::Read;
    let root = vfs.inner.lock().unwrap().root.clone();
    let vfs_path = vfs_resolve(&root, &path)?;
    let meta = vfs_path.metadata().map_err(|e| e.to_string())?;
    let file = vfs_path.open_file().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.take(PREVIEW_LIMIT).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let truncated = meta.len > PREVIEW_LIMIT;
    match String::from_utf8(buf) {
        Ok(content) => Ok(FilePreview::Text { content, truncated }),
        Err(_) => Ok(FilePreview::Binary),
    }
}

#[tauri::command]
fn list_dir(path: Option<String>, vfs: tauri::State<'_, VfsState>) -> Result<Listing, String> {
    let root = vfs.inner.lock().unwrap().root.clone();
    let dir = match path {
        Some(p) => vfs_resolve(&root, &p)?,
        None => root.clone(),
    };

    let at_root = dir.as_str().is_empty();
    let mut entries: Vec<DirEntry> = dir
        .read_dir()
        .map_err(|e| format!("{}: {e}", dir.as_str()))?
        .filter_map(|child| {
            let meta = child.metadata().ok()?;
            Some(DirEntry {
                name: child.filename(),
                path: child.as_str().to_string(),
                is_dir: meta.file_type == vfs::VfsFileType::Directory,
            })
        })
        // hide the snapshot store from the root listing
        .filter(|e| !(at_root && e.name == ".nofs"))
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(Listing {
        path: dir.as_str().to_string(),
        parent: if at_root {
            None
        } else {
            Some(dir.parent().as_str().to_string())
        },
        entries,
    })
}

/// Read dropped OS files and write them into the live memory layer.
#[tauri::command]
fn add_dropped_files(
    dest_dir: String,
    paths: Vec<String>,
    vfs: tauri::State<'_, VfsState>,
) -> Result<bool, String> {
    use std::io::Write;
    let memory = vfs.inner.lock().unwrap().memory.clone();
    let base = dest_dir.trim_start_matches('/');
    for p in &paths {
        let os_path = Path::new(p);
        let filename = match os_path.file_name() {
            Some(f) => f.to_string_lossy().to_string(),
            None => continue,
        };
        let bytes = std::fs::read(os_path).map_err(|e| format!("{p}: {e}"))?;
        let rel = if base.is_empty() {
            filename
        } else {
            format!("{base}/{filename}")
        };
        let dest = memory.join(&rel).map_err(|e| e.to_string())?;
        dest.parent().create_dir_all().map_err(|e| e.to_string())?;
        dest.create_file()
            .map_err(|e| e.to_string())?
            .write_all(&bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(is_dirty(&memory))
}

/// Snapshot the live memory layer into a new numbered archive, then start fresh.
#[tauri::command]
fn persist(vfs: tauri::State<'_, VfsState>) -> Result<bool, String> {
    let mut inner = vfs.inner.lock().unwrap();
    if !is_dirty(&inner.memory) {
        return Ok(false);
    }
    let n = next_layer_number(&vfs.persist_dir);
    let out = vfs.persist_dir.join(format!("{n:05}.tar.gz"));
    archive_memory(&inner.memory, &out)?;

    let frozen = inner.memory.clone();
    inner.persisted.insert(0, frozen);
    inner.memory = vfs::VfsPath::new(vfs::MemoryFS::new());
    inner.root = build_overlay(&inner.memory, &inner.persisted, &vfs.home);
    Ok(false)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_dir,
            read_file,
            stream_file,
            add_dropped_files,
            persist
        ])
        .setup(|app| {
            let home = app.path().home_dir()?;
            let persist_dir = home.join(".nofs");
            std::fs::create_dir_all(&persist_dir)?;

            let home_fs = vfs::VfsPath::new(vfs::PhysicalFS::new(&home));
            let persisted = load_all_archives(&persist_dir);
            let memory = vfs::VfsPath::new(vfs::MemoryFS::new());
            let root = build_overlay(&memory, &persisted, &home_fs);

            app.manage(VfsState {
                inner: Mutex::new(VfsInner { root, memory, persisted }),
                persist_dir,
                home: home_fs,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
