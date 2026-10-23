# OSyx

OSyx is a new x86-64 operating system written from scratch, primarily in Rust:
its own graphical UEFI boot manager, its own kernel, and its own user-space
standard library (OSyx Std). The long-term goal is native execution of
unmodified Windows PE32+ applications through an independently developed NT
compatibility layer; that work has not started yet (see
[docs/roadmap.md](docs/roadmap.md)).

## What works today (tested in QEMU + OVMF)

- `BOOTX64.EFI`: graphical boot screen (logo, progress bar tied to real boot
  stages, animated spinner), ELF64 kernel validation, page tables, BootInfo,
  ExitBootServices, kernel hand-off; clear error screen for bad kernels.
- Kernel: GDT/TSS/IDT, exception diagnostics, bitmap frame allocator,
  4-level paging with W^X/NX, growable heap, preemptive round-robin scheduler
  with kernel threads and per-thread FPU/SSE and FS state, framebuffer status
  screen, 25 in-kernel self-tests.
- Storage ([docs/storage.md](docs/storage.md)): GPT boot disk; a native,
  journalled **ext4** driver as `/` (crash recovery and orphan handling
  tested by cutting power at every write), the EFI partition as FAT32
  `/boot`, tmpfs `/tmp` and `/run`, devfs `/dev` (null, zero, a
  ChaCha20-based `random`, console, root-only disks); a VFS with mounts,
  symbolic links, current directories, Unix permissions and per-process
  handles. What OSyx writes is verified on the host with e2fsck, debugfs
  and mtools.
- User space: static ELF64 programs loaded from the file system
  (`/bin/init` starts the system; names are searched in `/bin` and
  `/usr/bin`), `spawn` and `execve` with argv, environment and a System V
  auxiliary vector; multi-threaded ring 3 processes with wait, zombies and
  orphans, futexes and FS-base TLS, SYSCALL/SYSRET ABI v6 with pointer
  validation, and Rust programs built on OSyx Std (`println!`, collections,
  heap, time, args, environment variables, `process::Command` incl. `exec`,
  `thread::spawn`, `thread_local!`, `Mutex`, `Condvar`, `fs`,
  `env::current_dir`, `SystemTime`, panics): `init` runs 69 checks using
  `worker` and `probe` children.

- Windows compatibility groundwork: Windows 11 research started
  ([API sets and forwarding](docs/research/windows11-apiset-and-forwarding.md));
  a bounds-checked, zero-copy PE32+ parser
  (headers, sections, imports, exports with forwarders, relocations, TLS,
  exception data), tested on local fixtures, malformed and mutated inputs,
  and swept without a single panic over the 4 372 files of `System32`.

## Quick start (Windows 11 host)

```powershell
.\scripts\check-env.ps1   # verify Rust targets, QEMU, OVMF
.\scripts\build.ps1       # build loader, kernel, programs; GPT disk build\osyx.img (ESP + ext4 root)
.\scripts\run-qemu.ps1    # boot in a QEMU window (serial log: build\serial.log)
.\scripts\test.ps1        # host unit tests, clippy, QEMU boot/fault/user tests
```

Building the ext4 root image and checking it needs WSL (Ubuntu 24.04) with
e2fsprogs; MSYS2 provides QEMU, OVMF and mtools. Only image files under
`build/` are written; no host disk or partition is touched.

## Layout

| Path | Contents |
|---|---|
| `boot/uefi-loader` | OSyx Boot Manager (UEFI application) |
| `boot/gfx` | integer-only 2D renderer, fonts, splash layers (shared with kernel) |
| `boot/elf` | bounds-checked ELF64 validation (kernel and user policies) |
| `boot/assets` | generated logo/font assets (`scripts/gen-assets.py`) |
| `kernel` | the kernel; `kernel/mm` holds host-tested memory algorithms |
| `compatibility/pe-parser` | PE32+ parser (`osyx-pe`), fixtures, `pe-dump` / `pe-sweep` tools |
| `storage/ext4` | ext4 driver (`osyx-ext4`), e2fsprogs-made test images |
| `storage/fat` | FAT32 driver (`osyx-fat`), `mkfat32` / `fatcheck` tools |
| `storage/vfs`, `storage/tmpfs`, `storage/part` | VFS building blocks, tmpfs, GPT (`mkdisk`) |
| `shared/crypto` | ChaCha20 DRBG and SHA-256 for `/dev/random` |
| `shared/bootinfo`, `shared/abi` | boot protocol and system call ABI |
| `runtime/os-sys`, `runtime/os-std` | raw system calls, OSyx Std |
| `userspace/apps` | user programs `init`, `worker` (`/bin`) and `probe` (`/usr/bin`) |
| `docs` | design and status documentation |

Documentation starts at [docs/architecture.md](docs/architecture.md).
