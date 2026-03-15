# nofs

A passthrough FUSE filesystem built with [pyfuse3](https://github.com/libfuse/pyfuse3) and [trio](https://trio.readthedocs.io/).

Mirrors a source directory to a mountpoint, passing all file operations through to the underlying filesystem.

## Requirements

- Python >= 3.13
- FUSE 3 (`libfuse3-dev` on Debian/Ubuntu)
- [Poetry](https://python-poetry.org/)

## Installation

```sh
poetry install
```

## Usage

```sh
nofs <source> <mountpoint>
```

### Options

| Flag             | Description                              |
|------------------|------------------------------------------|
| `--allow-other`  | Allow other users to access the mount    |
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
fusermount -u /tmp/mount
```

## Testing

```sh
./test.sh
```
