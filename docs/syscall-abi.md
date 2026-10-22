# System call ABI (version 1)

Defined in `shared/abi`; kernel side in `kernel/src/syscall.rs`, user side in
`runtime/os-sys`.

## Convention

- `syscall` instruction; number in `rax`; arguments in `rdi, rsi, rdx, r10, r8, r9`.
- `rcx` and `r11` are clobbered; all other registers are preserved.
- Result in `rax`: success values are non-negative; errors are `-errno`
  (`-1 … -4095` as `i64`).
- Entry runs with interrupts masked (FMASK clears IF, TF, DF, NT, AC) until
  the stack switch, then re-enables them.

## Calls

| # | Name | Arguments | Result |
|---|---|---|---|
| 0 | `exit` | `code: i32` | does not return |
| 1 | `write` | `fd` (1 stdout, 2 stderr), `buf`, `len ≤ 1 MiB` | bytes written |
| 2 | `map` | `len`, `prot` (R=1, W=2, X=4) | address of zeroed pages |
| 3 | `unmap` | `addr`, `len` (must match one whole `map`) | 0 |
| 4 | `sysinfo` | `out: *mut SystemInfo` (56 bytes) | 0 |
| 5 | `clock` | `id` (0 = monotonic since boot) | nanoseconds (10 ms resolution) |

## Errors

| errno | Name | Typical cause |
|---|---|---|
| 1 | InvalidArgument | zero length, bad flags, unknown clock, partial unmap |
| 2 | Fault | pointer outside user space, unmapped, or read-only for output |
| 3 | NoMemory | frames exhausted or 256 MiB per-process `map` limit |
| 4 | BadHandle | unknown file descriptor |
| 5 | NotSupported | unknown system call number |
| 6 | PermissionDenied | writable + executable mapping (W^X) |

## Pointer validation

Every user buffer is checked to lie inside `[64 KiB, 0x8000_0000_0000)` with
overflow-checked arithmetic, then copied with a fault-recovering `rep movsb`
(`fixup.rs`). An unmapped page or a write to a read-only page therefore
returns `Fault`. All six error paths are exercised by `userspace/hello` in
QEMU.

## Process start-up

`rsp` points to `argc`, followed by `argv[0..argc]`, `NULL`, an empty `envp`
(`NULL`), and an `AT_NULL` auxiliary vector; `rsp` is 16-byte aligned and all
other general purpose registers are zero. Fatal CPU exceptions terminate the
process with status `0x4000_0000 | vector`.
