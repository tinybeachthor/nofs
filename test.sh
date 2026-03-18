#!/usr/bin/env bash
set -euo pipefail

SOURCE=$(mktemp -d)
MOUNT=$(mktemp -d)
NOFS_PID=

cleanup() {
    fusermount -u "$MOUNT" 2>/dev/null || fusermount3 -u "$MOUNT" 2>/dev/null || true
    [[ -n "$NOFS_PID" ]] && wait "$NOFS_PID" 2>/dev/null || true
    rm -rf "$SOURCE" "$MOUNT"
}
trap cleanup EXIT

fail() { echo "FAIL: $1" >&2; exit 1; }

# Seed source directory
echo "hello world" > "$SOURCE/test.txt"
mkdir "$SOURCE/subdir"
echo "nested" > "$SOURCE/subdir/nested.txt"

# Mount
cargo build --release 2>&1
./target/release/nofs --no-ui "$SOURCE" "$MOUNT" &
NOFS_PID=$!

# Wait for mount to be ready
for i in $(seq 1 10); do
    if mountpoint -q "$MOUNT" 2>/dev/null; then
        break
    fi
    sleep 0.5
done
mountpoint -q "$MOUNT" || fail "mount not ready after 5s"

# Test: list files
[[ -f "$MOUNT/test.txt" ]] || fail "test.txt not visible in mount"
echo "PASS: file visible"

# Test: read file
[[ "$(cat "$MOUNT/test.txt")" == "hello world" ]] || fail "read content mismatch"
echo "PASS: read file"

# Test: read nested file
[[ "$(cat "$MOUNT/subdir/nested.txt")" == "nested" ]] || fail "nested read mismatch"
echo "PASS: read nested file"

# Test: write file
echo "new content" > "$MOUNT/write-test.txt"
[[ "$(cat "$SOURCE/write-test.txt")" == "new content" ]] || fail "write not passthrough"
echo "PASS: write file"

# Test: append
echo "appended" >> "$MOUNT/write-test.txt"
grep -q "appended" "$SOURCE/write-test.txt" || fail "append failed"
echo "PASS: append file"

# Test: mkdir
mkdir "$MOUNT/newdir"
[[ -d "$SOURCE/newdir" ]] || fail "mkdir not passthrough"
echo "PASS: mkdir"

# Test: remove file
rm "$MOUNT/write-test.txt"
[[ ! -f "$SOURCE/write-test.txt" ]] || fail "unlink not passthrough"
echo "PASS: unlink"

# Test: rmdir
rmdir "$MOUNT/newdir"
[[ ! -d "$SOURCE/newdir" ]] || fail "rmdir not passthrough"
echo "PASS: rmdir"

# Test: rename
echo "moveme" > "$MOUNT/before.txt"
mv "$MOUNT/before.txt" "$MOUNT/after.txt"
[[ ! -f "$SOURCE/before.txt" && "$(cat "$SOURCE/after.txt")" == "moveme" ]] || fail "rename failed"
echo "PASS: rename"

# Test: symlink
ln -s "$MOUNT/test.txt" "$MOUNT/link.txt"
[[ "$(cat "$MOUNT/link.txt")" == "hello world" ]] || fail "symlink read failed"
echo "PASS: symlink"

# Test: stat
[[ "$(stat -c %s "$MOUNT/test.txt")" == "$(stat -c %s "$SOURCE/test.txt")" ]] || fail "stat size mismatch"
echo "PASS: stat"

echo ""
echo "All tests passed."
