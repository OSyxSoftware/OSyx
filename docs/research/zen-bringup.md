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
