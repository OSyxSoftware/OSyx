# Roadmap

Status of the milestones from the project brief.

| # | Milestone | Status |
|---|---|---|
| 1 | Repository, toolchain, scripts, docs | Done |
| 2 | Graphical UEFI boot loader | Done |
| 3 | Kernel boot (serial, panic, CPU init, exceptions) | Done |
| 4 | Memory subsystem (frames, paging, heap) | Done |
| 5 | Initial OSyx Std | Done (convenience std; real `std` port planned) |
| 6 | PE32+ parser | Done (host tested) |
| 7 | User-space foundations (ring 3, syscalls, exit, console) | Done |
| 7b | Scheduler, kernel threads, preemption, FPU state, spawn/wait | Done (single CPU) |
| 7c | Threads per process, futexes, FS-base TLS, OSyx Std `thread`/`Mutex`/`Condvar` | Done (single CPU) |
| 7d | Storage: PCI, virtio-blk, block layer, FAT32, VFS, file handles, OSyx Std `fs` | Done (polled I/O, single CPU) |
| 7e | File system architecture: ext4 root (journalled), FAT32 `/boot`, tmpfs, devfs, GPT, permissions, cwd, symlinks, mount/umount | Done (see storage.md for limits) |
| 7f | Native executable loader: ELF64 from the VFS (`/bin`, `/usr/bin`), `execve`, argv/envp/auxv, OSyx Std environment | Done (static `ET_EXEC` only; see kernel.md) |
| 7g | Kernel objects and IPC: pipes, channels, shared memory, wait_handles (ABI v7) | Done |
| 7h | Native xHCI driver, USB enumeration, HID keyboard/mouse, input core | Done (root ports, boot keyboards; see graphics.md) |
| 7i | PRISM compositor, protocol v1, UI toolkit, desktop shell, AXIS Terminal, Files, System Information | Done (software composition; see graphics.md) |
| 8 | Native Windows PE execution | Initial subset implemented (console, files, threads, DLLs, GUI, Winsock; see windows-compatibility.md) |
| 9 | Native networking | Implemented (VirtIO-Net and IPv4 stack; see network/net) |
| 10 | VirtIO-GPU display backend | Implemented (2D scanout/damage/cursor; software composition) |
| 11 | Task Manager, Settings and four PE32+ desktop utilities | Implemented initial versions |

## Next steps (in order)

1. **Real-hardware storage/network**: NVMe/AHCI for persistent root storage,
   and a driver for a physical NIC. The current drivers target VirtIO.
2. **Windows compatibility expansion**: cross-process sharing/object names,
   child controls, true inaccessible reserved pages, SEH, delay imports,
   process creation and the runtime dependencies of larger applications.
3. **Windows 11 research with REA**: continue from
   `research/windows11-apiset-and-forwarding.md`, maintaining explicit
   supported/unsupported boundaries and executable regression programs.
4. **Storage follow-ups**: ext4 htree insertion and deeper extent trees, a
   page cache with delayed writes (and demand-paged program images), FAT
   timestamps, interrupt-driven virtio.
5. **SMP follow-ups**: audit kernel paths for finer-grained parallel execution,
   then replace the common kernel entry lock and add load balancing policies.
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
- **Network**: VirtIO-Net, Intel e1000/e1000e (older ids) and the Realtek
  r8169 family (RTL8111/8168, 8101, 8169; generic bring-up without the
  per-chip PHY tables, untested on hardware so far). No Intel igc/I219
  yet; Wi-Fi and Bluetooth chips are only identified (`devices`).
- **Display**: the GOP framebuffer works anywhere; there is no native
  modesetting for Intel/AMD/NVIDIA GPUs (VirtIO-GPU only under QEMU).
- **SMP**: enabled logical CPUs are discovered through MADT and started with INIT/SIPI. User threads run in parallel (up to 16 CPUs); kernel entry remains serialized pending a finer-grained locking audit.
- **USB**: root-port devices only (no hubs); keyboards behind a hub or a
  USB-C dock are not seen.
- **Laptop input**: the i8042 PS/2 keyboard and PS/2 touchpad (as a
  mouse with wheel) are polled at 1 kHz; I2C-HID touchpads and native
  touchpad protocols (Synaptics, ELAN gestures) are not supported.
- **Power**: no ACPI S-states or AML; shutdown on real hardware only halts.
