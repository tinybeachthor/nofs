use std::path::PathBuf;

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

#[tauri::command]
fn stream_file(path: String) -> Result<Response, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    Ok(Response::new(bytes))
}

#[tauri::command]
fn read_file(path: String) -> Result<FilePreview, String> {
    use std::io::Read;
    let file = std::fs::File::open(&path).map_err(|e| format!("{path}: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    std::io::BufReader::new(file)
        .take(PREVIEW_LIMIT)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    let truncated = meta.len() > PREVIEW_LIMIT;
    match String::from_utf8(buf) {
        Ok(content) => Ok(FilePreview::Text { content, truncated }),
        Err(_) => Ok(FilePreview::Binary),
    }
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

#[cfg(target_os = "macos")]
fn set_macos_background(window: &tauri::WebviewWindow) {
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let ns_window = window.ns_window().unwrap() as *mut objc::runtime::Object;
        let color: *mut objc::runtime::Object = msg_send![
            class!(NSColor),
            colorWithRed: (28.0_f64 / 255.0)
            green: (28.0_f64 / 255.0)
            blue: (30.0_f64 / 255.0)
            alpha: 1.0_f64
        ];
        let _: () = msg_send![ns_window, setBackgroundColor: color];
    }

    window.with_webview(|wv| {
        unsafe {
            use objc::{class, msg_send, sel, sel_impl};
            let wkwebview = wv.inner() as *mut objc::runtime::Object;
            let color: *mut objc::runtime::Object = msg_send![
                class!(NSColor),
                colorWithRed: (28.0_f64 / 255.0)
                green: (28.0_f64 / 255.0)
                blue: (30.0_f64 / 255.0)
                alpha: 1.0_f64
            ];
            let _: () = msg_send![wkwebview, setUnderPageBackgroundColor: color];
            let no: *mut objc::runtime::Object = msg_send![class!(NSNumber), numberWithBool: 0u8];
            let key: *mut objc::runtime::Object = msg_send![
                class!(NSString),
                stringWithUTF8String: b"drawsBackground\0".as_ptr()
            ];
            let _: () = msg_send![wkwebview, setValue: no forKey: key];
        }
    }).ok();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![list_dir, read_file, stream_file])
        .setup(|app| {
            let window = app.get_webview_window("main").unwrap();
            #[cfg(target_os = "macos")]
            set_macos_background(&window);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
