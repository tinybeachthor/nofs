#!/usr/bin/env bash
set -euo pipefail

DATA=$(mktemp -d)
MOUNT=$(mktemp -d)
NOFS_PID=

cleanup() {
    fusermount -u "$MOUNT" 2>/dev/null || fusermount3 -u "$MOUNT" 2>/dev/null || true
    [[ -n "$NOFS_PID" ]] && wait "$NOFS_PID" 2>/dev/null || true
    rm -rf "$DATA" "$MOUNT"
}
trap cleanup EXIT

fail() { echo "FAIL: $1" >&2; exit 1; }

start_mount() {
    ./target/release/nofs --no-ui "$DATA" "$MOUNT" &
    NOFS_PID=$!
    for i in $(seq 1 10); do
        if mountpoint -q "$MOUNT" 2>/dev/null; then
            break
        fi
        sleep 0.5
    done
    mountpoint -q "$MOUNT" || fail "mount not ready after 5s"
}

stop_mount() {
    fusermount -u "$MOUNT" 2>/dev/null || fusermount3 -u "$MOUNT" 2>/dev/null || true
    wait "$NOFS_PID" 2>/dev/null || true
    NOFS_PID=
}

# Build
cargo build --release 2>&1

# First mount
start_mount

# Test: write and read file
echo "hello world" > "$MOUNT/test.txt"
[[ "$(cat "$MOUNT/test.txt")" == "hello world" ]] || fail "write/read mismatch"
echo "PASS: write and read file"

# Test: write and read nested file
mkdir "$MOUNT/subdir"
echo "nested" > "$MOUNT/subdir/nested.txt"
[[ "$(cat "$MOUNT/subdir/nested.txt")" == "nested" ]] || fail "nested write/read mismatch"
echo "PASS: write and read nested file"

# Test: append
echo "appended" >> "$MOUNT/test.txt"
grep -q "appended" "$MOUNT/test.txt" || fail "append failed"
echo "PASS: append file"

# Test: mkdir
mkdir "$MOUNT/newdir"
[[ -d "$MOUNT/newdir" ]] || fail "mkdir failed"
echo "PASS: mkdir"

# Test: remove file
echo "deleteme" > "$MOUNT/todelete.txt"
rm "$MOUNT/todelete.txt"
[[ ! -f "$MOUNT/todelete.txt" ]] || fail "unlink failed"
echo "PASS: unlink"

# Test: rmdir
rmdir "$MOUNT/newdir"
[[ ! -d "$MOUNT/newdir" ]] || fail "rmdir failed"
echo "PASS: rmdir"

# Test: rename
echo "moveme" > "$MOUNT/before.txt"
mv "$MOUNT/before.txt" "$MOUNT/after.txt"
[[ ! -f "$MOUNT/before.txt" ]] || fail "rename: old still exists"
[[ "$(cat "$MOUNT/after.txt")" == "moveme" ]] || fail "rename: content mismatch"
echo "PASS: rename"

# Test: symlink
ln -s "test.txt" "$MOUNT/link.txt"
[[ "$(readlink "$MOUNT/link.txt")" == "test.txt" ]] || fail "symlink target mismatch"
echo "PASS: symlink"

# Test: stat
SIZE=$(stat -c %s "$MOUNT/test.txt")
[[ "$SIZE" -gt 0 ]] || fail "stat size is 0"
echo "PASS: stat"

# Test: persistence (unmount, remount, verify)
stop_mount
start_mount
grep -q "hello world" "$MOUNT/test.txt" || fail "persistence: test.txt content lost"
grep -q "appended" "$MOUNT/test.txt" || fail "persistence: test.txt append lost"
[[ "$(cat "$MOUNT/subdir/nested.txt")" == "nested" ]] || fail "persistence: nested.txt lost"
[[ "$(cat "$MOUNT/after.txt")" == "moveme" ]] || fail "persistence: after.txt lost"
echo "PASS: persistence"

echo ""
echo "All tests passed."
