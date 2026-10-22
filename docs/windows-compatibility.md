# Windows compatibility

**Status: not started.** Windows 11 reverse-engineering research was deferred
at the user's request and will be resumed as a separate stage.

## Principles

- Independent implementation of observable behaviour; no Microsoft code,
  no redistributed Microsoft binaries, no Wine/Proton/ReactOS code.
- Research notes stay in `research/` (with question, Windows build,
  methodology, evidence, uncertainties, proposed implementation, regression
  tests) and are kept separate from implementation code.
- Host research uses small documented-API test programs and REA static
  analysis; nothing on the host is patched or replaced.
- Unsupported behaviour returns explicit NTSTATUS / Win32 errors; no stubs that
  pretend to succeed.

## Planned order

PE32+ parser → PE loader without imports → minimal NTDLL/Kernel32 surface
(`NtClose`, `NtAllocateVirtualMemory`, `NtWriteFile`, … backed by real kernel
facilities) → object manager and handle tables → Win32 console subset.

## Compatibility matrix

None yet; it will live in `research/compatibility-matrix/`.
