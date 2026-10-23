#!/bin/bash
# Checks the ext4 root partition of a disk image after OSyx has run, with
# e2fsprogs (independent of OSyx). Read-only: e2fsck -n, debugfs without -w.
#   check-root.sh <disk image> <byte offset of the root partition>
# Prints key=value lines for scripts/test.ps1.
IMG="$1?offset=$2"
e2fsck -fn "$IMG" >/tmp/osyx-e2fsck.txt 2>&1
echo "e2fsck=$?"
echo "e2fsck_summary=$(tail -1 /tmp/osyx-e2fsck.txt)"
echo "hello=$(debugfs -R 'cat /osyx-out/hello-from-osyx.txt' "$IMG" 2>/dev/null)"
echo "worker=$(debugfs -R 'cat /osyx-out/from-worker.txt' "$IMG" 2>/dev/null)"
echo "pattern_sha256=$(debugfs -R 'cat /osyx-out/pattern.bin' "$IMG" 2>/dev/null | sha256sum | cut -d' ' -f1)"
echo "expected_sha256=$(python3 -c "import sys; sys.stdout.buffer.write(bytes((i*31+5)%256 for i in range(200000)))" | sha256sum | cut -d' ' -f1)"
echo "deep=$(debugfs -R 'cat /osyx-out/moved/c/deep.txt' "$IMG" 2>/dev/null)"
echo "link=$(debugfs -R 'stat /osyx-out/deep-link' "$IMG" 2>/dev/null | grep -o 'Fast link dest: .*')"
echo "private_mode=$(debugfs -R 'stat /osyx-out/private.txt' "$IMG" 2>/dev/null | grep -o 'Mode: *[0-7]*' | tr -s ' ')"
echo "temp_gone=$(debugfs -R 'stat /osyx-out/temp.txt' "$IMG" 2>&1 | grep -c 'File not found')"
