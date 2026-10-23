# Windows 11: API set schema and export forwarding (loader inputs)

First, light pass of the Windows 11 research that was deferred earlier. It
answers two questions the future OSyx PE32+ loader depends on: how
`api-ms-win-*` / `ext-ms-win-*` import names resolve to real DLLs, and how
much of the Win32 surface kernel32 actually implements versus forwards.

Method: the host's own files were only **read** (never executed or copied
into the repository). REA confirmed the identity of the schema file
(`open_binary`: PE, x86-64, SHA-256
`568c72f6216d83a716d6fa9bec59ae8d93e6728062d969ac362013f3910c0007`); no
native disassembler provider is configured on this host and none was
needed. The data was decoded with OSyx's own PE parser and a format reader
written for this purpose:
`cargo run -p osyx-pe --example apiset-dump -- <apisetschema.dll> [dll...]`.
No Microsoft code was decompiled or reproduced; what follows are facts
about data formats and export tables.

Host: Windows 11 build 26300; `apisetschema.dll` and `kernel32.dll` file
version 10.0.26100.9549.

## API set schema

Observed:

- The schema lives in the `.apiset` section of `apisetschema.dll` as a
  namespace with **version 6**, 171 272 bytes, **985 API sets** (364
  `api-ms-*`, 621 `ext-ms-*`), hash factor 31.
- Layout (offsets relative to the namespace start, little-endian):
  header (version, size, flags, count, entry offset, hash offset, hash
  factor); 24-byte entries (flags, name offset/length, hashed length, value
  offset, value count); 20-byte values (flags, importer-name offset/length,
  host-name offset/length); 8-byte hash records (hash, entry index). Names
  are UTF-16LE, stored without `.dll`.
- The hashed length covers the name **without its last version
  component**: `api-ms-win-core-file-l1-2-5` hashes the 25 characters of
  `api-ms-win-core-file-l1-2`. Recomputing `h = h * 31 + c` over the
  lower-cased hashed part finds **all 985** entries in the hash table, so a
  loader can look up any minor revision of a contract by binary search on
  the hash.
- Each set lists a default host (empty importer name) and optional
  importer-specific hosts. **6** sets have one: five `api-ms-win-core-*`
  contracts map to `kernel32.dll` by default but to `kernelbase.dll` when
  imported by kernel32 itself (which avoids kernel32 importing from itself),
  and `ext-ms-win-kernel32-errorhandling-l1-1-0` maps to `faultrep.dll` for
  kernel32.
- **139** sets have no host on this edition (`ext-ms-*` extensions for
  components that are not installed); imports through them must fail
  cleanly (or resolve to OSyx stubs that report the absence).
- Most common default hosts among 285 distinct ones: kernelbase.dll 125,
  user32.dll 82, kernel32.dll 45, advapi32.dll 37, sechost.dll 25,
  gdi32full.dll 16, shcore.dll 16, kernel.appcore.dll 15, ucrtbase.dll 15
  (all `api-ms-win-crt-*`), windows.storage.dll 15, win32kfull.sys 14,
  combase.dll 13.

## Export forwarding

| DLL | Exports | Implemented in the DLL | Forwarded | Main forwarding targets |
|---|---|---|---|---|
| ntdll.dll | 2 523 | 2 523 | 0 | — |
| kernelbase.dll | 2 037 | 1 933 | 104 | ntdll 104 |
| kernel32.dll | 1 698 | 1 487 | 211 | ntdll 77, kernelbase 52, API sets 82 (e.g. `api-ms-win-core-ioring-l1-1-0` 12, `…processthreads…` 20) |
| advapi32.dll | 854 | 758 | 96 | ntdll 32, cryptsp 25, API sets, cryptbase 11, kernelbase 3, sechost 2 |

Observed: kernel32 forwards not only to DLL names but also to **API set
names**, so forwarder resolution has to go through the API set schema too.
"Implemented in the DLL" only means the export points at an address inside
that image; this pass did not look at code, so how much work those entry
points do themselves is unknown.

## Consequences for the OSyx PE loader (design, not yet implemented)

1. Import and forwarder names that start with `api-` or `ext-` are API set
   contracts: strip `.dll`, lower-case, cut at the last `-`, hash with
   factor 31, look up; pick the importer-specific host if one matches the
   importing module, else the default host; no host means "not present".
2. Forwarder chains (kernel32 → kernelbase/API set → ntdll) must be
   followed with a depth limit (cycle protection).
3. OSyx will ship its **own** schema (same lookup semantics, OSyx host DLL
   names) rather than reading Microsoft's file, and its own implementations
   of the contracts it supports.

## Not covered

Delay-load descriptors, SxS/manifests, KnownDLLs, loader search order, TLS
callback ordering, ARM64EC/CHPE variants, and the edition differences of
the schema (only this host's file was read).
