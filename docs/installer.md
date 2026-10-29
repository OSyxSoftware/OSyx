# OSyx Setup (installer)

## Pieces

| Part | Where | What it does |
|---|---|---|
| Installation engine | `userspace/install` (`osyx-install`, `no_std`, host tested) | Plans the layout, writes a GPT, FAT32 on the ESP, ext4 (with a JBD2 journal) on the root, copies the running system, writes `/etc/hostname`, `/etc/passwd`, `/etc/group`, `/etc/shadow` (salted iterated SHA-256), `/etc/osyx/firstboot.conf` and `/etc/osyx/installed`, installs the boot files last and verifies the result. |
| ext4 mkfs | `storage/ext4/src/format.rs` | New volumes: 4 KiB blocks, extents, journal, sparse superblock backups, `lost+found`; accepted by `e2fsck -fn`. |
| Graphical installer | `userspace/desktop/src/bin/setup.rs` (`/system/bin/setup`) | PRISM window with the steps Welcome, Disk, Account, Options, Summary, Install, Finish. Writes nothing until a separate dialog confirms that the named disk will be erased. |
| Live system | `scripts/build.ps1 -Live` | `build/osyx-live.iso` and `build/osyx-live-usb.img` (GPT + FAT32 ESP; the whole system is the compressed `rootfs.z` module and runs from RAM). Live roots contain the installer payload in `/system/share/osyx-setup/esp` and an "Install OSyx" entry. |
| First start | `userspace/web/src/bin/firstboot.rs` (`/bin/firstboot`) | Run by init beside the session while `/etc/osyx/firstboot.conf` exists; state in `/etc/osyx/firstboot.state`, log in `/var/log/osyx-firstboot.log`; failed tasks are retried on the next start. |
| 7z reader | `storage/sevenz` (`osyx-7z`) | LZMA, BCJ2, BCJ, Copy; LZMA-encoded headers; CRC32 per file. Host test: the official Zen installer unpacks byte-identical to libarchive (66 files, 450 144 499 bytes). |
| HTTPS | `userspace/web` (`osyx-web`, `/bin/fetch`) | HTTP/1.1 GET with redirects, chunked and sized bodies, over rustls (TLS 1.2/1.3, RustCrypto provider, Mozilla roots from `webpki-roots`, RTC clock, `/dev/random`). |

## Safety

- The disk OSyx runs from (`/` or `/boot`) cannot be chosen. A live boot
  mounts at `/boot` only the ESP holding `\EFI\OSYX\BIN\rootfs.z`, not the
  internal disk's ESP; other volumes go to `/mnt/<device>`, and a disk
  whose volumes are only mounted there can be chosen: Setup unmounts them
  before writing. Disks under 770 MiB are not offered.
- Unattended mode (`setup --auto --disk X --confirm-erase X`) refuses to
  run unless both names match; the QEMU test uses it on an image file.
- The boot loader is copied last: an interrupted installation is not
  bootable, and a new attempt starts over on the same disk.
- Kernel command line options are separated by spaces, so `init.args`
  values must not contain spaces.

## Test

`scripts/install-test.ps1 [-Zen]` (QEMU, image files only):

1. boots `build/osyx-live-usb.img` as a USB stick with a blank 4 GiB virtio
   disk; Setup installs unattended and restarts;
2. checks the disk on the host: `sgdisk -v`, `e2fsck -fn`, `debugfs` (host
   name, account, desktop entries), `fatcheck`;
3. boots the installed disk alone: ext4 root on `vda2`, the desktop
   session, the first-boot service; with `-Zen` the download and install.

Last run (2026-10-28): 20 of 20 checks pass. In the installed system the
official Zen 1.23.4b installer (118 361 088 bytes) is downloaded over
verified HTTPS in about 11 s, matches GitHub's SHA-256 digest, its archive
is unpacked into `/opt/zen` (65 files, 428 MiB, CRC32 checked), Zen is
registered (launcher entry, desktop shortcut, default browser, registry
associations) and started from the AXIS launcher; its window appears.

## Known limits

- **The Zen installer's own code does not run.** `zen.installer.exe` is a
  32-bit (PE32) UPX-packed 7-Zip self-extractor with a 32-bit NSIS
  `setup.exe`; `winrun` runs only x64 programs. OSyx unpacks the verified
  archive natively instead, so the installer's registry entries,
  uninstaller and maintenance service are not created (OSyx writes the
  browser associations itself). Running 32-bit Windows programs (WoW64)
  is not implemented.
- There is no login screen; the password is stored in `/etc/shadow` but
  nothing asks for it. The desktop user's home stays `/home/user`.
- Real hardware: the installer works on whatever `/dev/disks` shows; NVMe,
  AHCI and USB storage drivers are recent and only tested in QEMU.
- `rustls-rustcrypto` is an alpha, unaudited provider.
