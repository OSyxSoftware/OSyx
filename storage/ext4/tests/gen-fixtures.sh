#!/bin/bash
# Generates the ext2/3/4 test images in storage/ext4/tests/fixtures with
# e2fsprogs (mke2fs -d, debugfs), an implementation independent of OSyx.
# Only image files in the output directory are created; no device is touched.
#
# Run inside WSL (or any Linux) from the repository root:
#   bash storage/ext4/tests/gen-fixtures.sh
set -euo pipefail
OUT="$(cd "$(dirname "$0")" && pwd)/fixtures"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"
SERIAL=0

# Deterministic file contents shared with the Rust tests: byte i of a file
# with seed s is (i * 7 + s) % 251.
pattern() { python3 -c "import sys; n=int(sys.argv[1]); s=int(sys.argv[2]); sys.stdout.buffer.write(bytes((i*7+s)%251 for i in range(n)))" "$1" "$2"; }

tree() {
    local t="$1"
    mkdir -p "$t/dir/a/b/c/d/e/f/g/h/i/j" "$t/many" "$t/empty-dir"
    : > "$t/empty.txt"
    printf 'Hello from mke2fs!\n' > "$t/hello.txt"
    pattern 5000 1 > "$t/small.bin"
    pattern 3145728 2 > "$t/big.bin"
    printf 'deep\n' > "$t/dir/a/b/c/d/e/f/g/h/i/j/deep.txt"
    for i in $(seq 0 399); do printf 'file %d\n' "$i" > "$t/many/file-$i.txt"; done
    ln -s hello.txt "$t/fast-link"
    ln -s "dir/a/b/c/d/e/f/g/h/i/j/../../../../../../../../../../../$(printf 'x%.0s' $(seq 1 80))" "$t/slow-link"
    ln -s /does/not/exist "$t/dangling"
    printf 'zażółć gęślą jaźń\n' > "$t/zażółć.txt"
    ln "$t/hello.txt" "$t/hard-link.txt"
    # Sparse: 6 GiB with data at 0, 1 GiB and the last 4 KiB (needs large_file).
    printf 'start' > "$t/sparse.bin"
    pattern 4096 3 | dd of="$t/sparse.bin" bs=4096 seek=$((1024*256)) conv=notrunc status=none
    pattern 4096 4 | dd of="$t/sparse.bin" bs=4096 seek=$((6*1024*256 - 1)) conv=notrunc status=none
}

make() {
    local name="$1" size="$2"; shift 2
    local src="$WORK/src-$name"
    rm -rf "$src"; mkdir -p "$src"; tree "$src"
    rm -f "$OUT/$name.img"
    truncate -s "$size" "$OUT/$name.img"
    mke2fs -q -F -L "$name" -U "0b5e0b5e-0000-4000-8000-$(printf %012d $((++SERIAL)))" -E root_owner=0:0 -d "$src" "$@" "$OUT/$name.img"
    # Ownership and permissions that mke2fs -d copies from the build user
    # are set explicitly so the image is the same on every machine.
    debugfs -w -R "sif /hello.txt uid 1000" "$OUT/$name.img" >/dev/null 2>&1
    debugfs -w -R "sif /hello.txt gid 1000" "$OUT/$name.img" >/dev/null 2>&1
    debugfs -w -R "sif /small.bin mode 0100600" "$OUT/$name.img" >/dev/null 2>&1
    # Index large directories (dir_index) as a long-running system would.
    e2fsck -fyD "$OUT/$name.img" >/dev/null 2>&1 || true
    e2fsck -fn "$OUT/$name.img" >/dev/null
    echo "$name: $(dumpe2fs -h "$OUT/$name.img" 2>/dev/null | grep -E 'features' | head -1)"
}

make ext4-4k 24M -t ext4 -b 4096
make ext4-1k 24M -t ext4 -b 1024
make ext4-nocsum 24M -t ext4 -b 4096 -O ^metadata_csum,uninit_bg
make ext4-metabg 24M -t ext4 -b 1024 -O meta_bg,^resize_inode
make ext3-1k 24M -t ext3 -b 1024
make ext2-4k 24M -t ext2 -b 4096

# A volume with a committed but unreplayed journal transaction that
# rewrites the data block of /hello.txt and revokes nothing (debugfs writes
# the journal; the file system itself is left untouched until recovery).
cp "$OUT/ext4-4k.img" "$OUT/ext4-journal.img"
B=$(debugfs -R "bmap /hello.txt 0" "$OUT/ext4-journal.img" 2>/dev/null)
python3 -c "import sys; sys.stdout.buffer.write(b'Replayed by the journal!\n'.ljust(4096, b'\0'))" > "$WORK/blk.bin"
printf 'jo\njw -b %s %s\njc\n' "$B" "$WORK/blk.bin" | debugfs -w -f - "$OUT/ext4-journal.img" >/dev/null 2>&1
echo "ext4-journal: hello.txt block $B; $(dumpe2fs -h "$OUT/ext4-journal.img" 2>/dev/null | grep -E 'features|Journal start' | tr '\n' ' ')"

# Features the driver must refuse rather than misread.
for f in inline_data encrypt casefold; do
    rm -f "$OUT/ext4-$f.img"; truncate -s 8M "$OUT/ext4-$f.img"
    mke2fs -q -F -t ext4 -O "$f" "$OUT/ext4-$f.img"
done
ls -l "$OUT"
