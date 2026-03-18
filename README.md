# nofs

A FUSE filesystem in Rust with content-addressed storage and a GUI window.

Stores file metadata in Polars DataFrames (persisted as AVRO) and file content as SHA-256-addressed blobs. The filesystem starts empty and persists all data to a configurable data directory. Shows an [eframe/egui](https://github.com/emilk/egui) window while mounted.

## Requirements

- Rust (via [Nix flake](flake.nix) or manually)
- FUSE 3 (`libfuse3-dev` on Debian/Ubuntu)

## Building

```sh
nix develop  # sets up Rust toolchain and system deps
cargo build --release
```

## Usage

```sh
nofs <data_dir> <mountpoint>
```

A GUI window will appear while the filesystem is mounted. Closing the window unmounts the filesystem.

### Data directory layout

```
<data_dir>/
  metadata.avro       -- inode metadata + directory entries
  blobs/
    ab/cdef1234...    -- content-addressed file blobs (SHA-256)
```

### Options

| Flag             | Description                              |
|------------------|------------------------------------------|
| `--allow-other`  | Allow other users to access the mount    |
| `--no-ui`        | Disable the GUI window                   |
| `--debug`        | Enable debug logging                     |
| `--debug-fuse`   | Enable FUSE debug output                 |

### Example

```sh
mkdir /tmp/data /tmp/mount
nofs /tmp/data /tmp/mount
# filesystem starts empty; files written to /tmp/mount persist in /tmp/data
```

To unmount:

```sh
fusermount3 -u /tmp/mount
```

Remounting the same data directory restores all files.

## Testing

```sh
./test.sh
```
