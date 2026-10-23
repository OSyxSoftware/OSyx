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
7. PCI enumeration; wall clock from the CMOS RTC (`time.rs`); virtio-blk
   disks (`vda`, …) and their GPT partitions (`vda1`, …); `/dev/random`
   seeded (`random.rs`); device nodes registered (`devices.rs`); the file
   system tree mounted: ext4 `/`, devfs, tmpfs `/tmp` `/run`, FAT32 `/boot`
   ([storage.md](storage.md)).
8. Framebuffer status screen, self-tests, `/bin/init` from the root file
   system spawned (`init=` picks another program, `init.args=a,b` adds
   arguments; environment `PATH=/bin:/usr/bin HOME=/ TERM=osyx`) and waited
   for, then `kmain` animates the status screen (sleeping between frames). With
   `test.exit` every volume is unmounted cleanly before QEMU is stopped
   (`test.power-cut` skips that to simulate a power loss).

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
held), `BootCell` and `KMutex`, a sleeping mutex for long operations (file
system and disk I/O): waiters block in the scheduler instead of spinning,
and the holder may be preempted. Spinlocked data uses `IrqSpinLock`, so a
thread is never preempted while holding a spinlock. A `KMutex` is never
taken while a spinlock is held. Lock order (outer → inner):
`KMutex (open file → volume → disk) → BLOCK/MOUNTS → process TABLE → FUTEX →
KMutex state → SCHED → OUTPUT_TAIL → HEAP → PAGING → MMIO → FRAMES → PCI → SERIAL`.
The panic path may force-unlock SERIAL and only try-locks the display.

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
- CR3, TSS.RSP0, the SYSCALL stack and the FS base (user TLS) follow the
  incoming thread.
- Kill requests: a flag on the thread (set when its process exits) that is
  checked at safe points: the end of every system call and every interrupt
  that returns to ring 3. Blocked threads are woken so they reach one.
- `sleep_ticks`, `yield_now`, `join`, and a block/wake protocol whose
  "register wake-up, mark blocked, schedule" sequence runs with interrupts
  disabled so wake-ups cannot be lost. A dead thread's stack is freed by the
  next thread right after the switch.

## Processes (`process.rs`)

- Created by `spawn(parent, request)` from an executable file (see
  [Program loader](#program-loader)) with one main thread; `thread_spawn`
  adds more (up to 64). Each process has its own address space: ELF
  segments with their permissions, a 512 KiB stack below
  `0x7FFF_FFFF_0000`, null guard below 64 KiB, System V initial stack
  (`argc`, `argv`, `envp`, `auxv`). `execve` swaps in a new address space
  (below).
- `exit` (or a fatal CPU exception in any thread) records the exit status
  and asks the other threads to stop; `thread_exit` ends one thread. The last
  thread to end destroys the address space, drops the process's futex
  registrations and leaves a zombie with the exit status; the parent's waiting
  threads are woken. `wait(parent, pid | 0)` returns and releases a zombie.
- Futexes (`futex.rs`): waiters keyed by (pid, address), FIFO wake-up,
  timeouts through the scheduler's sleep mechanism.
- Children of an exiting process become kernel orphans and are released as
  soon as they exit.
- Frame accounting: every frame a process causes (root, page tables, pages)
  is counted; teardown checks that exactly that many come back and logs
  `FRAME LEAK` otherwise.
- File handles: a per-process table (`3..`, at most 64) of open VFS files;
  whatever is still open when the last thread ends is closed.
- Identity: `uid`/`gid` (root for `init`; `setuid` lets root drop to a user,
  permanently) and the current directory; children inherit both.
- Anonymous memory (`map`, `map_ex`) is demand-zero: a mapping reserves
  address space, and each page gets a zeroed frame on its first access
  (from user code or a kernel copy to/from user memory). `protect` applies
  to touched pages at once and to the others when touched; `discard`
  returns pages' frames while keeping the mapping.
- Limits: 512 processes, 256 threads per process, 64 GiB of `map` address
  space per process (frames bounded by physical memory), 128 KiB of
  arguments and environment (4096 strings each), 64 MiB executables, 64
  open handles.

## Program loader

`exec.rs` finds and reads programs; `process.rs` maps them.

- **Lookup**: a name without `/` is tried as `/bin/<name>`, then
  `/usr/bin/<name>` (`osyx_abi::PROGRAM_PATH`); a path with `/` is resolved
  from the caller's current directory. `vfs::open_exec` requires a regular
  file with execute permission for the caller (root: any `x` bit), not read
  permission. Missing everywhere → `NotFound`; found only without
  permission → `PermissionDenied`.
- **Reading**: the ELF header and program header table are read first;
  then only the header range and the file data of each `PT_LOAD` segment
  (debug information is never read). Anything that does not look like a
  sane header table is read whole, so the parser reports precisely what is
  wrong.
- **Validation** (`osyx-elf`, user policy): ELF64, x86-64, `ET_EXEC`, no
  `PT_INTERP`/`PT_DYNAMIC`, page-aligned, non-overlapping W^X segments in
  `[64 KiB, 0x1000_0000_0000)` (below the anonymous-memory area), entry in
  an executable segment. Failures are `BadExecutable` and logged.
- **Mapping** into a fresh address space: segments page by page (file
  bytes, then zeros), 512 KiB stack, then the start-up data built by
  `osyx_abi::startup` (argv, envp, auxv with `AT_PHDR`, `AT_ENTRY`,
  `AT_RANDOM`, `AT_EXECFN`, …; see [syscall-abi.md](syscall-abi.md)).
- **`execve`**: refused with `Busy` if the process has other threads. The
  new image is complete before anything changes; then, with interrupts off,
  the process table entry gets the new space (anonymous regions reset,
  name updated), CR3 and the thread's root are switched, FS base is zeroed,
  x87/SSE state reset, the old space destroyed with its frame accounting
  checked (`old image released N frames, no leak`), and the thread enters
  ring 3 at the new entry with `iretq`. The system call's kernel stack is
  abandoned; nothing on it owns memory at that point.
- **Boot modules** (`\EFI\OSYX\BIN`) are only used when there is no usable
  root partition: they are installed into `/bin` of the fallback tmpfs
  root.
- Disk requests are polled: the virtio queue spins briefly before it starts
  yielding, because a yield to a CPU-bound thread costs a whole time slice.

## Self-tests (run on every boot)

boot memory map · frame allocator · page map/unmap (+ error cases) · exception
fixups (#GP and #PF) · identity map removed · kernel W^X · kernel heap
(including growth) · breakpoint exception · timer interrupts · user copy
validation · address space lifecycle · preemptive kernel threads (two
threads that never yield both make progress) · sleep and join · FPU/SSE
state per thread · FS base per thread · PCI enumeration · block device I/O ·
VFS on tmpfs · symlinks, cwd, permissions and case-insensitive lookup ·
mount, unmount and limits · devfs and `/dev/random` · FAT32 `/boot` · ext4
`/` · concurrent writers on tmpfs, FAT32 and ext4 followed by volume checks ·
program loader (search path, execute permission, directories, non-ELF
files, relative paths, argument limits).
Tests of volumes that are not mounted skip with a log line.

## Not implemented yet

Thread priorities, SMP and per-CPU data via `swapgs` (the futex and
block/wake protocols rely on disabling interrupts on a single CPU and must be
revisited for SMP), shared memory between processes, local APIC/IOAPIC, ACPI
parsing, PCIe ECAM, device interrupts/MSI-X, storage controllers other than
virtio-blk, NTFS, keyboard input, XSAVE (AVX state), dynamically linked or
position-independent (`ET_DYN`) programs, `#!` interpreter scripts,
set-user-ID programs, close-on-exec handles, demand paging of program
images.
