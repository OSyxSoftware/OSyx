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
  4-level paging with W^X/NX, growable heap, PIC/PIT timer, framebuffer status
  screen, 11 in-kernel self-tests.
- User space: ring 3 processes, SYSCALL/SYSRET ABI with pointer validation,
  and a Rust program built on OSyx Std (`println!`, `Vec`, `BTreeMap`, heap,
  time, args, panics) running as `init`.

## Quick start (Windows 11 host)

```powershell
.\scripts\check-env.ps1   # verify Rust targets, QEMU, OVMF
.\scripts\build.ps1       # build loader, kernel, init; assemble build\esp
.\scripts\run-qemu.ps1    # boot in a QEMU window (serial log: build\serial.log)
.\scripts\test.ps1        # host unit tests, clippy, QEMU boot/fault/user tests
```

## Layout

| Path | Contents |
|---|---|
| `boot/uefi-loader` | OSyx Boot Manager (UEFI application) |
| `boot/gfx` | integer-only 2D renderer, fonts, splash layers (shared with kernel) |
| `boot/elf` | bounds-checked ELF64 validation (kernel and user policies) |
| `boot/assets` | generated logo/font assets (`scripts/gen-assets.py`) |
| `kernel` | the kernel; `kernel/mm` holds host-tested memory algorithms |
| `shared/bootinfo`, `shared/abi` | boot protocol and system call ABI |
| `runtime/os-sys`, `runtime/os-std` | raw system calls, OSyx Std |
| `userspace/hello` | first user program (`INIT.ELF`) |
| `docs` | design and status documentation |

Documentation starts at [docs/architecture.md](docs/architecture.md).
