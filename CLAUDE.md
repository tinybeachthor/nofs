# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **Maintenance:** Keep this file up to date. Whenever you make a change to the codebase — architecture, data structures, CLI flags, conventions, test coverage, or build process — update the relevant section of this file in the same commit.

## Build & Test

```bash
nix develop              # enter dev shell (Rust toolchain, FUSE 3, GUI libs)
cargo build --release    # build
./test.sh                # run all 11 integration tests (builds automatically)
```

Tests mount a real FUSE filesystem with `--no-ui`, run assertions, then clean up. There are no unit tests; all testing is integration-based via `test.sh`.

## Project Structure

```
src/
  main.rs    — CLI (clap), thread orchestration, GUI (NofsApp stub)
  fs.rs      — NofsFS struct implementing fuser::Filesystem (~1300 lines)
test.sh      — 11 integration tests (mount, assert, unmount)
flake.nix    — Nix dev environment (Rust + FUSE 3 + GUI libs)
Cargo.toml   — Dependencies
```

## Architecture

FUSE filesystem in Rust with content-addressed blob storage, Polars/AVRO metadata persistence, and an eframe/egui GUI window.

### Storage model

- Metadata (inodes + directory entries) stored in-memory as `HashMap`s, persisted to `metadata.avro` via Polars DataFrames
- File content stored as SHA-256-addressed blobs under `blobs/<hash[..2]>/<hash[2..]>`
- AVRO schema is denormalized: one row per directory entry, with full inode metadata joined in. Root inode stored with `parent_inode=0, name=""`
- Metadata flushes on `destroy()` and periodically (every 30s via `FLUSH_INTERVAL` if `dirty`)
- Blob writes are atomic: written to `.tmp` file then renamed

### Key data structures (`src/fs.rs`)

- `InodeMeta` — inode number, file type, permissions, uid/gid, atime/mtime/ctime, `blob_hash: Option<String>`, `symlink_target: Option<String>`, `nlink: u32`, `rdev: u32`
- `OpenFile` — per open-file-handle buffer: `inode`, `data: Vec<u8>`, `writable`, `dirty`
- `NofsFS` — main state:
  - `inode_meta: HashMap<u64, InodeMeta>`
  - `dir_entries: HashMap<(u64, String), u64>` — (parent_inode, name) → child_inode
  - `open_files: HashMap<u64, OpenFile>` — keyed by file handle
  - `lookup_cnt: HashMap<u64, u64>` — FUSE reference counts for cache invalidation
  - `next_inode: u64`, `next_fh: u64` — monotonic counters (persisted in metadata)
  - `dirty: bool`, `last_flush: Instant`

### Two-thread model (default mode)

- Main thread: `eframe::run_native()` — GUI event loop (shows a stub "nofs" label window)
- Spawned thread: `fuser::mount2()` — blocking FUSE mount serving all filesystem ops

**Lifecycle:** closing the GUI window calls `fusermount3 -u` to unmount; if the FUSE mount exits externally, the spawned thread calls `std::process::exit(0)`.

**`--no-ui` mode:** FUSE mount runs directly on the main thread (no GUI, no spawned thread). Used by `test.sh`.

### FUSE mount options

`FSName("nofs")`, `DefaultPermissions` (kernel enforces permissions), `NoAtime`, optionally `AllowOther`

### CLI flags

| Flag | Purpose |
|------|---------|
| `<data_dir>` | Directory for `metadata.avro` + `blobs/` |
| `<mountpoint>` | Where to mount |
| `--allow-other` | Allow other users to access the mount |
| `--no-ui` | Disable GUI (used by tests) |
| `--debug` | Enable debug logging |
| `--debug-fuse` | Enable FUSE-level debug output |

## Key Implementation Details

### Blob storage

- `blob_path(hash)` → `<data_dir>/blobs/<hash[..2]>/<hash[2..]>`
- `write_blob(data)` — SHA-256 hash, atomic write (`.tmp` → rename), returns hash
- `read_blob(hash)` — reads bytes from blob file
- `delete_blob_if_unreferenced(hash)` — garbage-collects blob only if no inode references it
- Blobs are deduplicated: files with identical content share one physical blob
- `cdc_chunk_and_write(data)` — splits data using FastCDC (v2020) content-defined chunking (min=512KB, avg=1MB, max=2MB), writes each chunk as a blob, returns ordered hash list

### Open file lifecycle

1. `open()` / `create()` — loads blob into `OpenFile.data: Vec<u8>` buffer in RAM
2. `read()` / `write()` — operates on in-memory buffer; writes mark `dirty = true`
3. `flush()` — persists dirty buffer to blob store via CDC chunking, updates inode's `blob_hashes`
4. `release()` — flushes if dirty, removes handle from `open_files`

**Caution:** All open files are fully buffered in RAM. Large files can exhaust memory.

### Metadata persistence (AVRO)

- `load_metadata()` — reads `metadata.avro` on `init()`; bootstraps root inode if file absent
- `flush_metadata()` — serializes `inode_meta` + `dir_entries` to Polars DataFrame → AvroWriter
- Called from: `destroy()` (unmount), periodic check in `getattr()` every 30s if dirty

### Hardlinks & nlink

`nlink` is tracked per inode. `unlink()` decrements nlink; blob and inode are deleted only when `nlink == 0`.

### Directory lookup

`parent_of(ino)` does a linear O(n) scan over all `dir_entries` to find the parent. This is a known inefficiency.

## Testing

`test.sh` covers:
1. Write and read a file
2. Write and read a nested file
3. Append to a file
4. `mkdir`
5. `unlink` (delete file)
6. `rmdir`
7. `rename`
8. Symlink creation
9. `stat` (file size)
10. Persistence (unmount → remount → verify data survives)
11. (additional test in suite)

Pattern: build binary → mount with `--no-ui` → wait up to 5s for mount → run bash assertions → unmount → cleanup via `trap EXIT`.

## Conventions

- Error returns use libc error codes (`ENOENT`, `EIO`, `EINVAL`, `ENOTDIR`, `ENOTEMPTY`, etc.)
- Timestamps: `now_timespec()` returns `(secs, nsecs)` as `(i64, i32)`; stored as `TimeSpec`
- File type stored as `u8` in AVRO: `0=regular, 1=dir, 2=symlink, 3=block, 4=char, 5=fifo, 6=socket`
- `TTL: Duration = 1s` — FUSE entry/attribute cache timeout (aggressive invalidation)
- `FUSE_ROOT_INODE: u64 = 1`
