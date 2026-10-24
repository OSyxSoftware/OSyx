#!/bin/bash
# Builds the ext4 root file system image of OSyx with e2fsprogs.
#   mkrootfs.sh <program dir> <output image> <size MiB>
# <program dir> holds bin/, usr/bin/ and system/bin/ (any may be empty): the
# user programs installed as /bin/*, /usr/bin/* and /system/bin/* (mode 0755).
# /system also holds the desktop's application entries; /etc the time zone
# and keyboard layout; /home/user a few documents to look at.
# Runs as root inside WSL so that files belong to root (uid 0) in the image.
# Only a staging directory under /tmp and the output image file are written.
set -euo pipefail
BIN="$1"; OUT="$2"; SIZE="$3"
STAGE="$(mktemp -d /tmp/osyx-rootfs.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT
R="$STAGE/root"
mkdir -p "$R"/{boot,dev,tmp,run,mnt,bin,usr/bin,etc/osyx,var/log,home/user,osyx-seed,system/bin,system/share/applications}
chmod 1777 "$R/tmp"
chmod 0755 "$R"/{boot,dev,run,mnt,bin,usr,usr/bin,etc,etc/osyx,var,var/log,home,osyx-seed,system,system/bin,system/share,system/share/applications}
printf 'OSyx 0.1.0\n' > "$R/etc/osyx-release"
printf 'osyx\n' > "$R/etc/hostname"
for d in bin usr/bin system/bin; do
    if [ -d "$BIN/$d" ]; then
        find "$BIN/$d" -maxdepth 1 -type f -exec install -m 0755 -t "$R/$d/" {} +
    fi
done
# Desktop wallpapers (OSXI images converted by build.ps1).
if [ -d "$BIN/opt/zen" ]; then
    mkdir -p "$R/opt/zen"
    cp -a "$BIN/opt/zen/." "$R/opt/zen/"
    find "$R/opt/zen" -type d -exec chmod 0755 {} +
    find "$R/opt/zen" -type f -exec chmod 0644 {} +
    chmod 0755 "$R/opt/zen/zen.exe"
fi
if [ -d "$BIN/opt/apps" ]; then
    mkdir -p "$R/opt/apps"
    find "$BIN/opt/apps" -maxdepth 1 -type f -exec install -m 0755 -t "$R/opt/apps/" {} +
    chmod 0755 "$R/opt" "$R/opt/apps"
fi
# Windows programs (PE32+) for /bin/winrun: test programs and applications.
if [ -d "$BIN/opt/wintests" ]; then
    mkdir -p "$R/opt/wintests"
    find "$BIN/opt/wintests" -maxdepth 1 -type f -exec install -m 0755 -t "$R/opt/wintests/" {} +
    chmod 0755 "$R/opt" "$R/opt/wintests"
fi
# Fonts for Windows programs (C:\Windows\Fonts) and the initial HKLM hive.
if [ -d "$BIN/Windows/Fonts" ]; then
    mkdir -p "$R/Windows/Fonts"
    find "$BIN/Windows/Fonts" -maxdepth 1 -type f -exec install -m 0644 -t "$R/Windows/Fonts/" {} +
    chmod 0755 "$R/Windows" "$R/Windows/Fonts"
fi
if [ -f "$BIN/etc/osyx/registry/machine.reg" ]; then
    mkdir -p "$R/etc/osyx/registry"
    install -m 0644 "$BIN/etc/osyx/registry/machine.reg" "$R/etc/osyx/registry/machine.reg"
    chmod 0755 "$R/etc/osyx/registry"
fi
mkdir -p "$R/system/share/wallpapers"
if [ -d "$BIN/system/share/wallpapers" ]; then
    find "$BIN/system/share/wallpapers" -maxdepth 1 -type f -exec install -m 0644 -t "$R/system/share/wallpapers/" {} +
fi
chmod 0755 "$R/system/share/wallpapers"
# Desktop session configuration: POSIX TZ rules (Europe/Warsaw) and the
# keyboard layout PRISM translates keys with (us or pl).
printf 'CET-1CEST,M3.5.0,M10.5.0/3\n' > "$R/etc/TZ"
printf 'us\n' > "$R/etc/osyx/keyboard"
# Applications the launcher offers (only those whose program is installed).
app() { # file name exec icon comment
    if [ -f "$R$3" ]; then
        printf 'Name=%s\nExec=%s\nIcon=%s\nComment=%s\n' "$2" "$3" "$4" "$5" > "$R/system/share/applications/$1.app"
    fi
}
app 10-terminal "AXIS Terminal" /system/bin/terminal terminal "Run commands in the AXIS shell"
app 20-files "Files" /system/bin/files files "Browse and manage files on every mounted volume"
app 30-sysinfo "System Information" /system/bin/sysinfo info "Version, processor, memory, volumes and processes"
app 40-taskmgr "Task Manager" /system/bin/taskmgr info "Processes, CPU, memory, disk and network performance"
app 50-settings "Settings" /system/bin/settings info "Configure appearance, keyboard, time and networking"
app 60-textedit "Text Editor" /opt/apps/textedit.exe info "Edit UTF-8 text with the Win32 application"
app 70-diskutil "Disk Utility" /opt/apps/diskutil.exe info "Mounted volumes and directory usage"
app 80-devmgr "Device Manager" /opt/apps/devmgr.exe info "Devices reported by OSyx"
app 90-netcenter "Network Center" /opt/apps/netcenter.exe info "Network adapters, traffic, ping and DNS"
app 99-filedialogs "File Dialog Tests" /opt/wintests/filedialogs.exe info "Interactive WinAPI dialog regression (test images only)"
if [ -f "$R/opt/zen/zen.exe" ] && [ -f "$R/system/bin/zen" ]; then
    app 95-zen "Zen Browser" /system/bin/zen zen "Browse the web with Zen (default browser)"
    printf '/system/bin/zen\n' > "$R/etc/osyx/default-browser"
    chmod 0644 "$R/etc/osyx/default-browser"
fi
chmod 0644 "$R"/etc/TZ "$R"/etc/osyx/keyboard
find "$R/system/share/applications" -type f -exec chmod 0644 {} +
# The home of the desktop user (uid 1000): a few documents to look at.
mkdir -p "$R/home/user/Documents" "$R/home/user/Scripts" "$R/home/user/Desktop"
for shortcut in 10-terminal 20-files 95-zen; do
    if [ -f "$R/system/share/applications/$shortcut.app" ]; then
        cp "$R/system/share/applications/$shortcut.app" "$R/home/user/Desktop/"
    fi
done
cat > "$R/home/user/Documents/Welcome.txt" <<'EOF'
Welcome to OSyx.

This desktop is drawn by PRISM, a compositor running as an ordinary
ring-3 process. Keyboard and mouse reach it through the kernel's own
xHCI USB host controller driver and USB HID drivers.

Try: open AXIS Terminal from the launcher (or press Super) and type
help, ls -l /, ps or print("Hello", 6 * 7).
EOF
cat > "$R/home/user/Scripts/hello.ax" <<'EOF'
# An AXIS script: run it with  axis Scripts/hello.ax
name = "OSyx"
answer = 6 * 7
print("Hello from", name, "- the answer is", answer)
uname
EOF
chmod 0644 "$R/home/user/Documents/Welcome.txt" "$R/home/user/Scripts/hello.ax"
# Contents init checks (userspace/apps/src/fs_checks.rs).
printf 'Hello from the OSyx root file system!\n' > "$R/osyx-seed/hello.txt"
python3 -c "import sys; sys.stdout.buffer.write(bytes((i*11+3)%253 for i in range(300000)))" > "$R/osyx-seed/pattern.bin"
ln -s hello.txt "$R/osyx-seed/link"
printf 'end' | dd of="$R/osyx-seed/sparse.bin" bs=1 seek=$((5*1024*1024*1024)) conv=notrunc status=none
printf 'root only\n' > "$R/osyx-seed/secret"
chmod 0600 "$R/osyx-seed/secret"
chmod 0644 "$R"/osyx-seed/{hello.txt,pattern.bin,sparse.bin}
chown -R 0:0 "$R"
chown -R 1000:1000 "$R/home/user"
chmod 0750 "$R/home/user"
rm -f "$OUT"
truncate -s "${SIZE}M" "$OUT.tmp"
mke2fs -q -F -t ext4 -L OSYX-ROOT -U 0b5e0b5e-0000-4000-8000-0000000005e7 -E root_owner=0:0 -d "$R" "$OUT.tmp"
e2fsck -fn "$OUT.tmp" >/dev/null
mv "$OUT.tmp" "$OUT"
echo "$OUT: ext4 ${SIZE} MiB, $(find "$R" | wc -l) entries"
