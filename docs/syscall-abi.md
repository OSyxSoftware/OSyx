# System call ABI (version 2)

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
