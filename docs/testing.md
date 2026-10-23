# Testing

## Commands

```powershell
.\scripts\check-env.ps1             # tool check, no changes to the system
.\scripts\build.ps1 [-Cmdline '…'] [-NoInit] [-KeepRoot] [-DebugBuild]
.\scripts\run-qemu.ps1 [-Cmdline '…'] [-Gdb] [-Follow] [-Headless]
.\scripts\test.ps1 [-SkipHost] [-SkipQemu]
```

QEMU runs `q35`, `qemu64,+nx`, 512 MiB, OVMF from the MSYS2 QEMU package
with fresh firmware variables each boot, the GPT disk `build/osyx.img` as
virtio-blk (`vda`: ESP `vda1` FAT32 64 MiB, root `vda2` ext4 128 MiB), a
virtio-rng device, COM1 to a log file and `isa-debug-exit` at port `0xf4`.
Logs and screenshots go to `build/test/`. `-KeepRoot` reuses the root
partition of the previous disk image (used after the power-cut boot).

Host tools: `osyx-fat` `mkfat32`/`fatcheck`, `osyx-part` `mkdisk`, mtools
from MSYS2, and e2fsprogs 1.47 / sgdisk in WSL Ubuntu 24.04 (`mke2fs -d`
for the root image and test fixtures, `e2fsck -fn`, `debugfs`, `dumpe2fs`;
partitions are addressed inside the image file with `?offset=`).

## Suite (`scripts/test.ps1`)

| Test | Checks |
|---|---|
| host unit tests | `cargo test` for every host crate: abi, bootinfo, gfx, elf, pe, mm, crypto (RFC 8439 / FIPS 180-4 vectors), vfs, tmpfs, part (sgdisk interop), fat (mtools both ways), ext4 (e2fsprogs images, corruption/fuzz, writes checked by e2fsck and debugfs, power cut after every write), os-sys, os-std |
| clippy | `-D warnings` for host crates, loader (UEFI), kernel and user crates |
| QEMU boot + kernel self-tests | boot stages, 24 in-kernel tests (incl. preemption, FPU, PCI, block I/O, tmpfs, symlinks/cwd/permissions, mount/unmount, devfs, FAT32 `/boot`, ext4 `/`, concurrent writers), exit 33 |
| ring-3 init checks | 56 user checks: OSyx Std, syscalls, processes, threads, sync, TLS, faults, and 19 file system checks |
| process teardown | every process returns exactly its frames; 0 processes and 2 threads left |
| preemptive scheduling | three never-yielding children all start before the first finishes; 3×300 ms in < 650 ms |
| storage tree | GPT partitions found, ext4 `/` read-write, FAT32 `/boot`, tmpfs `/tmp` `/run`, devfs `/dev`, DRBG seeded; all 7 VFS self-tests ok |
| OSyx Std fs from ring 3 | 19 `fs` checks; a child dropped to uid 1000 passes 12 permission checks; leaked handles closed |
| ESP checked on the host | mtools read the files init wrote to `/boot`; `fatcheck` finds the volume consistent |
| ext4 root checked on the host | `e2fsck -fn` clean after shutdown; `debugfs` reads init's text, a 200 000-byte pattern (SHA-256), a renamed tree, a symlink, a 0640 mode and confirms a removed file is gone |
| power cut | QEMU stopped while a process holds a removed file open: on disk `needs_recovery`, "not clean", an orphan; the next boot replays the journal and frees the orphan; `e2fsck -fn` clean |
| no entropy source | without virtio-rng, `/dev/random` is not created and the self-tests still pass |
| user page fault | process killed with `#PF`, error code 0x4, kernel survives |
| user panic | panic message printed, status 101 |
| user exit code | status 42 propagates |
| boot without programs | kernel runs without `\EFI\OSYX\BIN` |
| exception diagnostics | kernel `#PF`, `#GP`, `#UD`, `#DE`: decoded report + panic exit 37 |
| invalid kernel | ELFCLASS32 kernel rejected with on-screen error |
| screenshots | loader error screen and running kernel screen via QMP `screendump` |

## Last recorded result

All 23 tests passed (181 host unit tests, 24 kernel self-tests, 56 user
checks of which 19 are file system checks, host verification of both
partitions, the power-cut scenario) on Windows 11 build 26300 with Rust
1.99.1, QEMU 11.1.2, mtools 4.0.49 and e2fsprogs 1.47.0 (WSL Ubuntu 24.04).
In the ext4 host tests, a 108-write workload was cut after every possible
write; all 108 states recovered consistently (13 also checked with
e2fsck). QEMU TCG is close to deterministic here, so repeated boots do not
stress the scheduler with different interleavings; the host tests of
OSyx Std's `Mutex`, `Condvar` and threads run on real parallel host threads.
## PE32+ parser corpus sweep

`pe-sweep` (release build with `-C overflow-checks=on`) over the read-only
Windows 11 build 26300 directories of the development host. Files are only
read; nothing is copied into the repository.

| Directory | Files | Parsed | Rejected | Panics | Table errors |
|---|---|---|---|---|---|
| `System32` | 4372 | 4333 | 39 PE32 (32-bit) | 0 | 0 |
| `System32\drivers` | 467 | 466 | 1 PE32 | 0 | 4: `.pdata` with begin > end in four VirtualBox drivers (confirmed in the files with an independent script) |
| `SysWOW64` | 2746 | 3 | 2737 PE32, 6 16-bit NE files | 0 | 0 |

In System32 the sweep walked 648 074 imports from 98 040 import
descriptors, 319 142 exports (4 974 forwarders, every name and ordinal
looked up again), 8 942 918 base relocations, 154 TLS callbacks and
5 117 656 runtime functions with their unwind headers.

The first sweep found three parser defects, now fixed and covered by
regression tests: names longer than the original 512-byte limit (C++
decorated exports of up to 699 bytes; limit raised to 4096), zero-length
`RUNTIME_FUNCTION` entries (`symcryptk.dll`), and empty unwind-code arrays
ending exactly at a section's file data (kernel-mode modules).

## Not covered yet

Real hardware, other firmware, resolutions other than the default GOP choice
(the layout itself is unit-tested for 640×480 – 3840×2160), multi-core,
memory sizes other than 512 MiB, disk controllers other than virtio-blk,
full or damaged volumes inside QEMU (covered by the host tests of the
drivers), ext4 volumes written by Linux with journal checksums v3 (the
replay path is implemented, but the fixtures' journals use no checksums).
