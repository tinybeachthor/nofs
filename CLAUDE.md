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
2. **persisted snapshots** (`Vec<Snapshot>`, newest-first) — each `Snapshot` wraps a frozen `MemoryFS` `layer` plus metadata: its archive `number` (from the `{N:05}.tar.gz` filename) and `created_ms` (the archive file's mtime, epoch millis).
3. **home** (`PhysicalFS` over the home dir) — read-only base layer.

Flow:
- **Drag-drop** (`add_dropped_files` → `import_path`): recursively imports dropped OS files *and folders* into the live memory layer at the currently-viewed dir, preserving directory structure under `dest/<name>/…`. Symlinks (file or dir) are skipped via `symlink_metadata` (avoids following links out of the tree or into loops). Collisions overwrite; the window becomes "dirty". Note: empty dirs live in memory but do not survive persist, since `archive_memory` only tars files.
- **Persist** (`persist`): compresses the whole live memory layer into `~/.nofs/{N:05}.tar.gz` (tar + gzip via `tar`/`flate2`), freezes that layer into `persisted` as a new `Snapshot`, then starts a fresh empty memory layer. Each persist is a separate numbered archive, so file history is retained across layers.
- **Startup** (`setup`): loads every `~/.nofs/*.tar.gz` back as `Snapshot`s (newest-first, via `load_all_snapshots`) and starts an empty memory layer.
- **Time-machine views**: `list_dir`, `read_file`, and `stream_file` take an optional `snapshot: Option<u32>` param. `None` = the live view (cached `root` overlay). `Some(n)` = a read-only view built per-call by `view()`/`snapshot_view()`: the persisted layers with `number <= n` stacked over the *current* home dir (no live memory, no newer snapshots). Per-call overlay construction is cheap (`VfsPath` layers are Arc-backed clones). Writes always target live memory, so snapshot views are read-only by construction.
- `list_snapshots` returns `Vec<SnapshotInfo>` (number + created_ms, newest-first) for the history picker.
- `list_dir` hides the `.nofs` store from the root listing and tags each entry with a `managed` flag (true when served by one of the active view's "ours" layers — memory + persisted for the live view, or the filtered persisted layers for a snapshot view — false when it comes only from home). `is_managed` takes the flat layer list that `view()` produces.

Archive/numbering/overlay/import helpers (`archive_memory`, `load_archive`, `load_all_snapshots`, `build_overlay`, `snapshot_view`, `is_managed`, `import_path`, `file_mtime_ms`, etc.) have unit tests under `#[cfg(test)]` in `lib.rs` (`cargo test`); the thin `#[tauri::command]` wrappers are not unit-tested.

## UI

- Dark macOS-style design (`#1c1c1e` background, system fonts).
- Files and folders rendered as tiles in a responsive CSS grid (`repeat(auto-fill, minmax(120px, 1fr))`).
- SVG icons defined inline in `App.tsx`: `FolderIcon`, `FileIcon`, `ImageIcon`, `PdfIcon` (chosen per file extension), and `HomeIcon` (root breadcrumb).
- **Layer indicator** — entries served by the memory/persisted layers (`managed: true`) get an accent dot badge (`.fb-tile-badge`) and a blue-tinted name (`.fb-tile-managed`); read-only home files render plain.
- **Drag-drop & Persist** — dropping OS files/folders onto the window adds them to the live layer (drop highlight via `.fb-dragging`); a **Persist** button (`.fb-persist`) appears in the topbar whenever the live layer is dirty *and* not viewing a snapshot. Drag-drop uses `getCurrentWebview().onDragDropEvent` from `@tauri-apps/api/webview` (enabled by default, no capability needed) and the `drop` payload carries real OS absolute paths.
- **History / time-machine** — a `.fb-history` `<select>` in the topbar (shown once ≥1 snapshot exists) lists "Now" plus each snapshot (`#N — date`). Selecting a snapshot enters a read-only view: an amber `.fb-snapshot-banner` appears with an **Exit** button, the Persist button and drag-drop are suppressed, and `list_dir`/`read_file`/`stream_file` invokes pass the `snapshot` number. The active snapshot is mirrored in `snapshotRef` because the drag-drop effect registers once and would otherwise close over a stale value. `switchView` closes any open preview (revoking blob URLs) and falls back to root if the current dir doesn't exist in the selected view.
- Topbar shows a clickable breadcrumb trail — each path segment navigates directly to that directory; the current segment is non-interactive.
- Clicking a file tile opens a slide-in preview panel (`PreviewPanel` in `App.tsx`) on the right. Text shows inline (up to 64 KB via `read_file`); images and PDFs stream as blob URLs via the `stream_file` command; other binaries show a "no preview" message. The panel is resizable via a drag handle and sits side-by-side with the grid inside `.fb-content`.
- Window uses the platform's standard title bar; no platform-specific native window customization, keeping the app cross-platform.

## When adding a new Rust command

1. Define `#[tauri::command] fn foo(...) -> Result<T, String>` in `src-tauri/src/lib.rs` (return types must be `Serialize`; map errors to `String` with `.map_err(|e| e.to_string())`). To serve files through the VFS, take `vfs: tauri::State<'_, VfsState>` and grab the overlay with `vfs.inner.lock().unwrap().root.clone()`.
2. Register it in `generate_handler![list_dir, read_file, stream_file, add_dropped_files, persist, list_snapshots, foo]`.
3. If it uses a Tauri plugin API, grant the capability in `src-tauri/capabilities/default.json`. Plain `std::fs` / `std::env` needs no capability.
