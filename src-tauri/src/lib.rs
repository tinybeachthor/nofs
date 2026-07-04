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
    // true when this path exists in the live memory or a persisted snapshot
    // layer (i.e. it is "ours"); false when it comes only from the home FS.
    managed: bool,
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

/// A frozen, persisted snapshot layer plus the metadata needed to identify it
/// (its archive number and creation time) in the history picker.
#[derive(Clone)]
struct Snapshot {
    number: u32,         // archive number, from {N:05}.tar.gz filename
    created_ms: u64,     // archive file mtime, millis since UNIX_EPOCH
    layer: vfs::VfsPath, // frozen MemoryFS
}

#[derive(Serialize)]
struct SnapshotInfo {
    number: u32,
    created_ms: u64,
}

struct VfsInner {
    root: vfs::VfsPath,       // current OverlayFS, used by read commands
    memory: vfs::VfsPath,     // live MemoryFS = overlay layer 0 = write target
    persisted: Vec<Snapshot>, // frozen snapshot layers, newest-first
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

/// Whether `path` is served by any of the given "ours" layers (live memory
/// and/or persisted snapshots), as opposed to coming only from the read-only
/// home filesystem. The caller supplies the layer set for the active view.
fn is_managed(layers: &[vfs::VfsPath], path: &str) -> bool {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return false;
    }
    layers.iter().any(|layer| {
        layer.join(rel).map(|p| p.exists().unwrap_or(false)).unwrap_or(false)
    })
}

/// Epoch-millis mtime of a file, or 0 if it cannot be determined.
fn file_mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build the read overlay and the "managed" layer set for a given view.
///
/// `None` is the live view: the cached root overlay, with memory + every
/// persisted layer counting as managed. `Some(n)` is a read-only time-machine
/// view: persisted layers with number <= n stacked over the current home dir
/// (no live memory, no newer snapshots); only those layers count as managed.
fn view(
    inner: &VfsInner,
    home: &vfs::VfsPath,
    snapshot: Option<u32>,
) -> (vfs::VfsPath, Vec<vfs::VfsPath>) {
    match snapshot {
        None => {
            let mut managed = vec![inner.memory.clone()];
            managed.extend(inner.persisted.iter().map(|s| s.layer.clone()));
            (inner.root.clone(), managed)
        }
        Some(n) => snapshot_view(&inner.persisted, home, n),
    }
}

/// The `Some(n)` arm of [`view`], factored out so it is unit-testable without a
/// Tauri app handle. Returns the read-only overlay (persisted layers <= n over
/// home) and the managed layer set (just those persisted layers).
fn snapshot_view(
    persisted: &[Snapshot],
    home: &vfs::VfsPath,
    n: u32,
) -> (vfs::VfsPath, Vec<vfs::VfsPath>) {
    let managed: Vec<vfs::VfsPath> = persisted
        .iter()
        .filter(|s| s.number <= n)
        .map(|s| s.layer.clone())
        .collect();
    let mut layers = managed.clone();
    layers.push(home.clone());
    (vfs::VfsPath::new(vfs::OverlayFS::new(&layers)), managed)
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

/// Load all persisted archives as snapshot layers, newest-first.
fn load_all_snapshots(dir: &Path) -> Vec<Snapshot> {
    let mut nums = archive_numbers(dir);
    nums.sort_unstable_by(|a, b| b.cmp(a));
    nums.into_iter()
        .filter_map(|n| {
            let path = dir.join(format!("{n:05}.tar.gz"));
            let layer = load_archive(&path).ok()?;
            Some(Snapshot { number: n, created_ms: file_mtime_ms(&path), layer })
        })
        .collect()
}

#[tauri::command]
fn stream_file(
    path: String,
    snapshot: Option<u32>,
    vfs: tauri::State<'_, VfsState>,
) -> Result<Response, String> {
    use std::io::Read;
    let root = {
        let inner = vfs.inner.lock().unwrap();
        view(&inner, &vfs.home, snapshot).0
    };
    let vfs_path = vfs_resolve(&root, &path)?;
    let mut file = vfs_path.open_file().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

#[tauri::command]
fn read_file(
    path: String,
    snapshot: Option<u32>,
    vfs: tauri::State<'_, VfsState>,
) -> Result<FilePreview, String> {
    use std::io::Read;
    let root = {
        let inner = vfs.inner.lock().unwrap();
        view(&inner, &vfs.home, snapshot).0
    };
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
fn list_dir(
    path: Option<String>,
    snapshot: Option<u32>,
    vfs: tauri::State<'_, VfsState>,
) -> Result<Listing, String> {
    let (root, managed) = {
        let inner = vfs.inner.lock().unwrap();
        view(&inner, &vfs.home, snapshot)
    };
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
            let path = child.as_str().to_string();
            Some(DirEntry {
                name: child.filename(),
                managed: is_managed(&managed, &path),
                path,
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

/// Recursively import an OS file or directory into the live `memory` layer under
/// `base` (a VFS-relative dir, no leading slash). Directory structure is
/// preserved beneath `base/<name>`; symlinks (to files or dirs) are skipped
/// entirely, which also prevents following links out of the tree or into loops.
fn import_path(memory: &vfs::VfsPath, base: &str, os_path: &Path) -> Result<(), String> {
    use std::io::Write;
    let meta = std::fs::symlink_metadata(os_path)
        .map_err(|e| format!("{}: {e}", os_path.display()))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let name = match os_path.file_name() {
        Some(f) => f.to_string_lossy().into_owned(),
        None => return Ok(()),
    };
    let rel = if base.is_empty() {
        name
    } else {
        format!("{base}/{name}")
    };
    if meta.is_dir() {
        memory
            .join(&rel)
            .map_err(|e| e.to_string())?
            .create_dir_all()
            .map_err(|e| format!("{rel}: {e}"))?;
        for entry in std::fs::read_dir(os_path).map_err(|e| format!("{}: {e}", os_path.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            import_path(memory, &rel, &entry.path())?;
        }
    } else {
        let bytes = std::fs::read(os_path).map_err(|e| format!("{}: {e}", os_path.display()))?;
        let dest = memory.join(&rel).map_err(|e| e.to_string())?;
        dest.parent().create_dir_all().map_err(|e| e.to_string())?;
        dest.create_file()
            .map_err(|e| e.to_string())?
            .write_all(&bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Read dropped OS files/folders and write them into the live memory layer.
#[tauri::command]
fn add_dropped_files(
    dest_dir: String,
    paths: Vec<String>,
    vfs: tauri::State<'_, VfsState>,
) -> Result<bool, String> {
    let memory = vfs.inner.lock().unwrap().memory.clone();
    let base = dest_dir.trim_start_matches('/');
    for p in &paths {
        import_path(&memory, base, Path::new(p))?;
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
    inner.persisted.insert(
        0,
        Snapshot { number: n, created_ms: file_mtime_ms(&out), layer: frozen },
    );
    inner.memory = vfs::VfsPath::new(vfs::MemoryFS::new());
    let layers: Vec<vfs::VfsPath> = inner.persisted.iter().map(|s| s.layer.clone()).collect();
    inner.root = build_overlay(&inner.memory, &layers, &vfs.home);
    Ok(false)
}

/// List persisted snapshots, newest-first, for the history picker.
#[tauri::command]
fn list_snapshots(vfs: tauri::State<'_, VfsState>) -> Result<Vec<SnapshotInfo>, String> {
    let inner = vfs.inner.lock().unwrap();
    Ok(inner
        .persisted
        .iter()
        .map(|s| SnapshotInfo { number: s.number, created_ms: s.created_ms })
        .collect())
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
            persist,
            list_snapshots
        ])
        .setup(|app| {
            let home = app.path().home_dir()?;
            let persist_dir = home.join(".nofs");
            std::fs::create_dir_all(&persist_dir)?;

            let home_fs = vfs::VfsPath::new(vfs::PhysicalFS::new(&home));
            let persisted = load_all_snapshots(&persist_dir);
            let memory = vfs::VfsPath::new(vfs::MemoryFS::new());
            let layers: Vec<vfs::VfsPath> = persisted.iter().map(|s| s.layer.clone()).collect();
            let root = build_overlay(&memory, &layers, &home_fs);

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn mem() -> vfs::VfsPath {
        vfs::VfsPath::new(vfs::MemoryFS::new())
    }

    fn write(root: &vfs::VfsPath, path: &str, content: &str) {
        let dest = root.join(path).unwrap();
        dest.parent().create_dir_all().unwrap();
        dest.create_file().unwrap().write_all(content.as_bytes()).unwrap();
    }

    fn read(root: &vfs::VfsPath, path: &str) -> String {
        let mut s = String::new();
        root.join(path).unwrap().open_file().unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn archive_roundtrip_preserves_nested_files() {
        let m = mem();
        write(&m, "a.txt", "alpha");
        write(&m, "sub/b.txt", "bravo");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("00001.tar.gz");

        archive_memory(&m, &out).unwrap();
        let loaded = load_archive(&out).unwrap();

        assert_eq!(read(&loaded, "a.txt"), "alpha");
        assert_eq!(read(&loaded, "sub/b.txt"), "bravo");
    }

    #[test]
    fn is_dirty_reflects_contents() {
        let m = mem();
        assert!(!is_dirty(&m));
        write(&m, "x.txt", "1");
        assert!(is_dirty(&m));
    }

    #[test]
    fn next_layer_number_skips_existing_and_ignores_non_archives() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("00001.tar.gz"), b"").unwrap();
        std::fs::write(dir.path().join("00003.tar.gz"), b"").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"").unwrap();
        assert_eq!(next_layer_number(dir.path()), 4);
    }

    #[test]
    fn next_layer_number_empty_dir_is_one() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(next_layer_number(dir.path()), 1);
    }

    #[test]
    fn overlay_resolves_layers_in_order() {
        let home = mem();
        write(&home, "shared.txt", "home");
        write(&home, "only_home.txt", "home");
        let persisted = mem();
        write(&persisted, "shared.txt", "persist");
        let live = mem();

        // empty live layer: persisted shadows home, home still visible elsewhere
        let overlay = build_overlay(&live, std::slice::from_ref(&persisted), &home);
        assert_eq!(read(&overlay, "shared.txt"), "persist");
        assert_eq!(read(&overlay, "only_home.txt"), "home");

        // live layer shadows everything below
        write(&live, "shared.txt", "memory");
        let overlay = build_overlay(&live, std::slice::from_ref(&persisted), &home);
        assert_eq!(read(&overlay, "shared.txt"), "memory");
    }

    #[test]
    fn load_all_snapshots_orders_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let first = mem();
        write(&first, "f.txt", "first");
        archive_memory(&first, &dir.path().join("00001.tar.gz")).unwrap();
        let second = mem();
        write(&second, "f.txt", "second");
        archive_memory(&second, &dir.path().join("00002.tar.gz")).unwrap();

        let snaps = load_all_snapshots(dir.path());
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].number, 2);
        assert_eq!(snaps[1].number, 1);
        assert_eq!(read(&snaps[0].layer, "f.txt"), "second");
        assert_eq!(read(&snaps[1].layer, "f.txt"), "first");
        assert!(snaps[0].created_ms > 0);
    }

    fn snapshot(number: u32, layer: vfs::VfsPath) -> Snapshot {
        Snapshot { number, created_ms: 0, layer }
    }

    #[test]
    fn import_path_recurses_directories() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), b"alpha").unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/b.txt"), b"bravo").unwrap();

        let m = mem();
        import_path(&m, "dest", src.path()).unwrap();

        let root = src.path().file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(read(&m, &format!("dest/{root}/a.txt")), "alpha");
        assert_eq!(read(&m, &format!("dest/{root}/sub/b.txt")), "bravo");
    }

    #[test]
    fn import_path_imports_single_file_at_root_base() {
        let src = tempfile::tempdir().unwrap();
        let file = src.path().join("only.txt");
        std::fs::write(&file, b"solo").unwrap();

        let m = mem();
        import_path(&m, "", &file).unwrap();
        assert_eq!(read(&m, "only.txt"), "solo");
    }

    #[test]
    fn import_path_preserves_empty_dirs_in_memory() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("empty")).unwrap();

        let m = mem();
        import_path(&m, "", src.path()).unwrap();
        let root = src.path().file_name().unwrap().to_string_lossy().to_string();
        let dir = m.join(&format!("{root}/empty")).unwrap();
        assert!(dir.exists().unwrap());
        assert!(dir.is_dir().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn import_path_skips_symlinks() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("real.txt"), b"real").unwrap();
        std::fs::create_dir(src.path().join("realdir")).unwrap();
        std::fs::write(src.path().join("realdir/inner.txt"), b"inner").unwrap();
        std::os::unix::fs::symlink(src.path().join("real.txt"), src.path().join("link.txt")).unwrap();
        std::os::unix::fs::symlink(src.path().join("realdir"), src.path().join("linkdir")).unwrap();

        let m = mem();
        import_path(&m, "", src.path()).unwrap();
        let root = src.path().file_name().unwrap().to_string_lossy().to_string();

        assert!(m.join(&format!("{root}/real.txt")).unwrap().exists().unwrap());
        assert!(m.join(&format!("{root}/realdir/inner.txt")).unwrap().exists().unwrap());
        assert!(!m.join(&format!("{root}/link.txt")).unwrap().exists().unwrap());
        assert!(!m.join(&format!("{root}/linkdir")).unwrap().exists().unwrap());
    }

    #[test]
    fn import_path_errors_when_dir_collides_with_memory_file() {
        let src = tempfile::tempdir().unwrap();
        let clash = src.path().join("x");
        std::fs::create_dir(&clash).unwrap();

        let m = mem();
        write(&m, "x", "i am a file");
        assert!(import_path(&m, "", &clash).is_err());
    }

    #[test]
    fn snapshot_view_filters_layers_by_number() {
        let home = mem();
        write(&home, "shared.txt", "home");

        let l1 = mem();
        write(&l1, "shared.txt", "one");
        let l2 = mem();
        write(&l2, "shared.txt", "two");
        let l3 = mem();
        write(&l3, "shared.txt", "three");
        // newest-first, as stored in VfsInner.persisted
        let persisted = vec![snapshot(3, l3), snapshot(2, l2), snapshot(1, l1)];

        // n between snapshot numbers resolves to the newest layer <= n
        let (root, managed) = snapshot_view(&persisted, &home, 2);
        assert_eq!(read(&root, "shared.txt"), "two");
        assert_eq!(managed.len(), 2);

        // n below the lowest number falls through to home; nothing managed
        let (root, managed) = snapshot_view(&persisted, &home, 0);
        assert_eq!(read(&root, "shared.txt"), "home");
        assert!(managed.is_empty());
    }

    #[test]
    fn is_managed_with_flat_layers() {
        let a = mem();
        write(&a, "in_a.txt", "1");
        let b = mem();
        write(&b, "in_b.txt", "2");
        let layers = [a, b];

        assert!(is_managed(&layers, "/in_a.txt"));
        assert!(is_managed(&layers, "/in_b.txt"));
        assert!(!is_managed(&layers, "/elsewhere.txt"));
        assert!(!is_managed(&layers, "/"));
    }

    #[test]
    fn file_mtime_ms_is_recent_for_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"hi").unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mtime = file_mtime_ms(&path);
        assert!(mtime > 0);
        // within a generous window either side of "now"
        assert!(now.abs_diff(mtime) < 60_000);
    }
}
