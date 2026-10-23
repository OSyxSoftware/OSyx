# PE32+ parser and loader

## Parser (`compatibility/pe-parser`, crate `osyx-pe`) — implemented

A bounds-checked, zero-copy parser for PE32+ (Windows x64) images, written
from the published PE/COFF specification. `no_std` without `alloc`, so the
kernel can use it for the future loader. It only reads bytes; nothing is ever
executed.

| Area | What is parsed / validated |
|---|---|
| DOS header | `MZ`, `e_lfanew` in range (overflow-safe) |
| PE / COFF headers | `PE\0\0`, machine, section count (1..=96), characteristics |
| Optional header | PE32+ magic only (PE32 is rejected), declared size vs. 16 data directories, file/section alignment rules, `SizeOfImage` multiple of the section alignment, `SizeOfHeaders` covers the section table and fits file and image, entry point inside the image |
| Data directories | every present directory inside the image; the certificate table (index 4) inside the *file* |
| Section table | names, flags; virtual addresses section-aligned, ascending, not overlapping the headers or each other, inside `SizeOfImage`; raw data inside the file; overflow-checked |
| RVA translation | `rva_to_offset` (file-backed bytes only), `slice_at_rva` (one section), `read_mapped` (loader view: zero-filled section tails read as 0), `va_to_rva`, bounded NUL-terminated names (≤ 512 bytes, UTF-8) |
| Imports | descriptors until the null entry (≤ 4096), DLL names, ILT/IAT (falls back to the IAT without an ILT), by-name (hint + name) and by-ordinal thunks, reserved-bit checks, ≤ 65536 thunks |
| Delay imports | Bounded RVA-form descriptors, DLL names, name/ordinal thunks and module/bound/unload table RVAs; legacy VA-form descriptors are rejected |
| Resources | Numeric type/name/language tree with bounded offsets and file-backed payloads; checked VS_VERSIONINFO binary and UTF-16 value queries |
| Exports | directory, address/name/ordinal tables file-backed, ordinals within 16 bits, forwarders (`DLL.Name`, `DLL.#ordinal`) detected by RVA-inside-directory, binary search by name, lookup by ordinal, sorted-name check, linear `functions()`/`names()` iterators |
| Base relocations | block headers (size ≥ 8, even, inside the directory, page aligned inside the image), entry types (ABSOLUTE, DIR64, HIGHLOW, others reported for loader policy), target inside the image |
| TLS | `IMAGE_TLS_DIRECTORY64` VAs converted to RVAs inside the image, callback array (≤ 1024, null-terminated), template data |
| Exception data | `RUNTIME_FUNCTION` table size, begin < end, sorted and non-overlapping, inside the image; binary-search `lookup`; `UNWIND_INFO` header (version 1/2, flags, prolog, codes file-backed) |

Not parsed yet: debug directory contents, load config, bound imports,
CLR header, COFF symbol table, long section names. Resource string names
are not exposed by the numeric lookup API. Delay-import parsing supports
auditing; the runtime does not implement delay-load resolution yet.

### Tests

- `tests/fixtures.rs`: `hello.exe` (imports from kernel32) and `sample.dll`
  (named, NONAME and forwarded exports, TLS callback, DIR64 relocations,
  unwind data), compiled locally from `tests/fixtures/src` by
  `scripts/gen-pe-fixtures.ps1` (clang + lld, no C runtime). Expected values
  come from GNU objdump, not from the parser.
- `tests/malformed.rs`: an in-memory PE32+ builder; each test corrupts one
  field and checks the exact error (headers, alignment, sections, directories,
  imports, exports, relocations, TLS, exception data, mapped-memory reads).
- `tests/robustness.rs`: every truncation, every header byte mutated to six
  values, 9000 random multi-byte mutants, and every 4-byte field of the
  table sections set to boundary values — all accessors are exercised and
  must not panic (debug build, overflow checks on).

### Corpus sweep

`cargo run -p osyx-pe --example pe-sweep -- <dir>` parses every
`.dll/.exe/.sys` file in a directory and walks all tables, counting
rejections, table errors and panics. It reads files only; nothing is copied
into the repository. See [testing.md](testing.md) for the last recorded run.
`pe-dump` prints the parse of one file.

`cargo run -p osyx-pe-loader --example dependency-audit -- <vendor-dir>
<qemu-serial-log> <output.json>` compares static and delay imports with
the actual `winrun --exports` manifest emitted in QEMU. It follows vendor
exports/forwarders and distinguishes missing DLLs, contracts and symbols.
Symbol presence is not a behavioural compatibility test; dynamically
requested dependencies are additional. See [Zen bring-up](research/zen-bringup.md).

## Loader — implemented initial subset

`compatibility/pe-loader` provides API set schema v6 parsing, validated
image layout and section protection, DIR64 relocations and linking against
built-in modules or PE DLLs. Imports by name and ordinal, export forwarding,
dependency cycles and failure cleanup are covered by host tests.

`compatibility/win32` provides the OSyx mapping backend and `/bin/winrun`:
DLL search, initializers, TLS callbacks/templates, PEB/TEB, GS-base and the
Windows x64 calling convention. Both console and GUI subsystem programs
run in the calling ring-3 process. The executable is loaded from the VFS;
unresolved imports fail before entering Windows code. See
[windows-compatibility.md](windows-compatibility.md) for the tested API
surface and limitations. No Microsoft binaries are distributed.
