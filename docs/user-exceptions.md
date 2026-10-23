# Resumable user exceptions

OSyx 0.1.0 provides optional `FAULT_HANDLER` (77) and `FAULT_RETURN` (78)
calls alongside the existing ABI 10 calls. Existing numbers, structures and
version checks are unchanged. A kernel without this extension returns
NotSupported; registering a Win32 vectored callback then fails explicitly.

`fault_handler(entry)` installs one process-wide System V
`fn(*mut osyx_abi::fault::Context) -> !`. Zero disables it. Threads share
the registration, but each has its own outstanding fault. Spawned processes
start without it; successful execve resets it. Thread/process exit releases
outstanding kernel records.

The kernel saves the actual integer/control registers and FXSAVE64 state.
For supported user CPU exceptions it places the 688-byte, 16-byte-aligned
context below the interrupted stack's red zone, then enters the callback
with normal System V stack alignment. An unusable stack, unsupported
exception vector, missing handler or fault inside an outstanding handler
retains process-termination behaviour. There is no alternate signal
stack or nested exception stack yet.

`fault_return(context)` only succeeds for a pending fault of the calling
thread and never returns on success. It validates user RIP/RSP and MXCSR
against the saved hardware mask. CS/SS come from the original trusted
interrupt frame. IOPL, NT and VM cannot be injected; IF stays enabled.
An IRET continuation restores all integer registers, including RCX/R11
(which ordinary SYSRET would clobber), and x87/SSE. AVX extended state is
not provided. The kernel stays soft-float throughout delivery and return.

## Win32 bridge

`compatibility/win32/src/exceptions.rs` translates the actual context into
the documented 1232-byte Win64 CONTEXT and 152-byte EXCEPTION_RECORD. The
following operations are backed by real dispatch:

- Add/RemoveVectoredExceptionHandler, including first/last ordering;
- Add/RemoveVectoredContinueHandler after a handled exception;
- SetUnhandledExceptionFilter as the fallback when vectored handlers decline;
- RaiseException with bounded 64-bit arguments and continuable dispatch.

The RaiseException entry thunk captures the PE caller's real registers,
stack, return address and FPU state before Rust helper frames run. A native
CPU trap transports the software exception through the same validated
delivery/return path. A successful continuation resumes the captured PE
caller, with any supported CONTEXT edits. Callback list snapshots are freed
before abandoning the dispatcher stack, avoiding a per-exception heap leak.

The bridge maps UD2, integer division, breakpoints and memory access faults
to Windows exception codes. Memory faults include the real fault address
and read/write/execute information. INT3's Windows context points to the
breakpoint byte; a continuation handler must advance it when appropriate.

This implements vectored dispatch, not frame-based SEH or C++ unwinding.
Debugger dispatch, nested delivery, stack overflow recovery, debug-register
editing and AVX state remain unsupported. Unhandled exceptions terminate;
continuing a noncontinuable software exception also terminates. No callback
registration succeeds merely to suppress a loader error.

## Verification

Native `worker fault-checks` executes UD2 and verifies register/SSE recovery,
independent threads, rejection of a kernel RIP/RSP and reserved MXCSR bits,
and sanitization of IOPL/NT/VM. Children verify no-handler termination,
exec reset, nested-fault termination and an unusable fault stack. It has
11 checks, including assertions performed by each actual fault callback.

`exceptions.exe` is built from the same C source for Windows and OSyx. Its
16 assertions cover UD2 CONTEXT edits, handler order, real read/write faults,
division, INT3, software RaiseException arguments, independent threads,
continue callbacks and the top-level filter. It passed on Windows and in
QEMU. The fixture needs `-fasynchronous-unwind-tables` for Windows to walk
the freestanding entry function when invoking its top-level filter.

The QEMU Windows integration run including these checks passed 312 checks,
0 failed (`build/zen/veh-final-qemu-tests.log` and matching `.serial.log`).
The broader kernel/desktop/network regression is recorded separately in
`build/zen/veh-full-regression.log`.

Public references: [vectored exception handling](https://learn.microsoft.com/en-us/windows/win32/debug/vectored-exception-handling),
[AddVectoredExceptionHandler](https://learn.microsoft.com/en-us/windows/win32/api/errhandlingapi/nf-errhandlingapi-addvectoredexceptionhandler).
