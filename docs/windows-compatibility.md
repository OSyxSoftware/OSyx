# Windows compatibility

**Status: research started, implementation not started.** The first
Windows 11 research pass (API set schema, export forwarding) is in
[research/windows11-apiset-and-forwarding.md](research/windows11-apiset-and-forwarding.md).

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

## File system preparation (VFS)

The NT file semantics are meant to be implemented by the compatibility
layer on top of the VFS, not inside the file system drivers (ext4 stays a
plain ext4 driver):

| Windows concept | VFS support today | Still to build in the compatibility layer |
|---|---|---|
| Case-insensitive names | `OPEN_CASE_INSENSITIVE` / `Lookup::case_insensitive`: every component matched ASCII case-insensitively, exact match first, canonical spelling kept (mount points found too); FAT is case-insensitive natively | Unicode upcase table semantics, collisions (`a` and `A` in one ext4 directory) |
| Drive letters, `\` separators, `\\?\`, `\Device\…` | — | translation table `C:` → VFS path, object-namespace symbolic links to mount points |
| File handles | per-process handle tables of shared open files with reference counting | NT handle values, access masks, inheritance, `DuplicateHandle` |
| Sharing modes, byte-range locks | stable node identity `(FileStat::dev, FileStat::ino)` on every file system (FAT gets assigned numbers that survive renames) to key share/lock tables | the tables themselves, `ERROR_SHARING_VIOLATION`, delete-on-close |
| Removing open files | FAT refuses with `Busy` (like Windows); ext4/tmpfs allow it (POSIX) | Windows-style refusal for any volume when opened through the NT layer |
| Metadata | `FileStat`: size, times (ns), mode, owner, FAT attributes | creation time, attribute mapping for ext4, security descriptors |

## Compatibility matrix

None yet; it will live in `research/compatibility-matrix/`.
