# Windows compatibility

**Status: initial PE32+ runtime implemented and tested in QEMU.** The first
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

## Architecture and supported surface

`/bin/winrun program.exe [args...]` is a normal OSyx ring-3 program. It maps
validated PE images through `compatibility/pe-loader`, enforces W^X, applies
DIR64 relocations, resolves named/ordinal imports and export forwarders,
maps the implemented API set contracts, builds PEB/TEB and TLS state, runs
DLL/TLS initializers, then calls the entry using the Windows x64 ABI. GS
holds each Windows thread's TEB and is switched by the kernel. FS remains
available to the OSyx runtime. A program fault terminates its OSyx process.

The built-in system DLLs take precedence over DLL files; other DLLs are
searched beside the executable and in the current directory. Missing DLLs
and symbols produce a loader diagnostic before the entry point executes.

| Module | Implemented subset |
|---|---|
| ntdll | File/section/memory operations, thread creation and waits, process/time queries, last-error/status helpers |
| kernel32 / kernelbase | Console and synchronous file I/O, handles, process/thread identity and exit, threads, events/mutexes/semaphores/waits, TLS, virtual memory/heap, clocks, environment/arguments, dynamic DLL loading, strings and UTF conversion |
| kernel32 volume APIs | C: root volume information/capacity, drive enumeration, UTF-16 FindFirstFile/FindNextFile/FindClose with ASCII-insensitive wildcards |
| user32 / gdi32 | Top-level PRISM windows, message queues, paint/input/timers, software GDI text/brushes/pens/rectangles, MB_OK and MB_OKCANCEL dialogs |
| ws2_32 | IPv4 TCP/UDP sockets, bind/connect/listen/accept, send/receive, basic socket options, DNS/getaddrinfo and ordinal imports |
| setupapi | Read-only ALLCLASSES/PRESENT inventory from real kernel PCI functions and attached HID inputs; descriptions, classes, hardware IDs, locations |
| iphlpapi | GetAdaptersInfo, GetIfTable, GetNetworkParams and synchronous ICMP echo, backed by OSyx interface counters/configuration, DNS and PingSocket |

SetupAPI rejects unsupported properties (including registry driver-service
information); it does not install drivers or fabricate driver names.
IP Helper reports unknown link speed as zero. Adapter statistics are the
real stack counters; MIB_IFROW's legacy DWORD counters wrap at 32 bits.

## Programs and tests

`scripts/build-wintests.ps1` uses MSYS2 clang/lld and public import libraries
to build the test executables without a C runtime. `scripts/win-test.ps1`
boots `init.args=win,7111,7112 test.exit`, runs them on OSyx, and checks both
their own assertions and the host TCP/UDP echo server's log:

- `minimal.exe`: relocation and exit code 42.
- `hello.exe`: console, command line, PEB/TEB, modules and errors.
- `fileio.exe`: files, sharing and delete-on-close inside one winrun process.
- `threads.exe` / `windll.dll`: synchronization, TLS, memory and DLL loading.
- `winsock.exe`: TCP and UDP against a host server.
- `gui.exe`: a real PRISM window, GDI painting and timers.
- `sysadmin.exe`: volume/directory data, buffer sizing, device/adapter
  enumeration, handle lifetime and ICMP payload preservation. C static
  assertions independently verify the Windows x64 structure sizes.

`scripts/build-winapps.ps1` builds four GUI applications from
`compatibility/win32/apps/src`: Text Editor, Disk Utility, Device Manager
and Network Center. The same PE files also run on Windows. `build.ps1`
installs them in `/opt/apps`; desktop entries invoke them through winrun.
The desktop integration test launches and closes each with USB input.

## Known limits

This is a small Win32 subset, not general Windows application compatibility.
There is no MSVC C runtime/UCRT implementation, PE32/WoW64, .NET, DirectX,
COM, registry service, Windows kernel drivers, child-window controls,
SEH unwinding or general process creation. Imports outside the implemented
surface fail. Delay imports and arbitrary application installers are not
supported. The virtual-memory backend allocates eagerly; reservation is
not demand paging, and PAGE_NOACCESS currently has a read-only backing
mapping (see `mem.rs`). File sharing and named synchronization tables are
local to one winrun process. ANSI strings are UTF-8 with Latin-1 fallback,
not a full Windows code-page implementation.

C: maps to `/`; other drive letters and UNC paths are rejected. Mounted
filesystems are available through paths such as `C:\boot` and `C:\tmp`.
Disk Utility currently lists C:; it does not manage partitions. Device
Manager reports OSyx's PCI/input inventory, not all Windows PnP properties.
Network Center supports IPv4; custom ICMP IP options fail explicitly.

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

The executable tests and the module table above are the current subset
matrix. See [testing.md](testing.md) for recorded validation results.
