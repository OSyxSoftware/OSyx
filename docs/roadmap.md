# Roadmap

Status of the milestones from the project brief. Audited against the code
on 2026-10-30.

| # | Milestone | Status |
|---|---|---|
| 1 | Repository, toolchain, scripts, docs | Done |
| 2 | Graphical UEFI boot loader | Done |
| 3 | Kernel boot (serial, panic, CPU init, exceptions) | Done |
| 4 | Memory subsystem (frames, paging, heap) | Done |
| 5 | Initial OSyx Std | Done (convenience std; real `std` port planned) |
| 6 | PE32+ parser | Done (host tested) |
| 7 | User-space foundations (ring 3, syscalls, exit, console) | Done |
| 7b | Scheduler, kernel threads, preemption, FPU state, spawn/wait | Done (SMP) |
| 7c | Threads per process, futexes, FS-base TLS, OSyx Std `thread`/`Mutex`/`Condvar` | Done (SMP) |
| 7d | Storage: PCI, virtio-blk, block layer, FAT32, VFS, file handles, OSyx Std `fs` | Done (completion interrupts with a polling fallback; SMP) |
| 7e | File system architecture: ext4 root (journalled), FAT32 `/boot`, tmpfs, devfs, GPT, permissions, cwd, symlinks, mount/umount | Done (see storage.md for limits) |
| 7f | Native executable loader: ELF64 from the VFS (`/bin`, `/usr/bin`), `execve`, argv/envp/auxv, OSyx Std environment | Done (static `ET_EXEC` only; see kernel.md) |
| 7g | Kernel objects and IPC: pipes, channels, shared memory, wait_handles (ABI v7) | Done |
| 7h | Native xHCI driver, USB enumeration, HID keyboard/mouse, input core | Done (root ports, boot keyboards; see graphics.md) |
| 7i | PRISM compositor, protocol v1, UI toolkit, desktop shell, AXIS Terminal, Files, System Information | Done (software composition; see graphics.md) |
| 8 | Native Windows PE execution | Initial subset implemented (console, files, threads, DLLs, GUI, Winsock; see windows-compatibility.md). Every Steam and Zen import the scanner scores is usable in source scan: Steam 265 implemented and 2 partial of 267, Zen 1476 implemented and 69 partial of 2644 (see Steam and Zen API gaps) |
| 9 | Native networking | Implemented (VirtIO-Net and IPv4 stack; see network/net) |
| 10 | VirtIO-GPU display backend | Implemented (2D scanout/damage/cursor; software composition) |
| 11 | Task Manager, Settings and four PE32+ desktop utilities | Implemented initial versions |
| 12 | Unmodified Windows browser (Zen) on OSyx | Runs and plays YouTube in QEMU; software rendering, Gecko sandbox off (see research/zen-bringup.md, research/performance-profile.md) |
| 13 | Signed system updates and live image | Done (`/bin/osyx-update`, GitHub releases, `scripts/release.ps1`) |
| 14 | Battery indicator (ACPI/EC) | Done in code (multi-battery, hotplug, EC tolerance, AML tests); unverified on most hardware |
| 15 | Kernel profiler and flame graphs | Done (`prof=N`, `scripts/flamegraph.py`) |

## Next steps (in order)

Planned work, roughly by priority. Items marked (in progress) have a branch.

### Near term

1. **USB storage for users**: done. Removable disks are mounted under
   `/media/<label>` on hot-plug (`kernel/src/media.rs`), eject flushes and
   unmounts and refuses when busy, surprise removal keeps the kernel alive,
   and the desktop panel lists disks with size, file system, usage and an
   Eject button (`userspace/desktop/src/drives.rs`).
2. **Settings that work**: done. Accent, wallpaper, keyboard layout, time
   zone, host name and IPv4 configuration are written and read back by the
   applications; About has Check for updates and Install update on top of
   `/bin/osyx-update` (`userspace/desktop/src/bin/settings.rs`).
3. **Battery detection**: done in code. Multiple batteries are aggregated,
   ACPI hotplug is followed, unknown or implausible EC values are filtered,
   and the AML tests cover two batteries, a missing battery and wild values
   (`userspace/battery/`, `tests/acpi.rs`). Batteries are found by `_HID`,
   `_CID` or (when the table is only partly readable) by name and `_BST`;
   `batteryd` stays resident and rescans the namespace every 30 s until a
   battery device appears. Still open: verification on real ThinkPads.
4. **File formats and multiple files in Files**: done. Multi-selection
   (copy, move, delete, pack into tar), Extract here for tar/tar.gz/zip/7z/gz,
   a viewer and hex viewer, and "Open with" with per-extension choices are
   implemented (`userspace/desktop/src/bin/files.rs`,
   `userspace/filekit/src/openwith.rs`). Files plays WAV, FLAC, MP3, AAC/M4A,
   Ogg Vorbis and Opus (`userspace/desktop/src/audio_view.rs`), WebM and
   Matroska video with VP8 (`userspace/desktop/src/video_view.rs`), shows
   PNG, JPEG, GIF, BMP, ICO, WebP, JPEG 2000 and JBIG2 pictures and PDF
   documents with CCITT, JBIG2 and JPEG 2000 images, all decoded by
   `userspace/media`. Still open: H.264/MP4, VP9 and AV1 video (those files
   still need a registered application), TIFF, and thumbnails.
5. **Release hygiene**: partly done. CodSpeed benchmarks run on every pull
   request. Still open: the release workflow builds and signs only; it runs
   neither the host tests nor the Win32, SMP, desktop and Zen QEMU suites.

### Steam and Zen API gaps

Measured with `scripts/winparity.py scan` against the binaries in `build/steam`
(32-bit `SteamSetup.exe` and its NSIS plug-ins) and `build/zen/1.23.4b`
(`zen.exe`, `xul.dll`, `mozglue.dll`, `nss3.dll`) on 2026-10-31. The scan
scores every Steam import as usable (265 implemented, 2 partial of 267) and
every Zen one too (1476 implemented, 69 partial of 2644; 851 imports ship
with the application, 248 belong to external DLLs). No import is missing or
unsupported. These are source-scan measurements of exports, not proof of
behaviour.

The latest batch closes the gaps listed before it: `user32!TrackPopupMenu`
(tracked popup menus with owner-drawn bitmaps, `TPM_RETURNCMD`,
keyboard and mouse selection), `CreateSyntheticPointerDevice` with
`InjectSyntheticPointerInput` for touch and pen records,
`gdi32!ChoosePixelFormat`, `SetPixelFormat`, `SwapBuffers`, `GetFontData`
and `GetOutlineTextMetricsA`, the `msi.dll` database/view/record
functions, `winspool.drv` ordinal 203, the `wsock32` legacy ordinals (the
scanner now follows an ordinal table that delegates to another), plus
`SetFileTime` (new `futimes` system call and ext4/tmpfs support),
`TerminateThread` (per-thread kill status), `OpenProcess` with handle
inheritance, `RegCreateKeyExW` with `REG_OPTION_CREATE_LINK` and link
following, `ExitWindowsEx` logoff, and the remaining constant stubs.

A second batch takes on the partial rows: named events, mutexes, sections,
threads and timers honour handle inheritance; `DuplicateHandle` works
between two other processes; `HANDLE_FLAG_PROTECT_FROM_CLOSE` keeps
`CloseHandle` from releasing a handle; `CopyFileExW` takes its documented
flags and copies symbolic links; `FindFirstFileExW` matches case
sensitively when asked; `SetFileInformationByHandle` sets access and write
times, renames relative to a root directory handle and deletes with POSIX
semantics; `CreateIoCompletionPort` associates any open handle;
`GetProcessMitigationPolicy` answers for any process handle; timer security
attributes, access masks and resume hints are accepted.

What is left, in the order it blocks the applications:

| Application | API | State | Why it matters |
|---|---|---|---|
| Steam, `SteamSetup.exe` | `kernel32!CreateProcessW` with `DEBUG_PROCESS`, `DEBUG_ONLY_THIS_PROCESS` | partial | those flags need a debug-event API (`WaitForDebugEvent`, `ContinueDebugEvent`); the rest of `CreateProcessW` works |
| Steam, `msvcrt.dll` | `??2@YAPAXI@Z` | partial | allocation works, but `operator new` returns NULL instead of throwing `std::bad_alloc`: winrun has no MSVC C++ exception objects, the throw lives in the application's `vcruntime140`/`msvcp140` |
| Zen, many DLLs | 69 rows | partial | job objects and tokens, service control, event log, crypt32 stores, hooks and window stations, D3D11/DXGI, the GDI path calls, pointer confinement, and queries of other processes |

Zen's partial rows are capabilities OSyx does not model (Windows jobs,
logon sessions, the service database, certificate stores); none of them stops
the browser from starting.

#### The updated Steam client (2026-11-02)

The scan above covers the installer. The client it installs is a different
program: the 32-bit bootstrapper updates itself, installs the 64-bit client
and starts its `steam.exe`, which loads `steamui.dll`, `steamclient64.dll`,
`vgui2_s.dll`, `chromehtml.dll` and finally the CEF browser
(`steamwebhelper.exe`, `libcef.dll`). Measured by running it in QEMU:

- The bootstrapper update and the client install work.
- `steamui.dll` and `steamclient64.dll` load: their imports are complete
  (`ncrypt.dll`, the crypt32 encoders and self-signed certificates, SSPI,
  `getnameinfo`, `WSAEventSelect`, `AcceptEx`, APCs with `ReadFileEx`, the
  winmm mixer, `imagehlp.dll`, `GetIfEntry2` and others).
- `vgui2_s.dll` has `msimg32.dll` (`AlphaBlend`, `GradientFill`,
  `TransparentBlt`) and `opengl32.dll` (OpenGL 1.1 in software with WGL)
  now. Next it needs about 30 more GDI/user32/imm32/shell32 functions
  (font enumeration and loading, `Polyline`, viewport origins,
  `SetDIBitsToDevice`, icon copies and drawing); `crashhandler64.dll`
  needs the WinINet HTTP client (`InternetOpenW` to `InternetReadFile`).
- After that, CEF: about 100 more imports plus `winhttp.dll` and
  `dhcpcsvc.dll`, then Chromium's multi-process model on winrun.
- SSPI has no security package yet: `AcquireCredentialsHandle` reports
  `SEC_E_SECPKG_NOT_FOUND`. SChannel over rustls is the candidate if a
  Steam component turns out to call it.

Status on 2026-11-04 (`fix/steam-vgui2`):

- The GDI/user32 functions of `vgui2_s.dll`, the WinINet HTTP client of
  `crashhandler64.dll`, and CEF's imports (`winhttp.dll`, `dhcpcsvc.dll`,
  oleaut32 ordinals, the startup set of its delay-loaded imports) are in.
  `steamwebhelper.exe` loads, runs Chromium's browser process and starts
  the network service, which stays up.
- Fixed on the way: 64 GiB per-process reservation cap (now 32 TiB) and a
  word of state per reserved page in winrun; DuplicateHandle of the
  current process and per-handle section rights; process queries on real
  and ended process handles; a DLL that LoadLibrary loaded and the last
  FreeLibrary detached restarts from its original data when loaded again
  (DLLs linked at start-up are never detached, as on Windows);
  IInitializeSpy; overlapped sockets on completion ports.
- Open: the GPU process does not start (`GPU process launch failed:
  error_code=49`). It runs in Chromium's sandbox, whose broker writes into
  the suspended child (VirtualAllocEx, WriteProcessMemory, ntdll
  interceptions); winrun's cross-process memory calls work only on the
  calling process. Without a GPU process there is no window. With
  `-no-cef-sandbox`, `steam.exe` stops on "fatal stalled cross-thread
  pipe" after 180 s instead.
- Also open: `WSALookupServiceBegin` for NS_NLA (network change
  notifications), the rest of CEF's delay-loaded imports (about 150, most
  in paths not reached yet), `steamservice.dll` (32-bit, loaded into the
  64-bit client).

### Performance (see research/performance-profile.md)

6. Split the global process table lock (handle tables, mappings, page
   faults) and the kernel entry lock. Done: handle tables have their own
   lock (`HANDLES`), so channel, wait and futex lookups no longer queue
   behind mappings and page faults. Still open: mappings and page faults
   share `TABLE`, and `smp::enter_kernel` serialises all system calls.
7. Demand-paged file views, virtio-blk with several requests in flight,
   async readahead. Done: a read-only section loads 256 KiB runs on the
   first touch instead of reading the whole 177 MiB archive at map time;
   writable sections stay eagerly loaded for coherent write-back; virtio-blk
   keeps up to four chains in flight; read-ahead windows run on a background
   kernel thread while the reader gets the first pages immediately.
8. Software WebRender: fewer full-frame copies, damage-based presents in
   PRISM. Done: PRISM renders and presents only damaged rectangles; Gecko
   already runs it with software WebRender. Still open: the Win32
   compatibility path (`winrun`, `vcruntime140`) still copies whole frames.
9. **GPU acceleration**: 2D scan-out (VirtIO-GPU) only. Done: full
   XSAVE/AVX state (XCR0, per-thread AVX state, TCG boot path). Still open:
   a 3D path (VirtIO-GPU virgl or a native driver) behind a
   Direct3D/OpenGL layer for the Windows compatibility code.

### Security

10. Enable a sandbox for browser content processes: Gecko's own sandbox
    needs restricted tokens, job objects and a broker; an OSyx-native
    alternative is per-process file system views and syscall filters.

### Longer term

11. **Real-hardware coverage**: NVMe, AHCI, wired NICs (`e1000`, `rtl8168`)
    and Intel Wi-Fi (`iwm` through the BSD layer) drivers exist; the open
    work is verification and fixes on more machines and firmware. I2C-HID
    touchpads and keyboards have a polled driver (DesignWare controllers,
    `kernel/src/drivers/i2c.rs`) that is untested on real hardware; interrupts
    need an I/O APIC driver and GPIO controller drivers.
12. **Windows compatibility expansion**: cross-process sharing and object
    names, child controls, SEH, delay imports, process creation and the
    runtime dependencies of larger applications; the measured remaining
    imports of Steam and Zen are listed under
    [Steam and Zen API gaps](#steam-and-zen-api-gaps). Windows 11 API-set
    research with REA (`research/windows11-apiset-and-forwarding.md`).
13. **Storage**: page cache with delayed writes. Done: ext4 htree insertion
    (splitting leaves and index nodes, hash collisions across nodes),
    deeper extent trees, completion interrupts for virtio-blk, AHCI and NVMe
    (MSI-X or MSI, polling fallback); FAT entries are stamped with the wall
    clock on create and write, and `stat` reports the stored time (2-second
    DOS resolution); the write-back thread flushes ext4 after 5 s and exFAT
    after 2 s of quiet. Still open: FAT writes its metadata through on every
    change.
14. **SMP**: finer-grained kernel locking (still one kernel entry lock and
    `TABLE` for mappings and page faults) and load balancing policies.
15. **OSyx Std**: `thread`, futex-based `Mutex`, `Condvar`, `RwLock`,
    `Once`/`OnceLock` and `mpsc` are implemented; next is the genuine `std`
    port ([rust-std.md](rust-std.md)).
16. **Desktop**: per-pixel alpha (ARGB8888 windows), raw-report keyboards,
    AXIS redirection (`<`, `>`, `>>`, built-ins included), pipelines with at
    most one built-in stage and `if`/`for` blocks are implemented; still
    open: job control (see [graphics.md](graphics.md)).

## Real hardware: current scope and limits

NVMe, AHCI, physical Ethernet and Intel Wi-Fi drivers are present. The README lists the ThinkPad T495 as tested hardware; support on other machines and firmware varies. NVMe and AHCI complete requests by interrupt (MSI-X or MSI) and fall back to polling.

- **Display**: GOP on physical hardware; VirtIO-GPU under QEMU with EDID
  preferred-mode selection. AMD display functions and their VBIOS are
  identified at boot, but native GPU modesetting is not implemented.
- **SMP**: user threads run in parallel; kernel entry uses a common lock.
- **USB**: xHCI supports hubs; device and topology coverage remains limited.
- **Laptop input**: PS/2 input is supported. HID over I2C (keyboards, mice, precision touchpads behind a DesignWare I2C controller, found through ACPI by `i2chidd`) is implemented but polled and not yet verified on real hardware; touchscreens, gestures (tap, scroll) and interrupt-driven input are not supported.
- **Power**: shutdown requests ACPI S5 using FADT and the static DSDT _S5 package where available. Firmware behavior varies.
