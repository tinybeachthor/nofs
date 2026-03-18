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

Passthrough FUSE filesystem in Rust with an eframe/egui GUI window.

**Two-thread model (default):**
- Main thread: `eframe::run_native()` — GUI event loop showing a window
- Spawned thread: `fuser::mount2()` — blocking FUSE mount serving filesystem ops

**Lifecycle:** closing the window runs `fusermount3 -u` to unmount; if the FUSE mount is unmounted externally, `std::process::exit(0)` terminates the process.

**`--no-ui` mode:** FUSE mount runs directly on the main thread (no GUI, no spawned thread). Used by `test.sh`.

**Key files:**
- `src/main.rs` — CLI (clap), thread orchestration, GUI (`NofsApp` struct)
- `src/fs.rs` — `fuser::Filesystem` implementation (~770 lines). Maintains inode↔path maps, fd tracking, and delegates all ops to the underlying filesystem via unsafe libc calls
