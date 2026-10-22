# Roadmap

Status of the milestones from the project brief.

| # | Milestone | Status |
|---|---|---|
| 1 | Repository, toolchain, scripts, docs | Done |
| 2 | Graphical UEFI boot loader | Done |
| 3 | Kernel boot (serial, panic, CPU init, exceptions) | Done |
| 4 | Memory subsystem (frames, paging, heap) | Done |
| 5 | Initial OSyx Std | Done (convenience std; real `std` port planned) |
| 6 | PE32+ parser | Not started |
| 7 | User-space foundations (ring 3, syscalls, exit, console) | Done (single process) |
| 8 | Native Windows PE execution | Not started |

## Next steps (in order)

1. **Scheduler and threads**: kernel threads, preemption on the timer, a
   process table, `spawn`/`wait`; save/restore FPU/SSE state.
2. **Local APIC + ACPI (MADT)**: replace PIC/PIT; then SMP bring-up and
   per-CPU data with `swapgs`.
3. **Storage**: PCI enumeration, virtio-blk driver, block layer, FAT32, VFS,
   file handles; OSyx Std `fs`.
4. **PE32+ parser** (`compatibility/pe-parser`): bounds-checked headers,
   sections, imports, exports, relocations, TLS, exception directory; host
   tests with locally compiled binaries and malformed inputs.
5. **Windows 11 research with REA**: deferred by request; restart with the
   PE loader behaviour questions (import resolution, relocations, TLS).
6. **PE loader milestone 1**: map and run a minimal PE32+ with no imports.
7. **OSyx Std**: threads, sync, fs, path; then the genuine `std` port
   ([rust-std.md](rust-std.md)).
8. **Input**: PS/2 or virtio keyboard, a text console and a shell.
