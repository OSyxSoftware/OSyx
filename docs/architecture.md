# Architecture

## Components

```
UEFI firmware (OVMF)
  └─ BOOTX64.EFI  ── boot/uefi-loader  (+ boot/gfx, boot/elf, shared/bootinfo)
       │  loads \EFI\OSYX\KERNEL.ELF, \EFI\OSYX\BIN\*, CMDLINE.TXT
       │  builds page tables + BootInfo, ExitBootServices, jumps
       ▼
  kernel (ring 0)  ── kernel (+ kernel/mm, boot/gfx, shared/abi)
       │  GDT/IDT, memory, timer, scheduler, display, self-tests, processes
       │  SYSCALL/SYSRET (shared/abi)
       ▼
  init, worker (ring 3) ── userspace/apps  (+ runtime/os-std, runtime/os-sys)
```

## Design rules

- **Rust first.** Everything so far is Rust; x86-64 assembly is used only
  where it is the right tool (interrupt stubs, SYSCALL entry, ring
  transitions, fault-tolerant copy routines), via `global_asm!`/`asm!`.
  No C++ yet: no component has needed it (see [cpp-interop.md](cpp-interop.md)).
- **Stable toolchain, no external crates.** UEFI bindings, ELF parsing,
  allocators, locks and graphics are all in-tree. Builds use stable Rust.
- **Host-testable cores.** Hardware-independent algorithms live in `no_std`
  crates with host unit tests (`boot/elf`, `boot/gfx`, `kernel/mm`,
  `shared/*`, `runtime/os-std` host back end). Hardware paths are tested by
  in-kernel self-tests and QEMU boots.
- **Untrusted input is validated.** Kernel and user ELF images, BootInfo,
  and every user pointer passed to a system call.
- **W^X everywhere.** Kernel and user segments, anonymous mappings.

## Status

| Area | Status | Tested |
|---|---|---|
| Graphical UEFI boot manager | Implemented | QEMU boot + error screen screenshots |
| BootInfo protocol v3 (boot modules) | Implemented | host layout tests, QEMU |
| Kernel early init (GDT/TSS/IDT, serial, panic) | Implemented | QEMU |
| Exception diagnostics (#PF/#GP/#UD/#DE) | Implemented | QEMU fault tests |
| Frame allocator, paging, kernel heap | Implemented | host + in-kernel tests |
| Timer interrupts (PIC/PIT) | Implemented | in-kernel test |
| Framebuffer status screen | Implemented | QEMU screenshot |
| User processes, ring 3, SYSCALL ABI v3 | Implemented | QEMU user tests |
| Threads per process, futexes, FS-base TLS | Implemented (single CPU) | in-kernel + QEMU user tests |
| Kernel threads, preemptive scheduler, FPU state | Implemented (single CPU) | in-kernel + QEMU user tests |
| spawn / wait / zombies / orphans | Implemented | QEMU user tests |
| OSyx Std (convenience std) | Partially implemented | host tests + running on OSyx |
| Genuine Rust `std` port | Planned | Not tested |
| SMP | Planned | Not tested |
| Filesystem (FAT32, VFS) | Planned | Not tested |
| ACPI, APIC, PCI, drivers | Planned | Not tested |
| C++23 components | Planned (no current need) | Not tested |
| PE32+ parser | Implemented (headers, sections, imports, exports, relocations, TLS, exception data) | host: fixtures, malformed, fuzz-style robustness; System32 sweep |
| PE32+ loader | Planned | Not tested |
| NT / Win32 compatibility | Planned | Not tested |
| Windows 11 reverse-engineering research | Deferred by request | Not started |
