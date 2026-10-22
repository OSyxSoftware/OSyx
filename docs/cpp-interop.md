# C++23 interoperability

**Status: planned, not implemented.** No component written so far had a
concrete engineering reason to use C++, so the project contains no C++ code
and no C++ runtime.

Tooling is available on the development host: clang 23.1.3 and LLD 23.1.3
(MSYS2 UCRT64), detected by `scripts/check-env.ps1`.

## Rules for when C++ is introduced

- Only with a documented reason (an established C++ interface, a library that
  is impractical to port, a measured performance need).
- Freestanding: `-ffreestanding -fno-exceptions -fno-rtti`, no host C++
  runtime, cross-compiled for the OSyx target (ELF, kernel or user code model
  as appropriate, no red zone in kernel code, soft-float until the kernel
  saves FPU state).
- Boundary: `extern "C"` functions with `noexcept`, `repr(C)` / standard-layout
  structs only; no Rust `Vec`, `String` or trait objects across it.
- No C++ exception may unwind into Rust frames.
- Static constructors run only through an explicit, Rust-controlled init call.
- Allocation goes through functions the Rust side exports.

The first planned step is a tiny `os_add` component linked into a QEMU-tested
user program, to validate the toolchain path end to end (host linking alone
does not count as evidence).
