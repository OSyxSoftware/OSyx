# Storage and file systems

## Layout

| Mount point | File system | Source | Notes |
|---|---|---|---|
| `/` | ext4 | GPT partition "Linux root (x86-64)" (`root=<dev>` overrides) | read-write, journalled; `ro` on the command line mounts it read-only |
| `/boot` | FAT32 | the EFI system partition | read-write for root, case-insensitive |
| `/tmp` | tmpfs | RAM, 64 MiB, mode 1777 | independent instance |
| `/run` | tmpfs | RAM, 8 MiB, mode 0755 | independent instance |
| `/dev` | devfs | device registry | read-only view |
| `/mnt/<dev>` | FAT32 / ext4 | other recognised volumes | |

Without a usable root partition a 16 MiB tmpfs becomes `/` (with the mount
point directories created), and the log says so. Root can also mount and
unmount volumes at run time (`mount` / `umount`).

The boot disk `build/osyx.img` is a GPT disk built by `scripts/build.ps1`:
the ESP (FAT32, `osyx-fat` `mkfat32`) and the root partition (ext4,
`mke2fs -d` in WSL via `scripts/mkrootfs.sh`), assembled by
`osyx-part`'s `mkdisk`. OVMF boots from the ESP; the kernel finds both
partitions by their GPT type GUIDs.

```
ring 3     os_std::fs, env::current_dir, os::mount ── os-sys ── ABI v6 (open … statfs)
ring 0     process: handle table, credentials (uid/gid), current directory
           vfs/mod.rs   resolution, permissions, mount table, open files
           ├─ vfs/ext4.rs   ── osyx-ext4  (storage/ext4)
           ├─ vfs/fat.rs    ── osyx-fat   (storage/fat)
           ├─ vfs/tmpfs.rs  ── osyx-tmpfs (storage/tmpfs)
           └─ vfs/devfs.rs  ── devices.rs (registry, /dev/null, zero, random, console, disks)
           block.rs     disks vda…, GPT partitions vda1… (osyx-part)
           drivers/     virtio-blk, virtio-rng (modern virtio-pci, polled)
```

Host-testable building blocks live in no_std crates: `osyx-vfs` (path
syntax, permission checks, mount table), `osyx-tmpfs`, `osyx-ext4`,
`osyx-fat`, `osyx-part`, `osyx-crypto`.

## VFS (`kernel/src/vfs`)

- **Driver interface** (`Filesystem`): inode-style — `lookup(dir, name)`,
  `stat`, `read_dir`, `read`/`write`/`truncate` by node number, `create`
  (file, directory, symlink), `unlink`, `rmdir`, `rename`, `set_mode`,
  `readlink`, `open`/`release` notifications, `device`, `sync`, `usage`,
  `unmount`, `fsck`. New file systems implement it and are added to
  `vfs::mount_disk` (disk-backed) or mounted directly; the VFS itself does
  not change.
- **Resolution**: one component at a time from `/` or the caller's current
  directory. A component whose canonical path is a mount point continues in
  the mounted root (longest-prefix match). Symbolic links are expanded in
  place (40 at most, then `Loop`); `..` returns to the previous directory on
  the walk, so it is "physical", crosses mount points correctly and stops at
  `/`.
- **Permissions**: search permission on every directory walked; read/write
  on the final node; write + search on directories whose entries change;
  sticky directories (`/tmp`) only let owners remove or rename; only owners
  and root `chmod`; only root mounts. uid 0 bypasses read/write checks.
  Processes carry `uid/gid` (inherited, root may `setuid` once to drop) and
  a current directory (inherited).
- **Open files and handles**: per-process tables (3…66, 64 at most) of
  shared open files; directory handles list with `readdir`. An open file
  keeps its mount alive (unmount → `Busy`) and its node alive in the driver
  (removed-while-open files keep their data until the last close on tmpfs
  and ext4; FAT refuses with `Busy`).
- **Locking**: one sleeping mutex per volume, one per open file (order open
  file → volume → disk); the mount table is a spinlock never held while a
  driver runs.

## ext4 (`storage/ext4`, crate `osyx-ext4`)

Written from the on-disk format description; tested against images made by
e2fsprogs 1.47 (`storage/ext4/tests/gen-fixtures.sh`).

Reading:
- block sizes 1–64 KiB, 32/64-byte descriptors (`64bit`), `flex_bg`,
  `meta_bg`, sparse superblock backups;
- extents (any depth) and ext2/ext3 direct/indirect block maps; unwritten
  extents and holes read as zeros; files beyond 4 GiB;
- linear and hashed (`dir_index`) directories (read linearly);
- fast and slow symbolic links, uid/gid/mode, nanosecond times;
- `metadata_csum` (crc32c) and `gdt_csum` (crc16) verified on every
  superblock, descriptor, inode, extent block and directory block read;
- every on-disk value is range-checked: corrupt metadata gives an error,
  never a panic (fuzzed).

Refused at mount (`Unsupported`): `inline_data`, `encrypt`, `casefold`,
`compression`, external journals, `mmp`, `dirdata`, `bigalloc`, unknown
incompatible features.

Writing (only volumes whose features the driver maintains; otherwise the
volume stays read-only and the reason is logged):
- allocation of blocks and inodes (bitmaps with checksums, `BLOCK_UNINIT` /
  `INODE_UNINIT` groups initialised on first use, `itable_unused`), free
  counts in descriptors and superblock kept exact;
- create files, directories, fast/slow symlinks; write (sparse), truncate
  (shrink frees blocks and zeroes the tail; grow leaves a hole); unlink,
  rmdir, rename (replacing files or empty directories, `..` and link counts
  updated, no moves into a subtree); chmod; extent trees of depth 0 or 1;
- **journalling**: every operation is one JBD2 transaction (descriptor,
  block copies, commit, checksum v2/v3 when the journal uses them), written,
  committed, checkpointed and the journal emptied before the call returns;
  file data is written before the metadata that points to it ("ordered");
  blocks freed in a transaction are not reused in it;
- **recovery**: a mount with `needs_recovery` replays committed
  transactions (read-only mounts into memory only); files removed while
  open go on the superblock orphan list and are freed at the last close or
  at the next writable mount;
- refused writes: block-mapped (ext2/ext3) files, `meta_bg`, quota/project,
  `ea_inode`, fast commits, not-cleanly-unmounted volumes without a journal,
  adding entries to hashed directories (removing from them works), extent
  trees needing more than 4 leaves.

The root partition is mounted read-write; `vfs::shutdown` (used by
`test.exit`) leaves it clean (journal empty, `needs_recovery` cleared,
state clean), which `e2fsck -fn` confirms after every QEMU test run.

## FAT32, tmpfs, devfs

- FAT32 (`osyx-fat`): see the driver's own documentation in the crate; the
  VFS adapter assigns stable inode numbers by path, synthesises root-owned
  0755/0644 permissions (0444 with the read-only attribute) and the FAT
  timestamp, and writes metadata through after every change.
- tmpfs (`osyx-tmpfs`): 4 KiB pages allocated on first write (holes are
  free), a per-instance memory limit (nodes, names, pages are charged;
  `NoSpace` when exceeded, short writes when partly possible), memory
  returned on removal or last close of a removed file, case-sensitive.
- devfs: `null`, `zero`, `random` (only when seeded), `console`
  (write-only: no input driver yet), `disks/<dev>` (block devices, root-only
  0600, raw writes refused while a volume on the disk is mounted). Drivers
  register and unregister nodes at run time.

## /dev/random

`kernel/src/random.rs`: a ChaCha20 DRBG with fast key erasure
(`osyx-crypto`, checked against RFC 8439 and FIPS 180-4 vectors), seeded
with SHA-256 over 64 bytes from each hardware source: RDSEED/RDRAND (when
CPUID reports them) and virtio-rng. RDRAND output is mixed in on every
request, virtio-rng every 64 requests. Without a source the generator is
not seeded and `/dev/random` is not created; it never returns predictable
bytes. QEMU's `qemu64` CPU has no RDRAND; the test VM gets
`-device virtio-rng-pci`.

## Tests

- Host: `osyx-ext4` (read: 6 image variants, sparse 6 GiB files, refused
  features, journal replay; corruption and fuzz; write: three feature sets
  checked by e2fsck and debugfs, extent trees, ENOSPC, orphans, journal
  replay to disk, **power cut after every device write of a workload**,
  model-based random operations), `osyx-tmpfs` (limits, reclamation,
  rename semantics, model test), `osyx-vfs`, `osyx-part` (sgdisk interop,
  corruption), `osyx-crypto` (vectors), `os-std` fs/env.
- Kernel self-tests: tmpfs through the VFS, symlinks/cwd/permissions/
  case-insensitive lookup, mount/unmount/limits, devfs and `/dev/random`,
  FAT32 `/boot`, ext4 `/`, concurrent writers on all three.
- `init` (ring 3): 19 file system checks across all mounts, and a child
  that drops to uid 1000 and checks 12 permission rules.
- After QEMU exits, the host checks the ESP with mtools + `fatcheck` and
  the root with `e2fsck -fn` + `debugfs`, and runs a power-cut scenario
  (QEMU stopped with an open removed file, recovery at the next boot).

## Not implemented

NTFS (out of scope), ext4 inline data/encryption/case folding/xattrs,
htree insertion, delayed allocation and caching (every operation is
synchronous), hard-link creation, FIFOs/sockets/device nodes on disk,
`statx`-style birth times, `/dev/input` (no input driver), interrupt-driven
disk I/O.
