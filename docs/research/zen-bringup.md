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

No fallback browser, mock UI or success-returning API placeholders were
introduced. This is an incomplete Windows compatibility port, not a
working Zen Browser release. Completing it requires implementing and
testing the runtime/process/security/graphics surfaces above, then real
browser interaction and verified HTTPS tests.

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
