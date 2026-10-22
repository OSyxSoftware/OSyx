# PE32+ parser and loader

**Status: not started.** This session focused on the boot loader, kernel and
OSyx Std as requested.

## Planned parser (`compatibility/pe-parser`, Rust, `no_std`, host tested)

DOS header and `e_lfanew`, PE signature, COFF header, optional header (PE32+
only), section table, data directories, imports, exports (including
forwarders), base relocations, TLS directory, exception directory metadata,
RVA → file offset translation. Every offset and size is bounds checked with
overflow-checked arithmetic; malformed input returns an error, never panics.

Test corpus: binaries compiled locally for the tests, plus synthetic malformed
images (truncation, bad offsets, overlapping or corrupted sections, invalid
imports and relocations). Parsing never executes the input.

## Planned loader stages

1. Map a PE32+ image with no imports into a user address space, apply base
   relocations, set section permissions, run its entry point.
2. Resolve imports against a minimal in-tree DLL providing `GetStdHandle`,
   `WriteFile`, `GetLastError`, `ExitProcess` with real behaviour.
3. A non-trivial console program; a compatibility matrix in
   `research/compatibility-matrix/`.

The existing pieces it will build on: user address spaces, the ELF loader's
segment mapping code, `copy_to_user`/`copy_from_user`, and the system call ABI.
