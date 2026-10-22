# Testing

## Commands

```powershell
.\scripts\check-env.ps1             # tool check, no changes to the system
.\scripts\build.ps1 [-Cmdline '…'] [-NoInit] [-DebugBuild]
.\scripts\run-qemu.ps1 [-Cmdline '…'] [-Gdb] [-Follow] [-Headless]
.\scripts\test.ps1 [-SkipHost] [-SkipQemu]
```

QEMU runs `q35`, `qemu64,+nx`, 512 MiB, OVMF from the MSYS2 QEMU package, the
ESP as a read-only virtio FAT disk (`fat:build/esp`), COM1 to a log file and
`isa-debug-exit` at port `0xf4`. Logs and screenshots go to `build/test/`.

## Suite (`scripts/test.ps1`)

| Test | Checks |
|---|---|
| host unit tests | `cargo test` for abi, bootinfo, gfx, elf, mm, os-sys, os-std |
| clippy | `-D warnings` for host crates, loader (UEFI), kernel and user crates |
| QEMU boot + kernel self-tests | boot stages, 11 in-kernel tests, exit 33 |
| ring-3 init checks | `Hello from OSyx userspace!`, 13 user checks, exit 0, no frame leak |
| user page fault | process killed with `#PF`, error code 0x4, kernel survives |
| user panic | panic message printed, status 101 |
| user exit code | status 42 propagates |
| boot without init | kernel runs without `INIT.ELF` |
| exception diagnostics | kernel `#PF`, `#GP`, `#UD`, `#DE`: decoded report + panic exit 37 |
| invalid kernel | ELFCLASS32 kernel rejected with on-screen error |
| screenshots | loader error screen and running kernel screen via QMP `screendump` |

## Last recorded result

All 15 tests passed (54 host unit tests) on Windows 11 build 26300 with
Rust 1.99.1, QEMU 11.1.2. The suite takes about 2–3 minutes.

## Not covered yet

Real hardware, other firmware, resolutions other than the default GOP choice
(the layout itself is unit-tested for 640×480 – 3840×2160), multi-core,
memory sizes other than 512 MiB.
