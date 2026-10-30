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
| 7b | Scheduler, kernel threads, preemption, FPU state, spawn/wait | Done (SMP) |
| 7c | Threads per process, futexes, FS-base TLS, OSyx Std `thread`/`Mutex`/`Condvar` | Done (SMP) |
| 7d | Storage: PCI, virtio-blk, block layer, FAT32, VFS, file handles, OSyx Std `fs` | Done (polled I/O; SMP) |
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

1. **Real-hardware coverage**: expand testing of NVMe, AHCI, physical NICs and Wi-Fi across more machines.
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
   client redraws and report-protocol keyboards, AXIS pipelines
   and blocks (see [graphics.md](graphics.md)).

## Real hardware: current scope and limits

NVMe, AHCI, physical Ethernet and Intel Wi-Fi drivers are present. The README lists the ThinkPad T495 as tested hardware; support on other machines and firmware varies. NVMe and AHCI use polled I/O.

- **Display**: GOP on physical hardware; VirtIO-GPU under QEMU. Native GPU modesetting is not implemented.
- **SMP**: user threads run in parallel; kernel entry uses a common lock.
- **USB**: xHCI supports hubs; device and topology coverage remains limited.
- **Laptop input**: PS/2 input is supported; I2C-HID touchpads and native gesture protocols are not supported.
- **Power**: shutdown requests ACPI S5 using FADT and the static DSDT _S5 package where available. Firmware behavior varies.
