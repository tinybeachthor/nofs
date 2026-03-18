# nofs

A passthrough FUSE filesystem in Rust with a GUI window.

Mirrors a source directory to a mountpoint, passing all file operations through to the underlying filesystem. Shows an [eframe/egui](https://github.com/emilk/egui) window while mounted.

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
nofs <source> <mountpoint>
```

A GUI window will appear while the filesystem is mounted. Closing the window unmounts the filesystem.

### Options

| Flag             | Description                              |
|------------------|------------------------------------------|
| `--allow-other`  | Allow other users to access the mount    |
| `--no-ui`        | Disable the GUI window                   |
| `--debug`        | Enable debug logging                     |
| `--debug-fuse`   | Enable FUSE debug output                 |

### Example

```sh
mkdir /tmp/mount
nofs /home/user/documents /tmp/mount
# files in /home/user/documents are now accessible at /tmp/mount
```

To unmount:

```sh
fusermount3 -u /tmp/mount
```

## Testing

```sh
./test.sh
```
