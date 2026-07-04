# nofs

A minimal, cross-platform file-backup desktop app built with Tauri 2, React 19, and TypeScript.

The Rust backend owns all filesystem access and serves everything through a layered virtual filesystem: a live in-memory layer stacked over frozen snapshot layers, over your read-only home directory. **Drag and drop** files or whole folders onto the window to import them into the live layer (directory structure preserved, symlinks skipped), then hit **Persist** to freeze them into a numbered `~/.nofs/{N}.tar.gz` snapshot. Because all history lives in that single `~/.nofs` store, backing up or syncing is just copying one directory between machines.

The **History** picker lets you time-travel: select a past snapshot to browse the whole tree exactly as it was backed up at that point (read-only). The frontend renders a macOS-style dark tile grid with SVG folder/file icons, a clickable breadcrumb topbar, and a slide-in preview panel that shows file content when you click a file tile. Files served by your backup layers are marked with an accent badge.

## Develop

```sh
bun install
bun run tauri dev
```

## Build

```sh
bun run tauri build
```
