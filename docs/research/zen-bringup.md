# Zen Browser Windows x64 bring-up

This work uses the unmodified official Windows x64 package, never a
replacement browser. OSyx stays at version 0.1.0. Zen is not registered in
the launcher or as the default browser until functional tests pass.

## Reproduction and provenance

Official release: https://github.com/zen-browser/desktop/releases/tag/1.23.4b
(linked from https://zen-browser.app/download/).

- Asset: `zen.installer.exe`, 118361088 bytes.
- SHA-256: `f8d6f7b52f5a08eb44ebc487fad2cf14c78887f38cd21552f0801e3e8f8269df`.
- Downloaded hash matches the asset digest returned by the official GitHub API.
- The installer is extracted with Windows tar; it is never executed.
- `application.ini`: Zen 1.23.4b, Gecko 158.0.1, build 20261020090730,
  source commit 541ee55196395ee8082862b83c84c90ab41d9f6d.
- The full `core` payload is 65 files / 449041763 bytes, including the
  original mozglue, xul, NSS, Visual C++ libraries and browser resources.
- Downloaded/vendor files remain ignored under `build/zen`.

Run `powershell -ExecutionPolicy Bypass -File scripts/zen-test.ps1`.
It verifies/downloads the pinned package, builds a 1 GiB ext4 root image,
installs the full payload in `/opt/zen`, then boots QEMU with 4 GiB RAM.
Init starts PRISM and invokes `/bin/winrun -v /opt/zen/zen.exe --no-remote
--profile C:\home\user\zen-profile about:blank`.

The probe's exit code **2** means blocked/unverified, never browser success.
Kernel boot success (QEMU exit 33) is recorded separately. Evidence is in
`build/zen/qemu.serial.log` and `build/zen/qemu-result.json`.

## Attempt 1: existing runtime

Actual QEMU result: winrun exit 126; QEMU exit 33; no kernel panic.

```
winrun: cannot load zen.exe: API set api-ms-win-core-version-l1-1-1.dll (needed by mozglue.dll) has no host
```

The executable parses as AMD64 PE32+ GUI, preferred base 0x140000000,
image size 0x137000. It imports the supplied mozglue.dll. That DLL imports
GetFileVersionInfoSizeW/GetFileVersionInfoW through the version contract,
and VerQueryValueW through its l1-1-0 revision. These require real version
resource parsing; they cannot be implemented as success-returning stubs.

No browser window, tabs, page rendering, input or HTTPS success has been
demonstrated. Those remain required gates for launcher/default registration.

## Attempts 2–4: implemented and measured progress

All four attempts booted the real disk image in QEMU, started PRISM and
invoked the original `/opt/zen/zen.exe`. Each ended with winrun status 126,
QEMU status 33 and no kernel panic. QEMU status 33 means the test boot
completed; it does **not** mean Zen ran successfully. Linking failed
before the browser entry point was called.

| Attempt | New implementation available | Actual next loader failure, needed by mozglue.dll |
|---|---|---|
| 1 | Existing runtime | api-ms-win-core-version-l1-1-1.dll has no host |
| 2 | PE version resources and version.dll | api-ms-win-core-psapi-l1-1-0.dll has no host |
| 3 | PSAPI module enumeration and synchronization | kernel32.dll!SetWaitableTimerEx does not exist |
| 4 | Waitable timers | api-ms-win-security-base-l1-1-0.dll has no host |

Preserved evidence: `build/zen/attempt-{1,2,3,4}.serial.log` and matching
`.result.json` files. A compact tracked snapshot, including SHA-256 hashes
of these logs and exact missing DLL/contract lists, is
[`research/compatibility-matrix/zen-1.23.4b.json`](../../research/compatibility-matrix/zen-1.23.4b.json).

The added functions perform real work:

- Bounds-checked PE numeric resource trees and VS_VERSIONINFO parsing;
  version.dll reads the target file's fixed values, UTF-16 strings and
  language translations. No invented application version is returned.
- PSAPI enumerates actual loader module handles for the current process,
  respects the caller's byte capacity and rejects remote process queries.
- SRW shared locks allow simultaneous readers. InitOnceExecuteOnce
  serializes the actual callback and retries failed initialization.
  Condition variables release/reacquire the actual SRW lock around a
  kernel futex wait. WaitOnAddress supports aligned DWORDs; other sizes
  return ERROR_NOT_SUPPORTED.
- Waitable timers use real monotonic deadlines and OSyx futex timeouts,
  including periodic/manual/automatic reset, cancellation and absolute
  FILETIME conversion through the real wall clock. Timer APC callbacks,
  suspend wakeups and requested high-resolution timers fail explicitly.
  Creation supports default security and TIMER_ALL_ACCESS; custom security
  or restricted access requests fail rather than pretending to enforce them.

The PE parser also reads bounded RVA-form delay-import descriptors for
the audit. This does not implement runtime delay-load resolution.

## Dependency audit and concrete blockers

`winrun --exports` prints the compiled runtime's real export tables and
API-set schema. The host audit consumes that QEMU manifest and the
unmodified package, rather than assuming Windows DLL availability:

```powershell
cargo run -q -p osyx-pe-loader --example dependency-audit -- `
  build/zen/1.23.4b/core build/zen/attempt-4.serial.log build/zen/dependencies.json
```

The probe script also runs this audit when a manifest was emitted.
Its report checks symbol presence only. It cannot establish that an
existing API's semantics satisfy Gecko, and additional dependencies loaded
through LoadLibrary/GetProcAddress are outside static analysis.

| Package measurement against attempt 4 runtime | Count |
|---|---:|
| Parsed PE32+ modules | 42 |
| Static import occurrences | 7892 |
| Delay import occurrences | 803 |
| Unresolved import occurrences | 4404 |
| Distinct unresolved DLL/symbol pairs | 1901 |
| Occurrences with no API-set host | 2182 |
| Occurrences with no DLL | 895 |
| Occurrences missing a runtime export | 1327 |
| Distinct missing DLL names / contract names | 53 / 74 |
| Parse errors | 0 |

Two PE32 maintenance/uninstall helpers are identified separately as
unsupported by the x64-only runtime. They are not parser failures and
are not the cause of the observed zen.exe startup failure.

The normal static startup closure contains zen.exe, mozglue.dll,
msvcp140.dll, vcruntime140.dll and vcruntime140_1.dll. This closure alone
has **437 unresolved import occurrences**: 274 unmapped-contract,
160 missing-export and 3 missing-DLL occurrences. Optional package DLLs
and delay imports are excluded from this narrower number. Gecko xul.dll
is loaded later and therefore is not in that static closure.

1. **Security and token semantics are the current observed blocker.**
   mozglue imports AddAccessAllowedAce, CreateWellKnownSid,
   GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor and
   SetSecurityDescriptorDacl/Group/Owner through the missing security
   contract. It also imports OpenProcessToken. These need a consistent
   token/SID/ACL model; mapping the contract to fake success functions
   would not implement the requested browser or its sandbox.
2. **The startup dependency closure also needs a real UCRT and NT/Win32
   runtime.** The vendor VC++ DLLs are present, but import missing
   api-ms-win-crt-* contracts: allocation, strings, locale, stdio,
   process initialization/exit and thread startup. mozglue additionally
   needs mapped-file views, vectored exception handlers, RtlVirtualUnwind,
   RtlLookupFunctionEntry, Rtl heap operations and registry access.
   zen.exe has unresolved shlwapi PathAppendW, PathMatchSpecW and
   PathRemoveFileSpecW imports. Many other unresolved imports are listed
   individually in the detailed JSON.
3. **Gecko's process and window/graphics dependencies remain unimplemented.**
   OSyx lacks general Windows process creation, SEH unwinding, COM,
   registry service, complex child controls and DirectX. The package
   imports shell32, ole32/oleaut32, rpcrt4, advapi32, comctl32, imm32,
   dxgi, d3d11, dcomp and windowscodecs. Some are optional/delay loaded;
   their absence does not prove every graphics path is mandatory.
   There is no tested software-rendering fallback in this build.
4. **Memory mapping needs architectural work for Gecko.** xul.dll alone
   is 177588080 bytes on disk and 178253824 bytes mapped. The current
   loader retains file/materialized/mapped copies and anonymous mappings
   are capped at 65536 pages (256 MiB) per process in
   `kernel/src/process.rs`. Their combined footprint is a prospective
   blocker inferred from code and image sizes; this allocation has not
   been reached in the actual Zen run. Raising a constant would not
   supply Windows reservation, demand paging or shared file mappings.
5. **Network support is not yet browser HTTPS support.** OSyx's WinSock
   subset already uses the native TCP/IP stack, but Zen has not reached
   it. NSS, softokn3 and freebl3 are present in the official payload;
   their existence does not demonstrate TLS initialization, trust-store
   handling or a verified HTTPS page. The audit also identifies crypt32,
   bcrypt/ncrypt, secur32 and other package dependencies. No TLS check
   has passed or been replaced with disabled certificate verification.

## Completion gates

| Requested stage | Actual state |
|---|---|
| Official Windows x64 download and verification | Complete |
| Attempt through the existing PE32+ loader | Four real QEMU attempts |
| Identify DLL/API/Gecko dependencies | Static/delay audit and observed startup failures recorded |
| Extend compatibility | Resource/version, PSAPI, synchronization and timers implemented; full compatibility incomplete |
| Browser windows/input in PRISM | Not reached |
| Browser TCP/IP, verified HTTPS | Not reached |
| Tabs and page rendering | Not reached |
| Menu/default registration | Withheld until functional success |

## Continued bring-up: attempt 5

The active objective remains a real running Zen Browser. The security
data APIs and native-credential token queries were subsequently implemented
in `compatibility/win32/src/security.rs`, with security-base mapped to
advapi32. No token privilege, impersonation or sandbox success is fabricated.
TokenUser/Groups/Owner/PrimaryGroup/Type use actual kernel uid/gid; unsupported
classes fail. Rights-reduced token duplicates cannot use TOKEN_QUERY.
Win32 creation calls reject custom DACL/inheritance requests they cannot
enforce, before creating the object or file.

The same PE security fixture passed **40 checks on Windows** and **46 checks
per invocation on OSyx**, including runs as native uid 0 and uid 1000.
The updated QEMU Windows integration suite passed **293 checks, 0 failed**;
Clippy for the changed userspace crates passed with -D warnings. Evidence:
`build/zen/security-final-windows.log`, `security-final-qemu.serial.log`,
`security-final-qemu-tests.log` and `security-clippy.log`.

Actual attempt 5 passed the security-base import gate and stopped at:

```
kernel32.dll!AddVectoredExceptionHandler (needed by mozglue.dll) does not exist
```

QEMU exited 33, winrun exited 126, no kernel panic. Files:
`build/zen/attempt-5.serial.log`, `attempt-5.result.json`,
`attempt-5.dependencies.json`. The new manifest audit has 4314 unresolved
import occurrences (428 in the normal static startup closure), down from
4404/437 for attempt 4. Adding advapi32 also changes many classifications
from missing DLL to missing export; that reclassification alone is not
API implementation or browser success. The earlier table and full regression
above remain measurements of attempt 4's baseline, not this newer state.

The next implementation requirement is real CPU exception delivery and
resumption behind vectored exception callbacks. Registering callbacks
without ever dispatching hardware faults would be a success stub and is
not an acceptable way to satisfy this import.

## Attempts 6–8: exceptions, file mappings, API-set revisions

| Attempt | New implementation available | Actual next loader failure, needed by mozglue.dll |
|---|---|---|
| 6 | Resumable user CPU exceptions, vectored handlers, RaiseException (`ed8bb48`, [user-exceptions.md](../user-exceptions.md)) | kernel32.dll!CreateFileMappingW does not exist |
| 7 | File mappings (`cd6aeb9`) | api-ms-win-core-file-l1-1-0.dll has no host |
| 8 | Older kernel32/ntdll/advapi32 contract revisions in the schema | kernel32.dll!GetProcessTimes does not exist |

Every attempt: QEMU exit 33, winrun 126, no kernel panic. Evidence:
`build/zen/attempt-{6,7,8}.serial.log`, `.result.json`, `.dependencies.json`.

File mappings: page-file sections are shared OSyx memory; file-backed
sections give read-only, execute-read and copy-on-write views at 64 KiB
offsets, filled from the file without moving its file pointer.
VirtualQuery reports them as MEM_MAPPED. Writable shared file views
and SEC_IMAGE need kernel page-cache sharing and fail with
ERROR_NOT_SUPPORTED. The `mapping.exe` fixture passed 34/34 on Windows and
on OSyx; the Windows QEMU suite passed 348/348. The full regression after
the exception work was green: desktop 34/34, VirtIO-GPU 38/38, Win32 312/312,
network 29/29 (rerun alone after an interactive build replaced
build/osyx.img mid-run; the exit-code boot was rerun for the same reason).

Contract revisions added to the schema (file-l1-1-0, heap-l2-1-0,
interlocked-l1-1-0, localization-l1-2-0, processenvironment-l1-1-0,
realtime-l1-1-1, sysinfo-l1-1-0, rtlsupport-l1-1-0, registry-l1-1-0) only
name the DLL that hosts them on Windows. Functions those DLLs lack still
fail as missing exports; nothing is bound to a placeholder.

After attempt 8 the static startup closure has **412** unresolved import
occurrences (168 missing exports, 241 unmapped contracts, 3 missing DLLs).
The largest remaining block is the UCRT (`api-ms-win-crt-*`: stdio,
printf family, locale, math, string, runtime startup), which needs a real
C runtime implementation. The other blocks: ntdll Rtl string/heap/lock
wrappers and unwinding (RtlLookupFunctionEntry, RtlVirtualUnwind,
RtlCaptureStackBackTrace), the registry, thread pools, job objects,
process creation, FLS, SList and locale APIs, and shlwapi paths.

## Attempt 9: mozglue fully linked

New in `84d781e`, all exercised by `runtime.exe` (55/55 on Windows, 56/56 on
OSyx; Win32 QEMU suite 406/406):

- `rtl.rs`: Rtl heap calls on the process heap, counted strings, interlocked
  SLists on the x64 header layout (CMPXCHG16B), Encode/DecodePointer with a
  per-process secret from /dev/random, NT names of SRW/run-once/condition
  variables.
- `unwind.rs`: x64 table-based unwinding over the images' `.pdata` and
  RtlAddFunctionTable tables, RtlVirtualUnwind including epilogue
  detection, chained unwind info and machine frames, RtlCaptureContext,
  RtlCaptureStackBackTrace, GetThreadContext for the calling thread only
  (OSyx cannot stop another thread to read its registers).
- `registry.rs`: persistent HKLM (`/etc/osyx/registry/machine.reg`) and
  HKCU (`$HOME/.osyx-registry/user.reg`) hives, atomic rewrite, per-handle
  access rights, volatile keys, RegGetValueW type filtering and expansion.
- `sysext.rs`: GetSystemTime/SystemTimeToFileTime, unbiased interrupt
  time, GetProcessTimes (scheduler samples; OSyx has no kernel/user split,
  so all CPU time is reported as user time), VerSetConditionMask and
  VerifyVersionInfoW against the reported 10.0.19041, IsBadReadPtr probing
  through a kernel copy that returns EFAULT, SearchPathW, FormatMessageA/W
  (system text in English for the errors OSyx reports; message tables of
  PE modules are not read). WER registration returns E_NOTIMPL.

Actual result (QEMU exit 33, winrun 126, no kernel panic):

```
API set api-ms-win-crt-runtime-l1-1-0.dll (needed by vcruntime140.dll) has no host
```

mozglue.dll has no unresolved static imports left. The startup closure has
336 unresolved occurrences: 239 UCRT (`api-ms-win-crt-*`, a separate
work item) and 97 others (zen.exe 55, msvcp140.dll 31, vcruntime140.dll 6,
vcruntime140_1.dll 5): FLS, RtlUnwind/RtlUnwindEx and frame-based exception
dispatch for C++ exceptions, thread pools, locale and file information
APIs, job objects and process creation.

No fallback browser, mock UI or success-returning API placeholders were
introduced. This is an incomplete Windows compatibility port, not a
working Zen Browser release. Completing it requires implementing and
testing the runtime/process/security/graphics surfaces above, then real
browser interaction and verified HTTPS tests.

## After attempt 9: C++ exceptions and the newer Win32 surface

`f4c2f51` added FLS with thread-exit callbacks, frame-based SEH dispatch
through language handlers, RtlUnwind/RtlUnwindEx and unwind consolidation
(`seh.exe`). The next commit covers every non-UCRT static import of
zen.exe and msvcp140.dll listed by attempt 9 (86 occurrences):

- `files.rs`: CreateFile2, CopyFile2/CopyFileW, FindFirstFileExW,
  Get/SetFileAttributes(Ex)W, Get/SetFileInformationByHandle(Ex)
  (basic, standard, name, id, end of file, disposition, rename).
  Only the read-only attribute is stored (as the owner write bit);
  setting times, hidden/system attributes and hard links fail with
  ERROR_NOT_SUPPORTED.
- `locale.rs`: one locale, en-US, with UTF-8 code pages. Comparison and
  sort keys order by case-folded scalars, then lower before upper case;
  flags needing Windows' sorting tables fail with ERROR_NOT_SUPPORTED.
- `threadpool.rs`: the default pool (work, timers, waits, registered
  waits, FreeLibraryWhenCallbackReturns) on on-demand Windows worker
  threads, and completion ports as queues (no overlapped file I/O).
- `process.rs`: CreateProcessW (PE children under a child winrun, native
  programs directly), waitable process handles with exit codes, job
  objects (kill-on-close, accounting, process lists), attribute lists,
  and the "remote" memory calls on the calling process only: OSyx gives a
  process no access to another's memory, so other targets fail with
  ERROR_ACCESS_DENIED.
- `shlwapi.rs`: shlwapi.dll path helpers and combase.dll WinRT strings;
  api-ms-win-core-winrt(-string)-l1-1-0 now map to combase. No runtime
  classes are registered, so activation reports REGDB_E_CLASSNOTREG.
- `sysext.rs`: processor topology (one CPU), mitigation policies, handle
  counts, system/Windows directories, ExpandEnvironmentStringsW,
  SetDllDirectoryW/SetDefaultDllDirectories (they change the loader's
  search list), CreateSemaphoreExW, NtQueryObject.

The `platform.exe` fixture exercises them: 96/96 on Windows and 96/96 on
OSyx; the Win32 QEMU suite passed 522/522. Its first OSyx runs found
three bugs, now fixed: the read-only attribute did not stop root from
opening a file for writing, LCMapStringEx left out the terminator of a
NUL-terminated source, and CreateProcessW passed console streams to the
kernel as handles 0-2 (they are kernel handles only when winrun's own
stdio is redirected) and gave the child the native instead of the Win32
environment.

The UCRT is developed separately (branch `zen-ucrt`); until it lands, Zen
still stops at `api-ms-win-crt-runtime-l1-1-0.dll (needed by
vcruntime140.dll) has no host`, so no new Zen attempt was run.

## Attempts 10-11: the Universal C Runtime

`compatibility/win32/src/ucrt/` is a built-in `ucrtbase.dll` (398
functions): heap, strings and ctype, conversions, runtime start-up and
exit, threads, environment, file system, time, math, `rand_s`, the C and
C.UTF-8 locales, low-level I/O, FILE streams with the buffer protocol
msvcp140 uses, and the `__stdio_common_*` printf/scanf family. The eleven
`api-ms-win-crt-*-l1-1-0` contracts map to it. Unsupported variants
(Unicode stream encodings, other code pages) fail with EINVAL.

winrun is built soft-float (x86_64-unknown-none), so Rust passes f32/f64
in integer registers, while Windows code passes and returns them in
xmm0-3. The 38 functions with float arguments or results are exported
through naked trampolines (`xmm_abi!`) that move them between the two.
Without them every math and strtod result was wrong in OSyx while the same
fixture passed on Windows.

The `ucrt.exe` fixture links the real UCRT import library: 242/242 on
Windows and 242/242 on OSyx; the Win32 QEMU suite passed 767/767. Bugs
the OSyx runs found and fixed: the float ABI above, fopen rejecting every
mode (the terminator was parsed as a mode character), setvbuf dropping a
"w" stream's write mode, and strtol("0x") converting a "0".

The static startup closure now has **0** unresolved imports (337 crt
imports remain in DLLs loaded later: xul 87, onnxruntime 46,
Microsoft.WindowsAppRuntime 49, dxcompiler 40, nss3 22 and others).
Attempt 10 passed the loader and ran zen.exe's start-up code until an
unhandled 0xC06D007E (delay-load module not found). winrun now names the
DLL and function of a failed delay load; attempt 11:

```
winrun: delay load failed: SHELL32.dll!SHGetKnownFolderPath
```

QEMU exit 33, no kernel panic. Evidence: `build/zen/attempt-{10,11}.*`.
The next requirement is shell32's known folders, with paths that exist on
OSyx.

## Attempt 12: shell32 known folders

`shell32.rs` implements SHGetKnownFolderPath and SHGetFolderPathW. The
profile is $HOME with the Windows layout below it (AppData\Roaming,
AppData\Local, AppData\LocalLow, Documents, Downloads, ...); machine
folders are the C:\ paths winrun already reports. A missing folder is
HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND) unless KF_FLAG_CREATE /
CSIDL_FLAG_CREATE creates it or KF_FLAG_DONT_VERIFY skips the check;
other users' folders (a non-NULL token) are not supported. `ole32.rs`
provides the task allocator the returned path is freed with; COM itself
is not provided. The `shell.exe` fixture passed 13/13 on Windows (the
OSyx-only missing-folder checks are skipped there) and 16/16 on OSyx; the
Win32 QEMU suite passed 785/785.

Attempt 12 got past the known-folder lookup and stopped at:

```
winrun: delay load failed: ADVAPI32.dll!GetSecurityInfo
```

(0xC06D007F, procedure not found; QEMU exit 33, no kernel panic.)
zen.exe's remaining delay imports from advapi32 are its sandbox set:
object security (Get/SetSecurityInfo, Get/SetNamedSecurityInfoW,
SetEntriesInAclW, SetKernelObjectSecurity), restricted and impersonation
tokens (CreateRestrictedToken, DuplicateTokenEx, SetTokenInformation,
SetThreadToken, OpenThreadToken, RevertToSelf, AdjustTokenPrivileges,
CreateProcessAsUserW), AccessCheck and the event log. They need a real
model of object security descriptors and restricted tokens on OSyx.

## winmm timers

`winmm.rs` provides timeGetTime (the monotonic clock in milliseconds),
timeBeginPeriod/timeEndPeriod and timeGetDevCaps, which zen.exe and
mozglue delay-import. The OSyx kernel ticks at a fixed 100 Hz, so the
reported minimum period is 10 ms; finer requests (such as the usual
timeBeginPeriod(1)) return TIMERR_NOCANDO rather than claiming a
resolution OSyx does not have, and timeEndPeriod must match an earlier
timeBeginPeriod. The `winmm.exe` fixture passed 7/7 on Windows and 10/10
on OSyx; the Win32 QEMU suite passed 797/797. No new Zen attempt was
run: Zen still stops first at the advapi32 delay import of attempt 12.

## Validation

Host parser/loader tests (including resource bounds, malformed delay
descriptors and mutation sweeps) and clippy passed. The version.exe fixture
also ran on the host Windows system: 10 checks passed, 0 failed, exit 0.

Final `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test.ps1`
completed with exit 0 and `All tests passed`. Evidence:
`build/zen/full-regression.log` (SHA-256
`fb0d9a9fff4abb98e58f4b73b290d21a61d6b38789fe5665f159419f9740888b`).

| Final regression component | Actual result |
|---|---|
| Host unit tests | 352 passed |
| Clippy host, kernel, loader and userspace | Passed with -D warnings |
| Kernel self-tests in QEMU | 25 passed, 0 failed |
| Desktop, PRISM and existing applications | 34 passed, 0 failed |
| Native network | 29 passed, 0 failed |
| Desktop on VirtIO-GPU | 38 passed, 0 failed |
| Windows compatibility integration | 198 passed, 0 failed |
| Included version.exe / synch.exe assertions | 10 / 35 passed, 0 failed |

The complete script emitted 330 PASS records and 0 FAIL records; those
records include component summaries and are not 330 additional independent
unit tests. Filesystem crash recovery, process teardown, controlled faults,
USB input and screenshot checks also passed. The PE audit example's two
unit tests passed separately with `cargo test -p osyx-pe -p osyx-pe-loader
--all-targets`. Clippy for the final timer security/access checks was rerun
successfully before the final QEMU Windows test.

Local implementation commits: `5f36f10` (official payload/probe),
`d54f3fd` (PE resources/delay-import audit), `637da3a` (real compatibility
extensions and Windows regression fixtures). No OS version change or
browser launcher/default registration is included.

Implementation references: [PE/COFF format](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format),
[condition variables](https://learn.microsoft.com/en-us/windows/win32/sync/condition-variables),
[SleepConditionVariableSRW](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-sleepconditionvariablesrw).

## GDI surfaces: attempts 47 and 48 (2026-10-24)

The baseline was main at 2ec93b7, with attempt 46 blocked on
`GDI32.dll!CreateDCW`. The worklist was the 46 gdi32 gaps imported by xul.dll
in `build/zen/dependencies.json`; all 46 now have runtime exports, including
WidenPath. The audit after attempt 47 reports no gdi32 gap for xul.dll.
No Windows binary or Zen payload is committed.

The new software GDI layer provides DCs, DIB storage and format conversion,
bitmap selection, pixel copies to PRISM, rectangle-list regions, paths,
DC snapshots, matrix state and bitmap-font queries. OpenGL entry points
exist and explicitly report unavailable support. Current software limits are
recorded in [windows-compatibility.md](../windows-compatibility.md).

Attempt 47 used `scripts/zen-test.ps1 -SkipFetch`. Evidence in
`build/zen/qemu.serial.log` (also preserved as `attempt-47.serial.log`):

```
[   15.800] [prism:3] prism: window 3 "nsAppShell:EventWindow" created (Normal, client 2)
[   16.450] [winrun:5] winrun: delay load failed: SHELL32.dll!GetCurrentProcessExplicitAppUserModelID
[   16.470] [prism:3] prism: client 2 (process 5) disconnected; released 1 window(s), 1 surface(s), 0 buffer(s), 0 KiB of pixels
```

Zen passes CreateDCW but still draws no pixels. The captured screen is
`build/zen/attempt-47.png`; only the desktop and the auxiliary event-window
entry appear. Probe result: exception 0xc06d007f, VM exit 33, no kernel panic,
BLOCKED. The script's exit 2 remains intentional. Tabs, page rendering,
keyboard/mouse and HTTPS are unverified. The next worklist remains the
`gaps` array in `build/zen/dependencies.json`, filtered by importer xul.dll;
the next reached failure belongs to shell32, outside this GDI round.

Attempt 48 reproduced the same shell32 blocker (serial time 15.920 s), with
one auxiliary window and zero pixel buffers. `zen-test.ps1` returned 2;
`build/zen/qemu-result.json` remains blocked. Its captured screen is
`build/zen/attempt-48.png`.

The final gdi2 fixture passed 156/0 first on native Windows and then in QEMU.
It covers each added export and asserts BITMAP, BITMAPINFOHEADER,
DIBSECTION, XFORM, RGNDATAHEADER, LOGBRUSH, GLYPHMETRICS, MAT2, GLYPHSET
and PIXELFORMATDESCRIPTOR layouts. The complete Win32 suite passed 1508/0
(`build/wintest.log`, `build/test/win.serial.log`); clippy passed with
`-D warnings` for x86_64-unknown-none using target/claude-check.

Additional clipping checks cover SetPixel in a complex region and a clipped
monospace glyph. Native test host: Windows 10.0.26300.

The GUI fixture copies a green 16x16 DIB into the PRISM window and verifies
the resulting pixel before EndPaint. The screenshot is
`build/test/win-gui.png` and the GUI fixture reports 17/0.
