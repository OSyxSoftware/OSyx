# Testing

## Commands

```powershell
.\scripts\check-env.ps1             # tool check, no changes to the system
.\scripts\build.ps1 [-Cmdline '…'] [-NoInit] [-DebugBuild]
.\scripts\run-qemu.ps1 [-Cmdline '…'] [-Gdb] [-Follow] [-Headless]
.\scripts\test.ps1 [-SkipHost] [-SkipQemu]
```

QEMU runs `q35`, `qemu64,+nx`, 512 MiB, OVMF from the MSYS2 QEMU package, the
ESP as a read-only virtio FAT disk (`fat:build/esp`, `vda`), the writable
64 MiB FAT32 data disk `build/data.img` (`vdb`, recreated by `build.ps1`
from `build/data-seed`), COM1 to a log file and `isa-debug-exit` at port
`0xf4`. Logs and screenshots go to `build/test/`.

Host tools for the data disk: `cargo run -p osyx-fat --example mkfat32` and
`--example fatcheck`, and mtools from MSYS2 (`mdir`, `mtype`, `mcopy`).

## Suite (`scripts/test.ps1`)

| Test | Checks |
|---|---|
| host unit tests | `cargo test` for abi, bootinfo, gfx, elf, pe, mm, fat, os-sys, os-std (the PE parser adds fixture, malformed-input and mutation tests; the FAT driver adds mtools interoperability tests in both directions) |
| clippy | `-D warnings` for host crates, loader (UEFI), kernel and user crates |
| QEMU boot + kernel self-tests | boot stages, 18 in-kernel tests (incl. preemption, sleep/join, FPU, FS base, PCI, block I/O, VFS), exit 33 |
| ring-3 init checks | 49 user checks: OSyx Std, syscall validation, spawn/wait, zombies, orphans, sleep, FPU, threads, Mutex/Condvar, futex, TLS, exit with blocked threads, faults/panics in threads, file system |
| process teardown | every process returns exactly its frames; 0 processes and 2 threads left |
| preemptive scheduling | three never-yielding children all start before the first finishes; 3×300 ms in < 650 ms |
| storage stack | both virtio-blk disks found, data disk mounted at `/` as FAT32 `OSYXDATA`, PCI/block/VFS self-tests ok |
| OSyx Std fs from ring 3 | 12 `fs` checks ok; the kernel closed the 11 handles a child leaked at exit |
| host reads OSyx-written files | after QEMU exits: `mtype`/`mcopy` read the text, the worker's file and a 200 000-byte pattern byte for byte; `fatcheck` finds the volume consistent with an accurate FSInfo |
| boot without data disk | storage self-tests and `fs` checks skip; 37 other user checks pass |
| user page fault | process killed with `#PF`, error code 0x4, kernel survives |
| user panic | panic message printed, status 101 |
| user exit code | status 42 propagates |
| boot without programs | kernel runs without `\EFI\OSYX\BIN` |
| exception diagnostics | kernel `#PF`, `#GP`, `#UD`, `#DE`: decoded report + panic exit 37 |
| invalid kernel | ELFCLASS32 kernel rejected with on-screen error |
| screenshots | loader error screen and running kernel screen via QMP `screendump` |

## Last recorded result

All 21 tests passed (133 host unit tests, 18 kernel self-tests, 49 user
checks of which 12 are file system checks, host verification of the written
data disk) on Windows 11 build 26300 with Rust 1.99.1, QEMU 11.1.2 and
mtools 4.0.49. QEMU TCG is close to deterministic here, so repeated boots do
not stress the scheduler with different interleavings; the host tests of
OSyx Std's `Mutex`, `Condvar` and threads run on real parallel host threads.
The suite takes about 2 minutes with a warm build cache.

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
full or damaged volumes inside QEMU (covered only by host tests of the FAT
driver), power loss during a write.
