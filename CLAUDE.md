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
- **Frontend ↔ Rust bridge** — `invoke("cmd_name", args)` from `@tauri-apps/api/core` calls `#[tauri::command]` functions registered inside `tauri::Builder::default().invoke_handler(tauri::generate_handler![...])` in `src-tauri/src/lib.rs`. See the `list_dir` command for the canonical pattern (including `Result<T, String>` error conversion and `Serialize` return types).
- **Filesystem access lives entirely in Rust.** The frontend never touches the FS directly — no `tauri-plugin-fs`, no FS capabilities. Add new FS features as `#[tauri::command]` functions using `std::fs`; resolve well-known directories via `app.path()` (requires `use tauri::Manager;`).
- **Capabilities/permissions** — declared in `src-tauri/capabilities/`; required for any Tauri plugin/API surface exposed to the webview.
- **Config** — `src-tauri/tauri.conf.json` controls window, bundling, dev URL, identifier (`com.martin.nofs`).
- **Plugins enabled** — `tauri-plugin-opener`.

## UI

- Dark macOS-style design (`#1c1c1e` background, system fonts).
- Files and folders rendered as tiles in a responsive CSS grid (`repeat(auto-fill, minmax(120px, 1fr))`).
- SVG icons: `FolderIcon` (blue gradient folder) and `FileIcon` (grey document) defined inline in `App.tsx`.
- Topbar shows a clickable breadcrumb trail — each path segment navigates directly to that directory; the current segment is non-interactive.

## When adding a new Rust command

1. Define `#[tauri::command] fn foo(...) -> Result<T, String>` in `src-tauri/src/lib.rs` (return types must be `Serialize`; map errors to `String` with `.map_err(|e| e.to_string())`).
2. Register it in `generate_handler![list_dir, foo]`.
3. If it uses a Tauri plugin API, grant the capability in `src-tauri/capabilities/default.json`. Plain `std::fs` / `std::env` needs no capability.
