use std::path::PathBuf;

use serde::Serialize;
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

#[tauri::command]
fn list_dir(path: Option<String>, app: tauri::AppHandle) -> Result<Listing, String> {
    let dir: PathBuf = match path {
        Some(p) => PathBuf::from(p),
        None => app
            .path()
            .home_dir()
            .map_err(|e| format!("failed to resolve home dir: {e}"))?,
    };

    let read = std::fs::read_dir(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;

    let mut entries: Vec<DirEntry> = read
        .filter_map(|r| r.ok())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path().to_string_lossy().into_owned();
            Some(DirEntry {
                name,
                path,
                is_dir: meta.is_dir(),
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(Listing {
        path: dir.to_string_lossy().into_owned(),
        parent: dir
            .parent()
            .map(|p| p.to_string_lossy().into_owned()),
        entries,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![list_dir])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
