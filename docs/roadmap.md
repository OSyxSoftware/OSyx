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
| 7e | File system architecture: ext4 root (journalled), FAT32 `/boot`, tmpfs, devfs, GPT, permissions, cwd, symlinks, mount/umount | Done (see storage.md for limits) |
| 7f | Native executable loader: ELF64 from the VFS (`/bin`, `/usr/bin`), `execve`, argv/envp/auxv, OSyx Std environment | Done (static `ET_EXEC` only; see kernel.md) |
| 7g | Kernel objects and IPC: pipes, channels, shared memory, wait_handles (ABI v7) | Done |
| 7h | Native xHCI driver, USB enumeration, HID keyboard/mouse, input core | Done (root ports, boot keyboards; see graphics.md) |
| 7i | PRISM compositor, protocol v1, UI toolkit, desktop shell, AXIS Terminal, Files, System Information | Done (software composition; see graphics.md) |
| 8 | Native Windows PE execution | Not started |

## Next steps (in order)

1. **PE loader milestone 1**: map a minimal PE32+ with no imports into a
   user address space (relocations, section permissions) and run it, read
   from the ext4 root through the same path as ELF programs (`exec.rs`).
2. **Windows 11 research with REA**: continue from
   `research/windows11-apiset-and-forwarding.md` with loader behaviour
   (search order, KnownDLLs, TLS callbacks, delay loads).
3. **PE loader milestone 2**: `hello.exe` against a minimal in-tree kernel32
   (`GetStdHandle`, `WriteFile`, `GetLastError`, `ExitProcess`).
4. **Storage follow-ups**: ext4 htree insertion and deeper extent trees, a
   page cache with delayed writes (and demand-paged program images), FAT
   timestamps, interrupt-driven virtio.
5. **Local APIC + ACPI (MADT)**: replace PIC/PIT; then SMP bring-up and
   per-CPU data with `swapgs`.
6. **More OSyx Std**: `RwLock`, `Once`/`OnceLock`, `mpsc`; then the
   genuine `std` port ([rust-std.md](rust-std.md)).
7. **Desktop follow-ups**: per-pixel alpha in the toolkit, damage-based
   client redraws, USB hubs and report-protocol keyboards, AXIS pipelines
   and blocks (see [graphics.md](graphics.md)).

## Real hardware: what is still missing

The kernel now shows its boot log on the GOP framebuffer from `_start`
(`log.screen` keeps it until a process takes the display), uses x2APIC
when the firmware enabled it, takes its tick from a calibrated local APIC
timer (PIT, ACPI PM timer or HPET as the reference; PIT virtual wire as
the fallback), bounds every driver timeout by the TSC as well as by
ticks, and does the xHCI BIOS-to-OS handoff with a 1 s limit. What a
typical PC still lacks:

- **Root disk drivers**: only virtio-blk exists. On real hardware no disk
  is found and init runs from the ESP boot modules (fallback root). NVMe
  (most PCs since ~2018) and AHCI/SATA are the next storage drivers.
- **Network**: only VirtIO-Net; Intel e1000e/igc or Realtek r8169 would be
  the first physical NICs.
- **Display**: the GOP framebuffer works anywhere; there is no native
  modesetting for Intel/AMD/NVIDIA GPUs (VirtIO-GPU only under QEMU).
- **SMP**: one CPU; ACPI is read only for timers so far (FADT PM timer, HPET), not the MADT.
- **USB**: root-port devices only (no hubs); keyboards behind a hub or a
  USB-C dock are not seen.
- **Power**: no ACPI S-states or AML; shutdown on real hardware only halts.
