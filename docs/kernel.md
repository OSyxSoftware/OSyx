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
5. Kernel heap; boot modules copied; bootloader memory reclaimed.
6. PIC remapped (vectors 32–47), PIT at 100 Hz, interrupts enabled, SSE
   enabled for user code, scheduler started (the boot flow becomes thread 1,
   `kmain`; thread 2 is `idle`).
7. Framebuffer status screen, self-tests, `init` spawned and waited for,
   then `kmain` animates the status screen (sleeping between frames).

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
held) and `BootCell`. All kernel data uses `IrqSpinLock`, so a thread is never
preempted while holding a lock. Lock order (outer → inner):
`process TABLE → SCHED → OUTPUT_TAIL → HEAP → PAGING → FRAMES → SERIAL`. The
panic path may force-unlock SERIAL and only try-locks the display.

## Threads and scheduling (`sched.rs`)

- Threads: 64 KiB kernel stack each (the boot thread keeps the loader's
  stack), saved RSP, 512-byte FXSAVE area, state Ready/Running/Blocked/Dead.
- `osyx_switch` saves callee-saved registers and RFLAGS and switches stacks;
  new threads start in a trampoline that calls `entry(a, b)`.
- Round robin with a 20 ms time slice (2 ticks of the 100 Hz PIT). The timer
  handler wakes sleepers and, when the slice is used up and another thread is
  ready, switches from inside the interrupt; the preempted thread resumes in
  that handler later. `idle` (`sti; hlt`) runs only when nothing is ready.
- x87/SSE state is saved and restored on every switch (eager FXSAVE); SSE is
  enabled in CR0/CR4. The kernel itself is compiled soft-float.
- CR3, TSS.RSP0 and the SYSCALL stack follow the incoming thread.
- `sleep_ticks`, `yield_now`, `join`, and a block/wake protocol whose
  "register wake-up, mark blocked, schedule" sequence runs with interrupts
  disabled so wake-ups cannot be lost. A dead thread's stack is freed by the
  next thread right after the switch.

## Processes (`process.rs`)

- Created by `spawn(parent, name, args)` from a boot module; each process has
  one thread (its main thread) and its own address space: ELF segments with
  their permissions, a 256 KiB stack below `0x7FFF_FFFF_0000`, null guard below
  64 KiB, System V initial stack (`argc`, `argv`, `envp`, `auxv`).
- `exit` (or a fatal CPU exception in ring 3) destroys the address space at
  once and leaves a zombie with the exit status; the parent's waiting threads
  are woken. `wait(parent, pid | 0)` returns and releases a zombie child.
- Children of an exiting process become kernel orphans and are released as
  soon as they exit.
- Frame accounting: every frame a process causes (root, page tables, pages)
  is counted; teardown checks that exactly that many come back and logs
  `FRAME LEAK` otherwise.
- Limits: 64 processes, 256 MiB of `map` memory per process, 16 KiB of
  arguments.

## Self-tests (run on every boot)

boot memory map · frame allocator · page map/unmap (+ error cases) · exception
fixups (#GP and #PF) · identity map removed · kernel W^X · kernel heap
(including growth) · breakpoint exception · timer interrupts · user copy
validation · address space lifecycle · preemptive kernel threads (two
threads that never yield both make progress) · sleep and join · FPU/SSE
state per thread.

## Not implemented yet

Multiple threads per process, thread priorities, SMP and per-CPU data via
`swapgs`, local APIC/IOAPIC, ACPI parsing, PCI, storage and filesystems,
keyboard input, XSAVE (AVX state).
