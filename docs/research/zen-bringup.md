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

Run `powershell -ExecutionPolicy Bypass -File scripts/zen-test.ps1 -MemoryMiB 8192 -TimeoutSec 900`.
It verifies/downloads the pinned package, builds a 1 GiB ext4 root image,
installs the full payload in `/opt/zen`, then boots QEMU. The default RAM
setting is 6144 MiB; recent functional probes use 8192 MiB. Init starts
PRISM and invokes `/bin/winrun -v /opt/zen/zen.exe --no-remote -profile
C:\tmp\zen-probe-profile-<timestamp> file:///C:/tmp/zen-probe-page.html
-no-deelevate`. Each run creates a fresh profile with software rendering
and first-run welcome URLs suppressed. The temporary HTML page has a button
that changes its rectangle, label and title, plus a link to example.com.
Use `-ProbeUrl https://example.com/` for a separate external-page probe and
`-SkipFetch` to reuse the verified vendor payload. `-SkipBuild` reuses the
image's existing URL and cannot be combined with an explicit `-ProbeUrl`.
The selected URL is logged and copied into the probe result. The browser's
own sandbox-disable switches remain necessary for this bring-up configuration.

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

## Explicit process AppUserModelID (2026-10-24)

Attempts 47/48 stopped at shell32 GetCurrentProcessExplicitAppUserModelID.
Implement that export and its setter from the xul.dll shell32 gap list.
The metadata is process-wide, separate from COM apartments, and queries
allocate independent UTF-16 copies through the task allocator. It starts
unset in a new winrun process. This does not add PRISM taskbar grouping.

Native observations on Windows 10.0.26300, using the public APIs in a fresh
console process (`build/appid-native-probe.log`, then `shell.exe`):

| Operation | Observed result |
|---|---|
| Get before an explicit set | E_FAIL (0x80004005), output cleared |
| Get after setting an ID | S_OK, independent CoTaskMemFree-owned copy |
| Set empty string | S_OK, removes the explicit ID |
| Set NULL | E_INVALIDARG, also removes the old ID |
| Set 127 UTF-16 units | S_OK |
| Set 128 UTF-16 units | E_INVALIDARG, old ID preserved |

Spaces and Unicode are preserved verbatim. Setters copy their input; callers
can modify or release it after return. Another thread sees and can replace
the process ID; a child process starts unset and cannot change the parent's
ID. Calling the native getter with a NULL output slot caused an access
violation in the isolated exploratory probe; the regression fixture only
passes valid output slots. The OSyx implementation rejects NULL safely.

The fixture passed 36/0 on native Windows, plus 3/0 in its child process,
before QEMU testing. It verifies copy ownership, boundary lengths, clearing,
last-error preservation, thread sharing and process isolation. Static
assertions cover WCHAR/HRESULT/GUID and the child-launch structure sizes.

API contracts: [GetCurrentProcessExplicitAppUserModelID](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-getcurrentprocessexplicitappusermodelid),
[SetCurrentProcessExplicitAppUserModelID](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-setcurrentprocessexplicitappusermodelid).

Attempt 49 (`zen-test.ps1 -SkipFetch`) passed the previous shell32 failure
and stopped at `ole32.dll!OleInitialize`, serial time 17.600 s. Evidence is
preserved in `build/zen/attempt-49.serial.log` and `attempt-49.result.json`;
the screen is `attempt-49.png`. The script returned 2, with no panic and
no browser pixel buffer. The audit has 1029 unresolved occurrences, three
fewer than attempt 48 (the two APIs are imported by xul.dll and zen.exe).

## OLE apartment initialization (2026-10-24)

Attempt 49 reached OleInitialize. The new OleInitialize/OleUninitialize
exports acquire and release COM STA references, with an independent OLE
nesting count for each thread. This permits existing COM callers to keep
their own references, and rejects an OLE call in an explicit MTA with
RPC_E_CHANGED_MODE without changing either count. Invalid reserved input
returns E_INVALIDARG. This adds apartment setup, not clipboard, drag/drop
or OLE class servers.

The exploratory public-API probe on Windows 10.0.26300
(`build/ole-init-native-probe.log`) showed that the first OLE call in an
already initialized COM STA returns S_OK, not the S_FALSE returned by a
second CoInitializeEx call. Repeated OLE calls return S_FALSE. Each
successful call acquires a COM reference; OleUninitialize without an OLE
reference leaves an independently initialized COM apartment intact.

`ole2.exe` passed 15/0 natively before QEMU testing, checking these counts,
MTA conflicts and thread isolation. It is a separate fixture because the
unchanged native interop.exe baseline has three window-station failures on
this host (`build/interop-baseline-native.log`: 61/3). The new apartment
tests do not depend on that host-specific station policy. No existing
interop assertion was removed or weakened.

API contracts: [OleInitialize](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-oleinitialize),
[OleUninitialize](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-oleuninitialize).

## Window creation, input and the first child process (attempts 50-58, 2026-10-24)

Each attempt was `zen-test.ps1 -SkipFetch`; each ended with script exit 2,
no kernel panic and no browser pixels. Every API below was first observed
on Windows 10.0.26300 with a public-API probe (`build/probe/*.c`) and its
fixture passed natively before the QEMU run.

| Attempt | Stopped at | Added |
|---|---|---|
| 50 | SHELL32!SHGetPropertyStoreForWindow | OleInitialize passed |
| 51 | dwmapi!DwmSetWindowAttribute | window property stores; MsgWaitForMultipleObjects(Ex), real GetQueueStatus |
| 52 | USER32!GetKeyboardLayout | dwmapi, imm32 (no IME), wtsapi32 |
| 53 | USER32!EnumDisplayDevicesW | US keyboard layout from Windows' tables; key messages now carry set-1 scan codes and the extended bit |
| 54 | ole32!CLSIDFromString | displays, capture, mouse tracking, icons, clipboard, touch/raw-input registration |
| 55 | MOZ_RELEASE_ASSERT(result.isOk()) in xul.dll | GUID strings, drag-drop registration, OLE clipboard, MTA usage; no-GPU d3d11/dxgi/d3dcompiler |
| 56 | same; failure trace showed CreateFileMapping refusing a descriptor | `-v` keeps the last 32 API failures (source location, error, thread) and prints them with an unhandled exception |
| 57 | same | WaitOnAddress for 1, 2 and 8 bytes (Rust's parker spun on ERROR_NOT_SUPPORTED) |
| 58 | int3 in zen.exe (sandbox broker), thread 42, after SetHandleInformation(HANDLE_FLAG_INHERIT) | section and directory descriptors |

Message waits no longer hold the GUI lock while sleeping; a post wakes
waiters at once, and PRISM input is polled every 10 ms while idle.

Attempt 58 is the first time Gecko launches a child process: the IPC pipe
`winpipe.gecko.*` is listening and the broker marks a handle inheritable.
OSyx winrun has no handle inheritance, and events, mutexes and semaphores
exist only inside one process. The next stage is cross-process handles
(inheritance and PROC_THREAD_ATTRIBUTE_HANDLE_LIST, shared sections and
events) and the Chromium sandbox broker's requirements (jobs, restricted
tokens, alternate desktops), or launching children without the sandbox.
DirectWrite is absent (`dwrite.dll` not found), so Gecko also needs real
TrueType fonts behind its GDI font path before text can render: the
atlas fonts have no sfnt tables (GetFontData fails).

## Inherited handles, DirectWrite and private JIT pages (2026-10-24)

The work continued from attempt 58 with cross-process inheritance of
events, page-file sections and connected pipes, an explicit handle list,
and DuplicateHandle transfers in both directions. Shared event state uses
shared memory; the kernel keys a futex in such a mapping by the shared
object and byte offset rather than by process and virtual address.
Files, mutexes and semaphores cannot yet be inherited. The diagnostic
launcher uses Mozilla's sandbox-disable environment switches because the
Windows broker's suspended-process patching is not implemented.

The new DirectWrite subset provides the original factory and font/face
interfaces, local and custom font loaders, metrics, cmap lookup, sfnt
tables, TrueType outlines and grayscale glyph coverage. Build scripts
install the unmodified OFL-licensed Lato and KaTeX fonts in C:\Windows\Fonts
and seed HKLM FontSubstitutes. Text layout/analysis, CFF outlines, hinting
and newer factory interfaces are unsupported. GDI's existing atlas font
model remains separate; this does not turn GetFontData into a TrueType API.

On resumption, attempt 77 crashed at mozavutil.dll+0x26be0 after a failed
load reporting the missing kernel32 GetProcessAffinityMask. The one-CPU
affinity query is now implemented. PE loads now roll back newly loaded
dependencies and their reservations when binding fails, preventing a later
LoadLibrary from finding a partially bound image and calling its entry.
Two host regression tests cover repeated failure, repair and retry, and
preservation of modules loaded before the failed operation.

| Attempt | Configuration and observed result |
|---|---|
| 78 | 4 GiB; passed mozavutil; reached missing USER32 GetWindowDC. Content processes crashed with MOZ_CRASH(OOM). |
| 79 | Added GetWindowDC; main process reached OLEAUT32 ordinal 200 (GetErrorInfo). Content failures came from VirtualAlloc(..., MEM_COMMIT, PAGE_EXECUTE_READWRITE), not exhausted RAM. Serial/result preserved as attempt-79.*. |
| 80 | Added Get/SetErrorInfo (including ordinals 200/201) and private JIT page support. No delay-load exception; the VM exhausted its actual 4 GiB at guest time 163.430 s. PRISM counted one buffer / 50 KiB, from an empty window. Screenshot attempt-80-live.png; full serial/result attempt-80.*. |
| 81 | Same image with 6 GiB, 240-second timeout. No unhandled Windows exception or memory exhaustion; remained alive with a tiny blank/black Zen window. Screenshot, serial and result are attempt-81.*. No browser UI or page rendering confirmed. |
| 82 | Separate profile with software WebRender; progressed to USER32 wsprintfW. Added ANSI/wide wsprintf and wvsprintf adapters over the CRT formatter; native message fixture passed 59/0 including owner and extended-style queries. |
| 83 | Software compositor preferences, wsprintf and initial taskbar grouping; 6 GiB, 300-second timeout. No loader error, unhandled Windows exception or fatal process fault. Functional UI still unverified; result state is unverified, not success. |
| 84 | Final hidden/owned window handling and task grouping; 6 GiB, 180-second timeout. Screenshot attempt-84.png confirms exactly one Zen Browser task and no helper-window tasks. The main window stays maximized at 1920x994 instead of shrinking to 160x80. No loader error or fatal process fault; browser content is still blank. Serial/result/screenshot preserved as attempt-84.*; live intermediate screenshot attempt-84-live.png. |

Private PAGE_EXECUTE_READWRITE pages are initially RW/NX. On instruction
fetch, the fault dispatcher switches that page to RX; a subsequent write
switches it back to RW/NX. The original CPU context is resumed, with no
Windows exception delivered for these internal permission transitions.
VirtualQuery reports the requested logical RWX protection. Kernel W^X is
unchanged. Same-page self-modifying instructions and writable executable
file/shared sections remain unsupported. The threads fixture tests code
generation, execution, patching, explicit RX, subrange commit, decommit and
zeroed recommit. Native Windows passed 39/0.

GetErrorInfo/SetErrorInfo keep a retained COM object per thread, transfer
ownership when retrieved, release replaced objects, and expose both named
and ordinal exports. Windows also exports the functions through ole32;
those aliases share the same state. The native interop fixture passes the
new nine error-info checks; its existing three window-station failures on
this Windows host remain (70 passed, 3 failed overall).

The font and inheritance fixtures passed native Windows first:
dwrite 83/0, childproc 18/0. The first resumed QEMU regression passed
1960/0, including dwrite 83/0 and the initial JIT execution/patch checks.
The final targeted Win32 rerun passed 1993/0, including threads 39/0,
msgwait 61/0, gui 20/0, dwrite 83/0 and childproc 18/0. Host unit tests
passed 359/0 and all target clippy checks passed with -D warnings.
The complete QEMU system run passed boot, kernel diagnostics, storage,
recovery, IPC, desktop (34/0), networking (29/0) and VirtIO-GPU desktop
(38/0). Its Win32 stage exposed a missing lstrcmp import used only by the
new fixture; replacing that dependency with a local exact comparison and
rerunning win-test.ps1 produced the final 1993/0 result. Evidence is
build/codex-resume-host-final.log, build/codex-resume-qemu-final.log and
build/codex-resume-win-final.log (the latter supersedes the earlier Win32
stage, not the independent system checks).
The host tests need a writable TEMP/TMP; this session used
build/host-test-tmp after Windows denied directory creation in its default
temporary folder. No production host filesystem code was changed.

The diagnostic probe now creates C:\tmp\zen-probe-profile with explicit
software WebRender preferences. Direct3D acceleration and its GPU process
are disabled for this probe; the browser binaries remain unchanged. The
VM memory limit is configurable via zen-test.ps1 -MemoryMiB (default 6144).
Result JSON now distinguishes actual loader/Windows exceptions and fatal
process faults from expected feature-probing API failures, and records
timeouts. A timeout, a mapped window or an empty pixel buffer is not
functional browser success.

### One application entry for Windows subprocesses

PRISM now excludes unmapped and explicitly auxiliary windows from its
task list. Winrun creates its windows hidden, forwards ShowWindow and
SWP_SHOWWINDOW/SWP_HIDEWINDOW visibility, and classifies message-only,
tool, nonactivating and owned windows as auxiliary. Drawing to a hidden
helper no longer maps an accidental visible window. Runtime changes to
extended styles update taskbar eligibility. Owned windows keep their
PRISM parent and expose GW_OWNER/GA_ROOTOWNER to Windows callers.

The desktop groups Windows windows by their highest winrun ancestor in
the kernel's process snapshot. It prefers the named parent window as the
task button's target, keeps other applications separate and retains a
child entry if the parent has no window. Focusing an owned helper marks
the parent's entry active; minimizing an application retains its task.
Processes remain independent. This is window ownership and taskbar
grouping, not a merger of Firefox's process address spaces.

Host tests cover separate process families, named parent selection,
missing parents, hidden-window pixels/focus/task lists, auxiliary windows,
owned-window focus and minimized entries. SetWindowPos also updates its
rectangle before returning; the GUI fixture checks immediate window and
client rectangle queries so callers do not have to wait for a PRISM
Configure event to observe their requested geometry. The message fixture
also verifies that destroying an owner destroys its helpers (61/0 native
Windows and QEMU).

Attempt 84's result remains unverified (script exit 2, timed_out true,
loader_error and process_fault null). Tabs, browser input, rendered web
content and verified HTTPS have not passed. The dependency audit has 894
unresolved import occurrences across the complete vendor package; unused
gaps are not treated as reached runtime failures.

API contracts and implementation references:
[GetProcessAffinityMask](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getprocessaffinitymask),
[GetErrorInfo](https://learn.microsoft.com/en-us/windows/win32/api/oleauto/nf-oleauto-geterrorinfo),
[SetErrorInfo](https://learn.microsoft.com/en-us/windows/win32/api/oleauto/nf-oleauto-seterrorinfo),
[VirtualProtect](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualprotect),
[Mozilla GPU process configuration](https://github.com/mozilla-firefox/firefox/blob/main/gfx/thebes/gfxPlatform.cpp),
[Mozilla WebRender configuration](https://github.com/mozilla-firefox/firefox/blob/main/gfx/config/gfxConfigManager.cpp).

## Continued rendering investigation (attempts 85 onward)

The first rendering fixes address observable Windows contracts rather than
browser-specific return values. IsIconic previously returned true for every
valid window, causing Gecko's occlusion tracker to classify normal windows
as hidden. Minimized/maximized style bits, IsZoomed, normal placement and
restore-to-maximized state now follow ShowWindow and PRISM acknowledgements.
PRISM reports minimize/restore transitions to its client; stale Configure
states cannot overwrite a newer pending request. GetWindowRgn reports the
absence of an explicit region without modifying the caller's region.

Message retrieval now respects HWND/range/PM_QS filters, preserves
PM_NOREMOVE ordering and generated timers, and delivers quit independently
of filters. GetUpdateRect exposes invalidated areas. SystemParametersInfo
provides ANSI/Unicode nonclient metrics and icon-title LOGFONTs using the
installed Lato family, including legacy structure-size bounds.

A full regression exposed an independent SLIST assembly defect. The
cmpxchg16b success output could be allocated in BL and then overwritten
while restoring RBX. This could spin forever or accept a failed CAS. The
replacement uses explicit, separate registers for the address, replacement
low word and success byte. The runtime fixture varies RBX seeds and pushes
512 distinct entries from four threads, checking register preservation,
depth and exactly-once retrieval. Its six new checks pass on Windows and
OSyx; the complete OSyx runtime fixture passes 62/0. The native runtime
fixture still has 15 pre-existing HKLM permission failures on this host.

Gecko's FrameState also depends on WM_WINDOWPOSCHANGING/CHANGED. SetWindowPos
now supplies live WINDOWPOS callbacks and applies callback changes before
updating geometry. DefWindowProc derives WM_SIZE/WM_MOVE from CHANGED, so
consuming it suppresses those messages. PRISM frame notifications are
delivered when the owning thread pumps messages, outside the GUI mutex.
Seven native reference checks cover callback order, mutation, thread,
SWP_NOSENDCHANGING, consumed CHANGED and ShowWindow state.

Verification through attempt 94: host tests 360/0, all target clippy checks
passed; native msgwait 122/0; full QEMU Win32 suite 2060/0, including
msgwait 122/0 and runtime 62/0. Evidence is build/zen/host-frame-91.log,
arranged-native-94.log and win-arranged-94.log. Earlier compositor/desktop checks
passed 35/0 host and 34/0 QEMU respectively.

The probe uses a fresh profile per boot and a real local HTML page with a
colored rectangle and a click-verification button. It removes old
screenshots at the start so an early failure cannot reuse an earlier image.
Attempt 90 (8 GiB, 360 seconds) had no unhandled loader exception, process
fault or panic, but only a tiny unusable browser window. Attempts 91/92
reach the new frame callbacks, then Gecko's protected window procedure
terminates the process with code 253. A null loader_error in the result
does not prove absence of a caught exception. Verbose process-exit traces
and bounded exception-dispatch logs are used to investigate that path.
Attempt 93 identifies the caught delay-load exception as
USER32!IsWindowArranged. That query is now exported: PRISM has no Snap
arrangement, so valid windows are unarranged; maximization is a separate
state. The four added reference checks pass natively and in QEMU.
Attempts 94/95 get past that exception and run to the 360-second timeout
without a fatal process fault or panic. They still shrink after Gecko
requests zero dimensions, and do not display the test page. The child
renderer reports "Failed to connect WebRenderBridgeChild"; the next probe
adds document-loader and IPC logs and a longer observation interval.
The final package audit for these attempts has 889 unresolved occurrences.
Functional page rendering, tabs, browser input and HTTPS remain unverified.

References: [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos),
[IsWindowArranged](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-iswindowarranged),
[Mozilla nsWindow frame state](https://github.com/mozilla-firefox/firefox/blob/main/widget/windows/nsWindow.cpp).

### Monitor device names and the zero-size welcome window

Attempt 96's document/IPC logs show the real browser chrome loading from
omni.ja. Inspecting the unmodified vendor ZenWelcome.mjs explains the
subsequent resize: its welcome screen uses min(1200, screen.availWidth)
and min(720, screen.availHeight), then locks both dimensions with CSS.

GetMonitorInfoW previously populated only the 40-byte MONITORINFO prefix
even for a 104-byte MONITORINFOEXW. Gecko's ScreenHelperWin passes the
missing szDevice to CreateDC, which failed with ERROR_NOT_SUPPORTED.
It consequently registered no usable screen, making the welcome dimensions
zero. The implementation now fills the adapter name for MONITORINFOEXW and
the 72-byte ANSI variant, validates structure sizes and preserves output
boundaries. The basic 40-byte query remains bounded to that structure.

Eight new native checks exercise both names through CreateDC, bounds,
ANSI/Unicode agreement, canaries and invalid sizes. Native gdi2 passes
166/0; the full OSyx Win32 suite passes 2068/0, including gdi2 166/0.
Targeted clippy passes with -D warnings. Evidence is
build/zen/monitor-native-97.log, win-monitor-97.log and
clippy-monitor-97.log. Attempt 96 was manually stopped after this diagnosis;
its zero VM exit code is not a browser exit or functional pass.

Attempt 97's WidgetScreen log confirms a real 1920x1080 screen at 96 DPI
and 60 Hz, and the initial browser size changes to 1280x972. Functional
rendering is still being checked. Implementation reference:
[Mozilla ScreenHelperWin](https://github.com/mozilla-firefox/firefox/blob/main/widget/windows/ScreenHelperWin.cpp).

### SQLite shared file sections

Attempts 97-99 keep a correctly sized browser window, but the client area
remains blank. Attempt 99 compares the same surface pixels before and after
the GDI copy, in PRISM and in the screenshot: the purple first row and gray
interior agree. This rules out the suspected shared-buffer copy failure.
Enabling the browser console instead reveals SQLite disk I/O errors and
NS_ERROR_STORAGE_IOERR during profile and session initialization. The shim
rejects PAGE_READWRITE mappings of the SQLite -shm files with error 50.

File sections now use a kernel cache keyed by mounted volume and inode.
Independent sections, inherited handles and views in different processes
alias the same frames. Growing a section retains frames already visible
through earlier views. Kernel section capabilities enforce read/write
rights, including against attempts to use generic SHM_MAP on a file section.
Copy views stay private, view references retain backing after handle close,
and FlushViewOfFile writes the selected range without changing the source
file position. A kernel worker writes and retires unused backing outside
process locks. Explicit truncation rejects live backing and invalidates an
unused cache entry, preventing old pages from reappearing after CREATE_ALWAYS.

The existing 64 MiB kernel shared-memory limit also bounds coherent file
sections. Larger immutable resources retain the previous snapshot fallback;
larger writable sections and SEC_IMAGE remain unsupported. Named section
lookup remains process-local. FlushViewOfFile does not promise physical
storage durability; callers can follow it with FlushFileBuffers.

Native Windows fixtures pass mapping 45/0 and childproc 18/0. The full OSyx
Win32 suite passes 2080/0, including direct kernel checks on tmpfs and ext4
for rights, bounds, aliases, deferred writeback and truncation. Host tests
pass 360/0 and all clippy targets pass with -D warnings. Evidence:
build/zen/filemap-native.log, filemap-child-native.log,
win-filemap-102.log and host-filemap-101.log.

Attempt 100 uses a fresh full image containing this kernel. SQLite I/O
errors disappear. The real vendor browser renders its chrome and the local
HTML page, including Lato text, the blue background and green rectangle.
Injected USB mouse input activates the real page's button: its label changes
to Click verified, the rectangle turns orange and the window/tab title
becomes OSyx click verified. Typed input and multiple tabs also work; the
desktop continues to show one task for the whole browser family. Screenshots:
build/zen/attempt-100-render.png, attempt-100-click2.png and
attempt-100-mouse-tab.png. The fresh probe profile skips the welcome screen
and enables console output; vendor binaries and browser assets are unchanged.

External HTTPS remains blocked: example.com resolves, but the channel stops
with NS_ERROR_SOCKET_CREATE_FAILED (0x804b0034), then attempts HTTP fallback.
No certificate bypass was used. The host can reach the same HTTPS URL.
attempt-100.result.json records these manual observations separately from
the conservative probe script defaults: local rendering/input/tabs pass,
https_verified is false, and state is partial. The VM ran to the 900-second
observation timeout without a fatal process fault or kernel panic. Bounded
Winsock diagnostics and socket-transport logs are the next investigation.

References: [MapViewOfFile](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-mapviewoffile),
[UnmapViewOfFile](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-unmapviewoffile),
[FlushViewOfFile](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-flushviewoffile).

### Legacy Winsock ordinals and socket endpoint queries

Attempts 101/103 isolate the generic socket-create error to NSPR's
PR_OpenTCPSocket. The same unmodified vendor nss3.dll passes an eight-check
initialization/TCP/SSL preflight on Windows but fails its TCP descriptor
check on OSyx with PR_UNKNOWN_ERROR and OS error zero. Disassembling the
vendor DLL shows it importing WSOCK32 ordinal 12 for ioctlsocket. OSyx had
used the WS2_32 table for both libraries: ordinal 12 instead returned the
non-null string pointer from inet_ntoa. NSPR interpreted it as ioctl failure.

The DLLs now have separate ordinal tables for the three differing entries:
WSOCK32 10/11/12 are inet_addr/inet_ntoa/ioctlsocket; WS2_32 retains
ioctlsocket/inet_addr/inet_ntoa. Pointer comparisons against both native
Windows DLLs and direct legacy ioctl calls form the regression fixture.
Attempt 104 then passes the same vendor NSS preflight 8/0 in OSyx, including
SSL_ImportFD, hostname setup and client-handshake initialization.

The subsequent endpoint error is independent: getsockname/getpeername
passed zero to the kernel control call and assumed a packed return value,
although the ABI writes a SockAddr into a supplied buffer. Both now pass a
real output structure and validate the Windows buffer and length pointers.
Six reference checks cover listener/local/remote endpoints and invalid
buffers. Native wsock32 passes 34/0; the complete OSyx Win32 suite passes
2094/0, including all 34 checks; targeted clippy passes with -D warnings.
Evidence: build/zen/wsock-endpoint-native-105.log, win-wsock-105.log,
clippy-wsock-final-106.log and attempt-104.serial.log.

Attempt 106 advances through real TLS certificate verification and logs
successful handshakes and HTTP/2 response data from Mozilla services. A
fresh interactive external-page check is in progress. The temporary vendor
NSS preflight hook was removed from init after the diagnosis; the browser
uses its original NSS implementation and certificate verification.

### TCP port-zero binding and probe command-line handling

Attempt 107 reaches a certificate-verified example.com TLS handshake and
HTTP/2 200, including the 334-byte compressed response body. The external
page remains blank at the 900-second timeout; transport success alone is
not a page-rendering pass. Browser chrome, a single browser-family taskbar
entry and delayed URL-bar typing are visible. Preserved evidence is
build/zen/attempt-107.serial.log, attempt-107.result.json and
attempt-107-late.png.

Firefox's pollable event creates a loopback TCP listener with bind(port 0),
queries the assigned port before listen, then connects a second socket.
OSyx previously only remembered the requested endpoint in bind and rejected
port zero in listen. TCP bindings now reserve a stack socket and an actual
dynamic port immediately. That reservation converts in place into a
listener or connection, remains reserved after rejected connect parameters,
and is released when a bound socket closes. Other bindings and connections
cannot reuse the reserved port. Netstat reports the intermediate BOUND state.

A host network regression covers reservation, rejected parameters, bound
connect, loopback transfer and releasing an unused binding. The native
Windows and OSyx wsock32 fixture both pass 42/0, including eight checks for
the browser's dynamic-port loopback signal sequence. The complete OSyx
Win32 suite passes 2102/0; network tests and network/kernel clippy pass.
Evidence: build/zen/net-bind-tests-109.log, net-bind-clippy-109.log,
kernel-bind-clippy-109.log, wsock-bind-native-109.log and win-bind-109.log.

Attempt 108 also exposes a probe startup defect. The original vendor
BrowserContentHandler.sys.mjs warns that -no-deelevate is unknown and skips
the following argument. With one URL it silently drops that URL; with two
it opens only the second. The launcher flag now comes last, and the probe
accepts exactly one requested URL through init's second argument. The local
HTML remains the default, and external HTTPS has its own explicit probe URL.
The unchanged vendor resources establish the argument-skipping behavior;
attempt-108.serial.log records the warning.

Attempts 110/112 then isolate an earlier startup stall exposed by the now
successful socket-pair creation. The socket thread sends its priming byte,
select wakes and recv reads that byte, but the next recv never returns.
Winsock accept must inherit the listener's nonblocking mode; OSyx's native
accept starts in blocking mode. The shim now records successful FIONBIO
changes and applies the listener's mode to each accepted socket. Changes to
the listener after accept do not change the accepted endpoint's saved mode.
The I/O trace is bounded to 96 lines and requires OSYX_WINRUN_TRACE.

The expanded native Windows and OSyx fixtures both pass 51/0. They cover inherited mode,
draining an empty nonblocking reader, and select waking when another thread
sends a signal. The complete OSyx Win32 suite passes 2111/0, recorded in
build/zen/win-inherit-nonblock-113.log. Win32 clippy passes with -D warnings. Preserved diagnostic
evidence is build/zen/attempt-112.serial.log; reference results are in
wsock-inherit-native-113.log and clippy-inherit-nonblock-113.log.

References: Mozilla's [PollableEvent.cpp](https://github.com/mozilla-firefox/firefox/blob/main/netwerk/base/PollableEvent.cpp),
Microsoft's [accept](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-accept).
