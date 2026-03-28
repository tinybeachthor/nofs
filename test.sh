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

# Build (skip if binary already present, e.g. pre-built by nix in CI)
[[ -x ./target/release/nofs ]] || cargo build --release 2>&1

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

# Test: Reed-Solomon recovery from corrupt blob shards
echo "rs recovery" > "$MOUNT/rs_test.txt"
stop_mount

RS_CONTENT="rs recovery"
RS_HASH=$(printf '%s\n' "$RS_CONTENT" | sha256sum | awk '{print $1}')
RS_BLOB="$DATA/blobs/${RS_HASH:0:2}/${RS_HASH:2}"
[[ -f "$RS_BLOB" ]] || fail "RS recovery: blob not found at $RS_BLOB"

python3 - "$RS_BLOB" <<'PYEOF'
import struct, sys
data = open(sys.argv[1], 'rb').read()
_, shard_size = struct.unpack_from('<QQ', data, 0)
shard_size = int(shard_size)
result = bytearray(data)
# Corrupt the first byte of 4 data shards — one more than any single parity shard can cover
for i in [0, 2, 4, 6]:
    data_offset = 16 + i * (4 + shard_size) + 4
    result[data_offset] ^= 0xFF
open(sys.argv[1], 'wb').write(result)
PYEOF

start_mount
[[ "$(cat "$MOUNT/rs_test.txt")" == "rs recovery" ]] || fail "RS recovery: content mismatch after 4 shards corrupted"
echo "PASS: Reed-Solomon recovery from 4 corrupt shards"

echo ""
echo "All tests passed."
