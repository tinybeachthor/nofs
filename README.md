# nofs

A minimal desktop file browser built with Tauri 2, React 19, and TypeScript.

The Rust backend owns all filesystem access via `list_dir` and `read_file` commands. The frontend renders a macOS-style dark tile grid with SVG folder/file icons, a clickable breadcrumb topbar for quick navigation, and a slide-in preview panel that shows file content when you click a file tile.

## Develop

```sh
bun install
bun run tauri dev
```

## Build

```sh
bun run tauri build
```
