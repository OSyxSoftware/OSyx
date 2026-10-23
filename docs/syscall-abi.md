# System call ABI (version 3)

Defined in `shared/abi`; kernel side in `kernel/src/syscall.rs`, user side in
`runtime/os-sys`.

## Convention

- `syscall` instruction; number in `rax`; arguments in `rdi, rsi, rdx, r10, r8, r9`.
- `rcx` and `r11` are clobbered; all other registers are preserved.
- Result in `rax`: success values are non-negative; errors are `-errno`
  (`-1 … -4095` as `i64`).
- Entry runs with interrupts masked (FMASK clears IF, TF, DF, NT, AC) until
  the stack switch, then re-enables them; a system call may be preempted and
  may block (`wait`, `sleep`).

## Calls

| # | Name | Arguments | Result |
|---|---|---|---|
| 0 | `exit` | `code: i32` | does not return |
| 1 | `write` | `fd` (1 stdout, 2 stderr), `buf`, `len ≤ 1 MiB` | bytes written |
| 2 | `map` | `len`, `prot` (R=1, W=2, X=4) | address of zeroed pages |
| 3 | `unmap` | `addr`, `len` (must match one whole `map`) | 0 |
| 4 | `sysinfo` | `out: *mut SystemInfo` (56 bytes) | 0 |
| 5 | `clock` | `id` (0 = monotonic since boot) | nanoseconds (10 ms resolution) |
| 6 | `spawn` | `name`, `name_len ≤ 31`, `args`, `args_len ≤ 4096` | child pid |
| 7 | `wait` | `pid` (0 = any child), `status: *mut i32` (may be null) | pid of the reaped child |
| 8 | `yield` | — | 0 |
| 9 | `sleep` | `ns` (rounded up to 10 ms ticks) | 0 |
| 10 | `thread_spawn` | `entry`, `stack_top` (16-byte aligned), `arg` (→ `rdi`), `fs_base` | thread id |
| 11 | `thread_exit` | `code`, `done_word: *mut u32` (may be null) | does not return |
| 12 | `futex_wait` | `addr` (4-byte aligned), `expected: u32`, `timeout_ns` (0 = none) | 0 |
| 13 | `futex_wake` | `addr`, `count` | threads woken |
| 14 | `set_fs_base` | `addr` (0 or a user address) | 0 |
| 15 | `gettid` | — | thread id |

### Threads

- `thread_spawn` starts a thread in the calling process (at most 64 per
  process); every other register is zero. Thread ids are global.
- `thread_exit` ends the calling thread. The kernel then stores 1 to
  `done_word` and wakes all futex waiters on it; because this happens after
  the thread has left its user stack, a joiner that sees 1 may free that
  stack. When the last thread of a process ends, the process exits with
  `code` (unless `exit` set a status earlier).
- `exit` ends the whole process: every other thread is asked to stop and does
  so at its next safe point (return to user mode, or an interrupted
  `futex_wait`, `sleep` or `wait`, which fail with `Interrupted`). The last
  thread to stop releases the address space.
- A CPU exception in any thread ends the whole process.

### Futexes

`futex_wait` reads `*addr`, and only if it equals `expected` blocks until a
`futex_wake` on the same address in the same process, the timeout, or process
exit. Check, registration and blocking are atomic with respect to
`futex_wake`, so a wake that follows a store cannot be lost. Waiters are woken
in FIFO order; callers must tolerate spurious returns.

### Thread-local storage

Each thread has its own FS base, saved and restored on context switch.
OSyx Std points it at a thread control block whose first word is its own
address (`fs:[0]`).

`spawn` starts the boot module `name` (a file of `\EFI\OSYX\BIN\`); `args` is
a block of NUL-terminated UTF-8 strings that become `argv[1..]` (`argv[0]` is
`name`). `wait` validates `status` before blocking, so a bad pointer returns
`Fault` and leaves the child waitable.

## Errors

| errno | Name | Typical cause |
|---|---|---|
| 1 | InvalidArgument | zero length, bad flags, unknown clock, partial unmap, malformed spawn arguments, invalid executable |
| 2 | Fault | pointer outside user space, unmapped, or read-only for output |
| 3 | NoMemory | frames exhausted, 256 MiB per-process `map` limit, 64-process limit |
| 4 | BadHandle | unknown file descriptor |
| 5 | NotSupported | unknown system call number |
| 6 | PermissionDenied | writable + executable mapping (W^X) |
| 7 | NotFound | `spawn` of a program that is not a boot module |
| 8 | NoChild | `wait` with no (matching) child |
| 9 | WouldBlock | `futex_wait`: the word did not hold `expected` |
| 10 | TimedOut | `futex_wait` timeout expired |
| 11 | Interrupted | a blocking call ended because the process is exiting |

## Pointer validation

Every user buffer is checked to lie inside `[64 KiB, 0x8000_0000_0000)` with
overflow-checked arithmetic, then copied with a fault-recovering `rep movsb`
(`fixup.rs`). An unmapped page or a write to a read-only page therefore
returns `Fault`. These paths are exercised from ring 3 by `userspace/apps`
(`init`) in QEMU.

## Process start-up and exit

`rsp` points to `argc`, followed by `argv[0..argc]`, `NULL`, an empty `envp`
(`NULL`), and an `AT_NULL` auxiliary vector; `rsp` is 16-byte aligned and all
other general purpose registers are zero. SSE is enabled; each thread has its
own x87/SSE state. Fatal CPU exceptions terminate the process with status
`0x4000_0000 | vector`.
