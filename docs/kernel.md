# Kernel

`kernel/` is a `#![no_std]`, `#![no_main]` Rust binary for
`x86_64-unknown-none` (kernel code model, no red zone, soft-float), linked
as a static ELF at `0xFFFF_FFFF_8000_0000` with three W^X segments
(`kernel/linker.ld`).

## Initialisation order (`kernel/src/main.rs`)

1. COM1 serial, BootInfo validation, copy of BootInfo / memory map / command
   line into kernel memory.
2. GDT with TSS (IST stacks for #DF, NMI, #MC), IDT (256 gates), SYSCALL MSRs.
3. CPU identification, `CR0.WP` set.
4. Frame allocator, paging state (NX, kernel root, pre-allocated upper-half
   PDPTs), removal of the boot identity map.
5. Kernel heap; init module copied; bootloader memory reclaimed.
6. PIC remapped (vectors 32–47), PIT at 100 Hz, interrupts enabled.
7. Framebuffer status screen, self-tests, init program, idle loop (`sti; hlt`).

## Interrupts and exceptions

- `arch/idt.rs`: 16-byte stubs per vector push a uniform `TrapFrame`.
- Kernel-mode #PF/#GP at an instruction listed in the `.osyx_fixup` table
  resume at its recovery address (`fixup.rs`); this backs `probe_*` and
  `copy_from_user`/`copy_to_user`.
- Other kernel-mode exceptions print name, error code, decoded page-fault
  cause, CR2/CR3 and all registers, then panic.
- User-mode exceptions terminate the process with status
  `0x4000_0000 | vector`; the kernel keeps running.

## Locking

`sync.rs` provides `SpinLock`, `IrqSpinLock` (interrupts disabled while
held) and `BootCell`. Data touched from interrupt context uses
`IrqSpinLock`. Lock order (outer → inner):
`CURRENT → OUTPUT_TAIL → HEAP → PAGING → FRAMES → SERIAL`. The panic path may
force-unlock SERIAL and only try-locks the display.

## Processes (`process.rs`)

Single CPU, one process at a time, run to completion (no scheduler yet):

- address space: new PML4, upper half shared with the kernel;
- ELF segments mapped with their permissions, 256 KiB stack below
  `0x7FFF_FFFF_0000`, null guard below 64 KiB;
- initial stack in System V layout (`argc`, `argv`, `envp`, `auxv`);
- ring 3 entered with `iretq`; `exit` or a fatal exception restores the saved
  kernel context; the address space is destroyed and the frame count is
  compared against the count before the process started (leak check).

## Self-tests (run on every boot)

boot memory map · frame allocator · page map/unmap (+ error cases) · exception
fixups (#GP and #PF) · identity map removed · kernel W^X · kernel heap
(including growth) · breakpoint exception · timer interrupts · user copy
validation · address space lifecycle.

## Not implemented yet

Scheduler and threads, SMP, local APIC/IOAPIC, ACPI parsing, PCI, storage and
filesystems, keyboard input, FPU/SSE state for user programs (user code is
soft-float), per-CPU data via `swapgs`.
