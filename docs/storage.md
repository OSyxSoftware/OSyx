# Storage: PCI, virtio-blk, block layer, FAT32, VFS

```
ring 3   os_std::fs (File, OpenOptions, read_dir, ...) ── os-sys ── ABI v4 syscalls 16-28
ring 0   handle table per process (process.rs)
         vfs.rs      mount table, path resolution, OpenFile (KMutex per handle)
         osyx-fat    FAT32 driver (storage/fat, no_std, host-tested)  (KMutex per volume)
         block.rs    disk registry vda, vdb, ...; range checks, chunking
         drivers/virtio_blk.rs ── drivers/virtio.rs (modern virtio-pci, split virtqueue)
         pci.rs      config space via 0xCF8/0xCFC, bus scan, BARs, capabilities
```

## PCI (`kernel/src/pci.rs`)

- Configuration mechanism #1 (ports `0xCF8`/`0xCFC`), first 256 bytes of each
  function; no ECAM/MCFG yet.
- Recursive scan from bus 0 through PCI-to-PCI bridges; multi-function
  devices; each function is reported once.
- BAR decoding with size probing (memory decoding is switched off while a BAR
  is probed), 32- and 64-bit memory BARs, I/O BARs.
- Capability list walk bounded against loops and checked to stay at or above
  offset `0x40`.

## Virtio transport and virtio-blk (`kernel/src/drivers`)

- Only the modern (virtio 1.x) PCI transport: vendor capabilities
  (`cfg_type` common / notify / ISR / device) locate register blocks inside
  BARs, which are mapped uncached into the kernel MMIO window
  (`0xFFFF_E000_0000_0000`, see [memory.md](memory.md)), so BARs above 4 GiB
  work. QEMU's transitional devices (`1af4:1001`) expose this interface.
- Feature negotiation requires `VIRTIO_F_VERSION_1`; virtio-blk additionally
  accepts read-only (`VIRTIO_BLK_F_RO`) and flush (`VIRTIO_BLK_F_FLUSH`).
- One split virtqueue per disk, rings in one 4 KiB frame, at most 64 entries.
  Requests are polled (device interrupts are suppressed); the polling loop
  yields the CPU, so other threads run while a request is in flight. A
  request that does not complete in time marks the disk failed (the device
  may still own the buffers, so they are never reused).
- Data goes through DMA bounce pages owned by the driver (64 KiB = 128 sectors
  per request); callers pass ordinary kernel buffers.
- No interrupts, no MSI-X, one request in flight per disk.

## Block layer (`kernel/src/block.rs`)

Registry of disks named `vda`, `vdb`, ... in PCI order. `block::read` /
`block::write` check that the range lies on the disk and that the buffer is a
whole number of sectors, refuse writes to read-only disks and split
transfers into the driver's request size. `Disk` adapts a registered disk to
the FAT driver's `BlockDevice` trait.

## FAT32 driver (`storage/fat`, crate `osyx-fat`)

`no_std` + `alloc`, written from the Microsoft FAT specification, tested on
the host (unit tests on in-memory volumes, and both directions against
mtools: images made by `mformat`/`mcopy` are read by OSyx, images written by
OSyx are read by `mdir`/`mtype`).

- BPB validation, FSInfo (treated as advisory, rewritten on flush), all FAT
  copies kept in sync, 8-sector write-back FAT cache.
- Long file names (UCS-2, checksums, sequence numbers), NT lower-case flags,
  case-insensitive lookup, `~N` short-name aliases.
- Files: read, write with zero-filled gaps, truncate/extend, clusters
  trimmed after a failed write. Directories: create (with `.`/`..`), list,
  remove when empty. Rename within a volume, replacing an existing file,
  directory moves update `..`, moving a directory into itself is refused.
- `format` (32 reserved sectors, 2 FATs, backup boot sector at 6) and a
  consistency `check` (cross-links, loops, chain length vs. size, lost
  clusters, FSInfo accuracy).
- Not implemented: FAT12/16, exFAT, real timestamps (always 1980-01-01),
  changing attributes, journaling. A damaged volume is reported, not
  repaired.

Host tools: `cargo run -p osyx-fat --example mkfat32 -- <image> <MiB> <label> [dir]`
creates an image from a host directory; `--example fatcheck -- <image>` runs
`check` on an image.

## VFS (`kernel/src/vfs.rs`)

- At boot every registered disk is probed; FAT32 volumes are mounted, the
  first writable one at `/`, others at `/vol1`, `/vol2`, ... The vvfat ESP
  disk QEMU synthesises is not FAT32 and is skipped.
- Paths are absolute UTF-8, normalised lexically (`.`, `..`, repeated `/`)
  before the longest matching mount point is chosen.
- An open file is a `FileRef` plus position and access mode; directories
  are opened read-only and listed with `readdir`.
- Locking: sleeping `KMutex`es (open file → volume), so a thread waiting for
  the disk does not spin and can be preempted. Every mutating operation ends
  with a flush of the FAT cache, FSInfo and the disk's write cache, so data a
  call reported as written survives QEMU exiting.
- Handles are per process (`3..`, up to 64), closed when the process exits
  (the kernel logs `closed N open handles`).

## Tests

- Kernel self-tests: PCI enumeration, block I/O (boot signature, transfers
  crossing the request size, range checks, rewrite of the backup boot
  sector, read-only enforcement), VFS operations including two kernel
  threads writing concurrently and a full volume check afterwards.
- `init` (ring 3): 12 `os_std::fs` checks, including reading files the build
  host put on the disk, 200 KB writes with seeks, append/`set_len`,
  directories and rename, error kinds, pointer and handle validation, the
  64-handle limit, 4 threads writing concurrently, a child process writing a
  file, and handles leaked at process exit.
- After QEMU exits, `test.ps1` reads the files OSyx wrote with mtools and
  runs `fatcheck` on the image.
- Boots without the data disk must skip the storage tests cleanly.

Not covered: AHCI/NVMe/IDE, interrupts-driven I/O, multiple requests in
flight, power-loss consistency beyond write-through, real hardware.
