#!/bin/bash
# Builds the ext4 root file system image of OSyx with e2fsprogs.
#   mkrootfs.sh <program dir> <output image> <size MiB>
# <program dir> holds bin/ and usr/bin/ (either may be empty): the user
# programs installed as /bin/* and /usr/bin/* (mode 0755).
# Runs as root inside WSL so that files belong to root (uid 0) in the image.
# Only a staging directory under /tmp and the output image file are written.
set -euo pipefail
BIN="$1"; OUT="$2"; SIZE="$3"
STAGE="$(mktemp -d /tmp/osyx-rootfs.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT
R="$STAGE/root"
mkdir -p "$R"/{boot,dev,tmp,run,mnt,bin,usr/bin,etc,var/log,home/user,osyx-seed}
chmod 1777 "$R/tmp"
chmod 0755 "$R"/{boot,dev,run,mnt,bin,usr,usr/bin,etc,var,var/log,home,osyx-seed}
printf 'OSyx 0.1.0\n' > "$R/etc/osyx-release"
printf 'osyx\n' > "$R/etc/hostname"
for d in bin usr/bin; do
    find "$BIN/$d" -maxdepth 1 -type f -exec install -m 0755 -t "$R/$d/" {} +
done
# Contents init checks (userspace/apps/src/fs_checks.rs).
printf 'Hello from the OSyx root file system!\n' > "$R/osyx-seed/hello.txt"
python3 -c "import sys; sys.stdout.buffer.write(bytes((i*11+3)%253 for i in range(300000)))" > "$R/osyx-seed/pattern.bin"
ln -s hello.txt "$R/osyx-seed/link"
printf 'end' | dd of="$R/osyx-seed/sparse.bin" bs=1 seek=$((5*1024*1024*1024)) conv=notrunc status=none
printf 'root only\n' > "$R/osyx-seed/secret"
chmod 0600 "$R/osyx-seed/secret"
chmod 0644 "$R"/osyx-seed/{hello.txt,pattern.bin,sparse.bin}
chown -R 0:0 "$R"
chown 1000:1000 "$R/home/user"
chmod 0750 "$R/home/user"
rm -f "$OUT"
truncate -s "${SIZE}M" "$OUT.tmp"
mke2fs -q -F -t ext4 -L OSYX-ROOT -U 0b5e0b5e-0000-4000-8000-0000000005e7 -E root_owner=0:0 -d "$R" "$OUT.tmp"
e2fsck -fn "$OUT.tmp" >/dev/null
mv "$OUT.tmp" "$OUT"
echo "$OUT: ext4 ${SIZE} MiB, $(find "$R" | wc -l) entries"
