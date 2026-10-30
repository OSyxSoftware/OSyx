# 32-bit (PE32) Windows programs: plan and status

OSyx runs 64-bit PE32+ programs through `winrun`. Many installers and older
applications (SteamSetup.exe, NSIS installers) are 32-bit PE32 and are refused:
`optional header magic 0x10b is not PE32+`. This document is the plan for
running them, and the record of what exists.

## Inventory: SteamSetup.exe

Checked with `cargo run -p osyx-pe --example pe32-imports -- SteamSetup.exe`:
PE32, machine 0x14c, GUI subsystem, 166 imports in 8 DLLs: KERNEL32 66,
USER32 65, GDI32 8, ADVAPI32 13, SHELL32 6, COMCTL32 4, ole32 4. That is an
NSIS installer: a bounded, ordinary API surface, and most of those functions
already exist in the 64-bit `winrun`. The installer then downloads the
32-bit Steam client, which needs far more (a browser engine, DirectX); that is
a later milestone and not covered here.

## Design

1. **Compatibility mode.** 32-bit code runs in the same process in x86-64
   compatibility mode, with a 32-bit ring 3 code segment in the GDT
   (D=1, L=0) beside the 64-bit one. All 32-bit images, heap and stacks are
   placed below 4 GiB.
2. **Entering and leaving 32-bit code.** `winrun` far-returns into the
   32-bit segment; a 32-bit API stub far-jumps back to the 64-bit segment
   ("heaven's gate", as Windows' WoW64 does).
3. **FS segment.** 32-bit code finds its TEB at `fs:[0x18]`. Compatibility
   mode takes the FS base from a descriptor, not from the FS base MSR, so the
   kernel needs a per-thread data descriptor whose base is switched with the
   thread.
4. **Loader.** PE32 headers, `IMAGE_REL_BASED_HIGHLOW` relocations, 4-byte
   import thunks, `IMAGE_TLS_DIRECTORY32`, 32-bit export tables.
5. **API bridge.** Every 32-bit import resolves to a small stub that
   switches to 64-bit mode and calls the existing 64-bit implementation
   with the stdcall arguments widened (pointers zero-extended, signed values
   sign-extended per parameter). Functions that take or return structures
   with pointer-sized fields (`WNDCLASSW`, `MSG`, `PAINTSTRUCT`,
   `WIN32_FIND_DATAW`, `STARTUPINFOW`, `LOGFONT`...) need explicit
   conversion. Window procedures and callbacks need the reverse stub (64-bit
   to 32-bit).
6. **Exceptions and SEH.** 32-bit frame-based SEH (`fs:[0]` chain), not the
   table-based x64 scheme.

## Milestones

| # | Milestone | Status |
|---|---|---|
| 1 | `osyx-pe` parses PE32 (`PeFile::parse_any`, `is_pe32`): headers, 4-byte import thunks, TLS directory and callbacks; `pe32-imports` example | Done (host tested on a real installer) |
| 2 | Loader support in `osyx-pe-loader`: HIGHLOW relocations, 32-bit image layout below 4 GiB, imports written as 32-bit stubs | Not started |
| 3 | Kernel: 32-bit user code segment and per-thread FS descriptor, switched on context switch | Not started |
| 4 | Bridge for a console program (`GetStdHandle`, `WriteFile`, `ExitProcess`...) and a 32-bit test program in `compatibility/win32/tests` | Not started |
| 5 | The 166 imports of SteamSetup.exe: kernel32, advapi32, then user32/gdi32/comctl32 with callbacks | Not started |
| 6 | 32-bit SEH, WOW64-style file system and registry views | Not started |

`parse` still rejects PE32, so nothing changes for existing programs until
the loader accepts the new parser entry point.
