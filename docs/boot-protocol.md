# Boot protocol (BootInfo v2)

Defined in `shared/bootinfo` (`repr(C)`, layout checked by unit tests:
`BootInfo` is 168 bytes, `MemoryDescriptor` 24 bytes).

## Boot volume layout (EFI System Partition)

| Path | Required | Purpose |
|---|---|---|
| `\EFI\BOOT\BOOTX64.EFI` | yes | OSyx Boot Manager |
| `\EFI\OSYX\KERNEL.ELF` | yes | kernel, static ELF64 `ET_EXEC` |
| `\EFI\OSYX\INIT.ELF` | no | first user program |
| `\EFI\OSYX\CMDLINE.TXT` | no | command line (≤ 4096 bytes) |

## Boot manager stages

The progress bar advances only when a stage has completed:

1. firmware services, command line, GOP mode selection, splash
2. kernel located
3. kernel read
4. kernel validated (`osyx-elf`, kernel policy) and segments loaded; init module read
5. page tables, stack, BootInfo, memory map prepared
6. `ExitBootServices`, final frame drawn, jump to kernel

Any failure shows a red error state with a description on screen and on
COM1, waits up to 30 s for a key, then returns the error to the firmware.

## Machine state at kernel entry

- Long mode, interrupts disabled, `CR3` = boot-built PML4, `EFER.NXE` set when
  the CPU supports NX.
- Page tables:
  - identity map of `[0, hhdm_size)` (2 MiB pages; removed by the kernel),
  - HHDM: the same range at `hhdm_offset = 0xFFFF_8000_0000_0000`, non-executable,
  - kernel segments in the top 2 GiB with 4 KiB pages and their ELF permissions.
- `hhdm_size` = highest RAM address (RAM-backed memory types only), at least
  4 GiB, rounded to 1 GiB, extended to cover the framebuffer.
- `RDI` = HHDM address of `BootInfo`; `RSP` = top of a 128 KiB stack
  (`KernelData`), with a zero return address pushed (System V entry alignment).
- The firmware's GDT/IDT are still loaded; the kernel replaces them before
  it allocates memory.

## BootInfo fields

| Field | Meaning |
|---|---|
| `magic`, `version`, `size` | `"OSYXBOOT"`, 2, `size_of::<BootInfo>()` (checked by `validate()`) |
| `bootloader_version` | `major << 16 \| minor << 8 \| patch` |
| `flags` | `FLAG_FRAMEBUFFER`, `FLAG_ACPI`, `FLAG_INIT_MODULE` |
| `framebuffer` | physical base, size, width, height, stride (pixels), RGB/BGR |
| `memory_map` | HHDM pointer to sorted, merged, non-overlapping descriptors |
| `kernel` | physical base/size, virtual base, entry point |
| `acpi_rsdp` | physical RSDP address (ACPI 2.0 preferred) |
| `cmdline_ptr/len` | HHDM pointer to UTF-8 command line |
| `hhdm_offset/size` | direct map description |
| `init_module/size` | HHDM pointer to the raw `INIT.ELF` bytes (unvalidated) |

## Memory ownership

| `MemoryKind` | Owner after hand-off |
|---|---|
| `Usable` | free RAM (includes former boot-services memory) |
| `BootloaderReclaimable` | BootInfo, memory map, command line, init module, loader image and buffers; the kernel copies what it needs, then frees it |
| `KernelImage` | kernel segments |
| `KernelData` | boot page tables and boot stack (still in use) |
| `AcpiReclaimable`, `AcpiNvs`, `Mmio`, `Reserved`, `Unusable` | never allocated |

## Command line options

| Option | Effect |
|---|---|
| `boot.fast` | skip boot animations |
| `boot.mode=WxH` | preferred GOP resolution (default: largest ≤ 1920×1080) |
| `init.args=a,b` | extra arguments for init (`argv[0]` is `init`) |
| `init.expect=N` | expected init exit status for `test.exit` (default 0) |
| `test.exit` | leave QEMU after boot checks (status 33 ok, 35 failure, 37 panic) |
| `test.fault=pf\|gp\|ud\|de` | trigger a kernel CPU exception to test diagnostics |
