use serde::Serialize;
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

struct VfsState {
    root: vfs::VfsPath,
}

fn vfs_resolve(root: &vfs::VfsPath, path: &str) -> Result<vfs::VfsPath, String> {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        Ok(root.clone())
    } else {
        root.join(rel).map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn stream_file(path: String, vfs: tauri::State<'_, VfsState>) -> Result<Response, String> {
    use std::io::Read;
    let vfs_path = vfs_resolve(&vfs.root, &path)?;
    let mut file = vfs_path.open_file().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

#[tauri::command]
fn read_file(path: String, vfs: tauri::State<'_, VfsState>) -> Result<FilePreview, String> {
    use std::io::Read;
    let vfs_path = vfs_resolve(&vfs.root, &path)?;
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
    let dir = match path {
        Some(p) => vfs_resolve(&vfs.root, &p)?,
        None => vfs.root.clone(),
    };

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
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(Listing {
        path: dir.as_str().to_string(),
        parent: if dir.as_str().is_empty() {
            None
        } else {
            Some(dir.parent().as_str().to_string())
        },
        entries,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![list_dir, read_file, stream_file])
        .setup(|app| {
            let home = app.path().home_dir()?;
            let lower = vfs::VfsPath::new(vfs::PhysicalFS::new(&home));
            let upper = vfs::VfsPath::new(vfs::MemoryFS::new());
            let overlay = vfs::OverlayFS::new(&[lower, upper]);
            app.manage(VfsState { root: vfs::VfsPath::new(overlay) });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
