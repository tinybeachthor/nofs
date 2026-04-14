# nofs

A minimal desktop file browser built with Tauri 2, React 19, and TypeScript.

The Rust backend owns all filesystem access and exposes a single `list_dir` command; the frontend is a pure view that renders the current path in a topbar, lists entries, and lets you descend into folders or navigate up one level.

## Develop

```sh
bun install
bun run tauri dev
```

## Build

```sh
bun run tauri build
```
