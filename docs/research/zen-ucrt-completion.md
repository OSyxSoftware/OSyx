# Zen: extended UCRT imports

The unfinished UCRT work has been extended with formatted input,
floating-point control, diagnostics, file metadata and environment helpers.
This closes a group of import gaps in `xul.dll`; it does not establish
functional browser support. The real Zen package is still tested separately.

## Implemented behaviour

- `__stdio_common_vsscanf`, `vswscanf`, `vfscanf` and `vfwscanf`: bounded
  input, Windows integer widths, floating conversions, scansets, suppression,
  `%n`, secure buffer-size arguments and the legacy wide-specifier option.
  A stream is locked across scanning and its single lookahead character is
  returned before unlocking. UTF-8 continuation bytes do not consume extra
  characters from a wide destination's field width.
- `_control87`, `_controlfp`, `_controlfp_s`, `fegetround`, `fesetround`,
  `_statusfp`, `_clearfp` and `_fpreset`: Windows FLOAT.H masks translated
  to the calling thread's MXCSR. The release Windows CRT accepts ignored
  x87 precision/infinity requests on x64; unknown requested bits fail.
- `strerror`, `strerror_s`, `_wcserror`, `_wcserror_s`, `perror`, `_wperror`
  and `_wassert`. Narrow error text, wide error text and the calendar result
  have distinct, correctly aligned per-thread buffers.
- `_wstat64`, `_wstat64i32`, `_fstat64i32`, `_wchmod`, `_getdrives`,
  `_mktemp_s`, `_wmktemp_s`, `getenv_s` and `_wgetdcwd`: real file attributes,
  sizes and timestamps, readonly changes, drive mapping, absent-name checks,
  environment copies and the current directory. Temporary-name generation
  does not create the file; the caller still needs exclusive creation.
- `mbsrtowcs`, `_mbsinc` and `_mbsdec` for the supported C/C.UTF-8 locales;
  `imaxdiv` uses the existing 64-bit quotient/remainder implementation.
  The multibyte API-set contract is mapped to `ucrtbase.dll` as well.
- The earlier extended math and local-time work is integrated. Its
  `sinh(700)`/`cosh(700)` failure was fixed by avoiding an intermediate
  double-double division whose operand splitting overflowed. `llrintf`
  now also has the required XMM argument trampoline. Time conversion uses
  the system POSIX rule in `/etc/TZ`, with the CRT `TZ` override; `_ftime64`
  reports system DST status even when `TZ` overrides `_timezone`.
- Dynamic `LoadLibrary` failures now log the original dependency error
  before it is reduced to a Win32 error number. This makes an XPCOM load
  failure actionable without changing the API's result.

## Verification

The PE fixtures link the Windows UCRT import library and are executed both
on the Windows host and in OSyx under QEMU. `crt2.c` includes 170 reference
math vectors and edge cases. `crt3.c` covers scanning, control modes across
threads, diagnostics and timezone transitions. `crt4.c` independently checks
the 56-byte `_stat64` and 48-byte `_stat64i32` layouts, files, environment
and multibyte conversion. All three are included in the normal Win32 suite
and their summaries are required by `scripts/win-test.ps1`.

Windows host results: `crt2` 20/20, `crt3` 69/69 and `crt4` 39/39.
The OSyx variants additionally exercise the shared UTF-8 decoder and
cursor boundaries, which have explicitly different coverage on Windows.
QEMU results: `crt2` 20/20, `crt3` 71/71 and `crt4` 40/40; the complete
Win32 integration suite passed 1070 records with zero failures, including
27/27 init checks. Clippy for `osyx-win32` passed with `-D warnings`.

Evidence files are kept under `build/`: `crt2-native.log`,
`crt3-native.log`, `crt4-native.log`, `codex-crt-win-test.log` and
`test/win.serial.log`. Browser probes and dependency audits are under
`build/zen/attempt-15.*` and `attempt-16.*`.

## Browser probe 16

The audit of the actual compiled export/API-set tables reports **zero
unresolved UCRT import occurrences for `xul.dll`**. Package-wide gaps fell
from 1913 in attempt 14 to 1654 in attempt 16. These counts include repeated
static/delay import occurrences across the package, and measure symbol
presence rather than complete API semantics.

Zen 1.23.4b remains blocked before functional browser tests. The final
dynamic loader error is now visible:

```
LoadLibrary(C:\opt\zen\nss3.dll) failed:
wsock32.dll (needed by nss3.dll) was not found
```

`nss3.dll` imports 30 legacy Winsock symbols by ordinal from `wsock32.dll`,
including `#1`, `#2`, `#3`, `#111`, `#115`, `#116` and `#151`. That DLL and
its ordinal export table are the next concrete loading requirement. Earlier
attempts to load `bcryptprimitives.dll` and `cryptbase.dll` also failed;
those failures are recorded without assuming that each is fatal. The probe
had no kernel panic, but window/page rendering, tabs, input and HTTPS remain
unverified. The script's exit code 2 means blocked/unverified.

## Remaining limits

The scanner uses the calling thread's C/C.UTF-8 locale; a non-NULL explicit
`_locale_t` argument is not yet applied. OSyx deliberately uses its shared
UTF-8 decoder for narrow-to-wide scanning: this host's Windows UCRT instead
sign-extends the leading byte for the tested `%ls` input `C5 BC`. Those
UTF-8 scanning cases are therefore tested separately in OSyx.

`mbsrtowcs` supports an initial opaque `mbstate_t` and restarts at complete
characters through the source cursor. A noninitial opaque state is rejected
with `EILSEQ`; the runtime does not yet expose `mbrtowc` to construct one.
The 32-bit-size stat variants reject files above `INT32_MAX` with `ERANGE`.
Drive-directory lookup supports drive 0 and C:, matching the runtime's
current drive model. The math reference vectors do not prove correct
rounding over every possible floating-point input.

API references: [scanf buffer sizes](https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/scanf-s-scanf-s-l-wscanf-s-wscanf-s-l),
[floating-point controls](https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/control87-controlfp-control87-2),
[perror](https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/perror-wperror),
[ftime](https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/ftime-ftime32-ftime64),
[restartable multibyte conversion](https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/mbsrtowcs).
