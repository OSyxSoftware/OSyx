# Roadmap

Status of the milestones from the project brief.

| # | Milestone | Status |
|---|---|---|
| 1 | Repository, toolchain, scripts, docs | Done |
| 2 | Graphical UEFI boot loader | Done |
| 3 | Kernel boot (serial, panic, CPU init, exceptions) | Done |
| 4 | Memory subsystem (frames, paging, heap) | Done |
| 5 | Initial OSyx Std | Done (convenience std; real `std` port planned) |
| 6 | PE32+ parser | Done (host tested; loader not started) |
| 7 | User-space foundations (ring 3, syscalls, exit, console) | Done |
| 7b | Scheduler, kernel threads, preemption, FPU state, spawn/wait | Done (single CPU) |
| 7c | Threads per process, futexes, FS-base TLS, OSyx Std `thread`/`Mutex`/`Condvar` | Done (single CPU) |
| 7d | Storage: PCI, virtio-blk, block layer, FAT32, VFS, file handles, OSyx Std `fs` | Done (polled I/O, single CPU) |
| 8 | Native Windows PE execution | Not started |

## Next steps (in order)

1. **PE loader milestone 1**: map a minimal PE32+ with no imports into a
   user address space (relocations, section permissions) and run it; with
   the VFS in place it can be loaded from the data disk instead of a boot
   module.
2. **Windows 11 research with REA**: deferred by request; restart with the
   PE loader behaviour questions (import resolution, relocations, TLS).
3. **PE loader milestone 2**: `hello.exe` against a minimal in-tree kernel32
   (`GetStdHandle`, `WriteFile`, `GetLastError`, `ExitProcess`).
4. **Storage follow-ups**: `spawn` from a path on the VFS, FAT timestamps
   (needs an RTC), interrupt-driven virtio with several requests in flight.
5. **Local APIC + ACPI (MADT)**: replace PIC/PIT; then SMP bring-up and
   per-CPU data with `swapgs`.
6. **More OSyx Std**: `RwLock`, `Once`/`OnceLock`, `mpsc`, current
   directory; then the genuine `std` port ([rust-std.md](rust-std.md)).
7. **Input**: PS/2 or virtio keyboard, a text console and a shell.
