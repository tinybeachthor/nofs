# nofs

A minimal desktop file browser built with Tauri 2, React 19, and TypeScript.

The Rust backend owns all filesystem access and exposes a single `list_dir` command. The frontend renders a macOS-style dark tile grid with SVG folder/file icons, a clickable breadcrumb topbar for quick navigation, and lets you descend into folders by clicking tiles.

## Develop

```sh
bun install
bun run tauri dev
```

## Build

```sh
bun run tauri build
```
