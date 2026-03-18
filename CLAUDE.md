# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
nix develop              # enter dev shell (Rust toolchain, FUSE 3, GUI libs)
cargo build --release    # build
./test.sh                # run all 11 integration tests (builds automatically)
```

Tests mount a real FUSE filesystem with `--no-ui`, run assertions, then clean up. There are no unit tests; all testing is integration-based via `test.sh`.

## Architecture

FUSE filesystem in Rust with content-addressed blob storage, Polars/AVRO metadata persistence, and an eframe/egui GUI window.

**Storage model:**
- Metadata (inodes + directory entries) stored in-memory as `HashMap`s, persisted to `metadata.avro` via Polars DataFrames
- File content stored as SHA-256-addressed blobs under `blobs/<hash[..2]>/<hash[2..]>`
- AVRO is denormalized: one row per directory entry, with full inode metadata joined in. Root inode stored with `parent_inode=0`.
- Metadata flushes on `destroy()` and periodically (every 30s if dirty)

**Two-thread model (default):**
- Main thread: `eframe::run_native()` — GUI event loop showing a window
- Spawned thread: `fuser::mount2()` — blocking FUSE mount serving filesystem ops

**Lifecycle:** closing the window runs `fusermount3 -u` to unmount; if the FUSE mount is unmounted externally, `std::process::exit(0)` terminates the process.

**`--no-ui` mode:** FUSE mount runs directly on the main thread (no GUI, no spawned thread). Used by `test.sh`.

**Key files:**
- `src/main.rs` — CLI (clap), thread orchestration, GUI (`NofsApp` struct)
- `src/fs.rs` — `NofsFS` struct implementing `fuser::Filesystem`. In-memory inode metadata (`HashMap<u64, InodeMeta>`), directory entries (`HashMap<(u64, String), u64>`), open file buffers (`HashMap<u64, OpenFile>`), blob read/write helpers, and Polars AVRO serialization
