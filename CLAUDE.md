# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

JS/frontend (use **bun** — lockfile is `bun.lock`, not npm/pnpm):

- `bun install` — install JS deps
- `bun run dev` — Vite dev server on port 1420 (port is fixed by Tauri convention)
- `bun run build` — `tsc` typecheck + Vite production build into `dist/`
- `bun run tauri dev` — run the desktop app; Tauri spawns `bun run dev` automatically via `beforeDevCommand`
- `bun run tauri build` — build distributable bundle

Rust/backend (from `src-tauri/`):

- `cargo check` / `cargo build` / `cargo test`

No linter or JS test runner is configured.

## Architecture

Two-process Tauri 2 desktop app (React 19 + TS frontend, Rust backend).

- **Frontend** — `src/`, entry `src/main.tsx` → `src/App.tsx`. Vite builds to `dist/`, which Tauri consumes via `frontendDist: "../dist"` in `src-tauri/tauri.conf.json`.
- **Backend** — Rust crate in `src-tauri/` (lib name `nofs_lib`, binary `nofs`). Entry: `src-tauri/src/main.rs` → `lib.rs::run()`.
- **Frontend ↔ Rust bridge** — `invoke("cmd_name", args)` from `@tauri-apps/api/core` calls `#[tauri::command]` functions registered inside `tauri::Builder::default().invoke_handler(tauri::generate_handler![...])` in `src-tauri/src/lib.rs`. See the `list_dir` and `read_file` commands for the canonical pattern (including `Result<T, String>` error conversion and `Serialize` return types).
- **Filesystem access lives entirely in Rust.** The frontend never touches the FS directly — no `tauri-plugin-fs`, no FS capabilities. Add new FS features as `#[tauri::command]` functions; resolve well-known directories via `app.path()` (requires `use tauri::Manager;`).
- **Capabilities/permissions** — declared in `src-tauri/capabilities/`; required for any Tauri plugin/API surface exposed to the webview.
- **Config** — `src-tauri/tauri.conf.json` controls window, bundling, dev URL, identifier (`com.martin.nofs`).
- **Plugins enabled** — `tauri-plugin-opener`.

## VFS overlay & persistence model

The backend does not read the real filesystem directly; it serves everything through a `vfs::OverlayFS` (`vfs` crate) held in a `Mutex<VfsInner>` inside `VfsState`. The overlay is a stack of layers, resolved **top-down** (first layer that has a path wins; `OverlayFS` writes go only to layer 0):

1. **live memory** (`MemoryFS`) — layer 0, the write target for dropped files.
2. **persisted snapshots** (`Vec<VfsPath>`, newest-first) — frozen `MemoryFS` layers, each loaded from a numbered `.tar.gz`.
3. **home** (`PhysicalFS` over the home dir) — read-only base layer.

Flow:
- **Drag-drop** (`add_dropped_files`): reads OS files with `std::fs::read` and writes them into the live memory layer at the currently-viewed dir (collisions overwrite). The window becomes "dirty".
- **Persist** (`persist`): compresses the whole live memory layer into `~/.nofs/{N:05}.tar.gz` (tar + gzip via `tar`/`flate2`), freezes that layer into `persisted`, then starts a fresh empty memory layer. Each persist is a separate numbered archive, so file history is retained across layers.
- **Startup** (`setup`): loads every `~/.nofs/*.tar.gz` back as persisted layers (newest-first) and starts an empty memory layer.
- `list_dir` hides the `.nofs` store from the root listing and tags each entry with a `managed` flag (true when the path is served by the memory or a persisted layer, false when it comes only from home).

Archive/numbering/overlay helpers (`archive_memory`, `load_archive`, `build_overlay`, `is_managed`, etc.) have unit tests under `#[cfg(test)]` in `lib.rs` (`cargo test`); the thin `#[tauri::command]` wrappers are not unit-tested.

## UI

- Dark macOS-style design (`#1c1c1e` background, system fonts).
- Files and folders rendered as tiles in a responsive CSS grid (`repeat(auto-fill, minmax(120px, 1fr))`).
- SVG icons defined inline in `App.tsx`: `FolderIcon`, `FileIcon`, `ImageIcon`, `PdfIcon` (chosen per file extension), and `HomeIcon` (root breadcrumb).
- **Layer indicator** — entries served by the memory/persisted layers (`managed: true`) get an accent dot badge (`.fb-tile-badge`) and a blue-tinted name (`.fb-tile-managed`); read-only home files render plain.
- **Drag-drop & Persist** — dropping OS files onto the window adds them to the live layer (drop highlight via `.fb-dragging`); a **Persist** button (`.fb-persist`) appears in the topbar whenever the live layer is dirty. Drag-drop uses `getCurrentWebview().onDragDropEvent` from `@tauri-apps/api/webview` (enabled by default, no capability needed) and the `drop` payload carries real OS absolute paths.
- Topbar shows a clickable breadcrumb trail — each path segment navigates directly to that directory; the current segment is non-interactive.
- Clicking a file tile opens a slide-in preview panel (`PreviewPanel` in `App.tsx`) on the right. Text shows inline (up to 64 KB via `read_file`); images and PDFs stream as blob URLs via the `stream_file` command; other binaries show a "no preview" message. The panel is resizable via a drag handle and sits side-by-side with the grid inside `.fb-content`.
- Window uses the platform's standard title bar; no platform-specific native window customization, keeping the app cross-platform.

## When adding a new Rust command

1. Define `#[tauri::command] fn foo(...) -> Result<T, String>` in `src-tauri/src/lib.rs` (return types must be `Serialize`; map errors to `String` with `.map_err(|e| e.to_string())`). To serve files through the VFS, take `vfs: tauri::State<'_, VfsState>` and grab the overlay with `vfs.inner.lock().unwrap().root.clone()`.
2. Register it in `generate_handler![list_dir, read_file, stream_file, add_dropped_files, persist, foo]`.
3. If it uses a Tauri plugin API, grant the capability in `src-tauri/capabilities/default.json`. Plain `std::fs` / `std::env` needs no capability.
