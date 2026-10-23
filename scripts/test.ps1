# Full OSyx test suite:
#   1. host unit tests (cargo test) and clippy
#   2. QEMU boot test: loader -> kernel -> in-kernel self-tests (incl. threads,
#      preemption, FPU, PCI, virtio-blk, VFS, program loader) -> /bin/init
#      from the ext4 root in ring 3 running its checks (incl. os_std::fs on
#      every mount, spawn/execve of worker and /usr/bin/probe) -> exit 33;
#      leak and preemption checks on the log; the files OSyx wrote are then
#      read back on the host with mtools and e2fsprogs; variants for user
#      faults, user panics, exit codes, power cut, no entropy, a boot
#      without programs and one on the fallback root
#   3. controlled CPU exception tests (#PF, #GP, #UD, #DE) -> diagnostics + exit 37
#   4. invalid kernel image -> boot loader error screen
#   5. screenshots of the kernel screen and the loader error screen via QMP
#   6. the graphical session: PRISM, desktop shell, AXIS Terminal, Files and
#      System Information driven by injected USB keyboard and mouse events
# Serial logs and screenshots are kept in build/test/.
param(
    [switch]$SkipHost,
    [switch]$SkipQemu,
    [int]$TimeoutSec = 120
)
. (Join-Path $PSScriptRoot 'common.ps1')
# "[user] fs: ..." checks init runs (userspace/apps/src/fs_checks.rs).
$ExpectedFsChecks = 19
# "[user] exec: ..." checks init runs (userspace/apps/src/exec_checks.rs).
$ExpectedExecChecks = 13
# "[user] ipc: ..." checks init runs (userspace/apps/src/ipc_checks.rs).
$ExpectedIpcChecks = 13
$results = New-Object System.Collections.Generic.List[object]
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null

function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    $results.Add([pscustomobject]@{ Test = $Name; Result = $(if ($Ok) { 'PASS' } else { 'FAIL' }); Detail = $Detail })
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
}

function Clean-Log([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    return ([IO.File]::ReadAllText($Path) -replace '\x1b\[[0-9;?]*[a-zA-Z]', '' -replace "`r", '')
}

# Boots the current build/esp and waits for QEMU to exit (or kills it).
function Invoke-Boot([string]$Name, [int]$Timeout, [string]$WaitPattern = '', [string]$Screenshot = '', [switch]$NoRng) {
    $log = Join-Path $testDir "$Name.serial.log"
    if (Test-Path $log) { Remove-Item $log }
    $port = 0
    if ($Screenshot) { $port = Get-Random -Minimum 20000 -Maximum 40000 }
    $p = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -QmpPort $port -NoRng:$NoRng)
    $shot = $null
    if ($WaitPattern) {
        if (Wait-ForLog $log $WaitPattern $Timeout $p) {
            if ($Screenshot) {
                Start-Sleep -Milliseconds 700  # let the framebuffer settle
                $shot = Save-Screenshot $port $Screenshot
            }
        }
        if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
        return @{ Log = (Clean-Log $log); Exit = $null; Shot = $shot }
    }
    if (-not $p.WaitForExit($Timeout * 1000)) {
        $p.Kill(); $p.WaitForExit()
        return @{ Log = (Clean-Log $log); Exit = 'timeout'; Shot = $null }
    }
    return @{ Log = (Clean-Log $log); Exit = $p.ExitCode; Shot = $null }
}

Push-Location $Root
try {
    if (-not $SkipHost) {
        Write-Host '==> host unit tests'
        $out = ''
        try { $out = (Invoke-Checked 'cargo test' { cargo test --workspace --exclude osyx-kernel --exclude osyx-uefi-loader --exclude osyx-apps --exclude osyx-desktop --exclude prism-client }) -join "`n"; $ok = $true } catch { $ok = $false; $out += $_ }
        $passed = ([regex]::Matches($out, 'test result: ok\. (\d+) passed') | ForEach-Object { [int]$_.Groups[1].Value } | Measure-Object -Sum).Sum
        Record 'host unit tests' $ok "$passed tests passed"
        Write-Host '==> clippy (host crates + freestanding targets)'
        $ok = $true
        try {
            Invoke-Checked 'clippy host' { cargo clippy --workspace --exclude osyx-kernel --exclude osyx-uefi-loader --exclude osyx-apps --exclude osyx-desktop --exclude prism-client --exclude osyx-nettools --all-targets -- -D warnings } | Out-Null
            Invoke-Checked 'clippy user' { cargo clippy -p osyx-apps -p osyx-desktop -p osyx-nettools -p prism-client -p os-std -p os-sys --target x86_64-unknown-none --target-dir target/user -- -D warnings } | Out-Null
            Invoke-Checked 'clippy loader' { cargo clippy -p osyx-uefi-loader --target x86_64-unknown-uefi -- -D warnings } | Out-Null
            Invoke-Checked 'clippy kernel' { cargo clippy -p osyx-kernel --target x86_64-unknown-none -- -D warnings } | Out-Null
        } catch { $ok = $false; Write-Host $_ }
        Record 'clippy -D warnings' $ok ''
    }

    if (-not $SkipQemu) {
        # --- 2. normal boot with self-tests --------------------------------
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'test.exit boot.fast' | Out-Null
        $r = Invoke-Boot 'boot' $TimeoutSec
        $selftest = [regex]::Match($r.Log, 'self-tests: (\d+) passed, (\d+) failed')
        $ok = $r.Exit -eq 33 -and $selftest.Success -and $selftest.Groups[2].Value -eq '0' `
            -and $r.Log -match 'boot services exited' -and $r.Log -match 'OSyx kernel \S+ \(x86_64\) starting'
        Record 'QEMU boot + kernel self-tests' $ok "exit=$($r.Exit) $($selftest.Value)"
        $user = [regex]::Match($r.Log, 'user checks: (\d+) passed, (\d+) failed')
        $ok = $r.Exit -eq 33 -and $r.Log -match '\[init:1\] Hello from OSyx userspace!' -and $user.Success `
            -and $user.Groups[2].Value -eq '0' -and $r.Log -match 'init: exit status 0x0, expected 0x0: ok'
        Record 'ring-3 init (OSyx Std) checks' $ok $user.Value

        # Every process must return exactly the frames it owned, and nothing
        # may be left behind once init has exited.
        $released = [regex]::Matches($r.Log, 'process \d+ \(\w+\): released \d+ frames, no leak').Count
        $ok = $released -ge 10 -and $r.Log -notmatch 'FRAME LEAK' -and $r.Log -match ', 0 processes left' `
            -and $r.Log -match 'scheduler: \d+ context switches, \d+ threads alive \(0 ready\), 0 processes left' `
            -and $r.Log -notmatch '  thread +\d+ \S+ +pid +[1-9]'
        Record 'process teardown without leaks' $ok "$released processes released cleanly"

        # Preemption between user processes: three children that never yield
        # must all have started before the first one finishes.
        $lines = $r.Log -split "`n"
        # Only the first three spinners belong to this check (later ones are$nl        # busy children of the IPC kill check).
        $startLines = @($lines | Select-String -Pattern 'spin \d+: started' | Select-Object -First 3)
        $starts = $startLines.Count
        $firstDone = ($lines | Select-String -Pattern 'spin \d+: finished' | Select-Object -First 1).LineNumber
        $lastStart = if ($starts) { $startLines[-1].LineNumber } else { 0 }
        $took = [regex]::Match($r.Log, '3 x 300 ms spinners finished in (\d+) ms')
        $ok = $starts -eq 3 -and $firstDone -and $lastStart -lt $firstDone -and $took.Success -and [int]$took.Groups[1].Value -lt 650
        Record 'preemptive scheduling of user processes' $ok $took.Value

        # Storage: GPT disk, FAT32 ESP at /boot, tmpfs, devfs, entropy, and
        # the kernel-side VFS self-tests.
        $ok = $r.Log -match 'gpt vda: vda1 64 MiB' -and $r.Log -match 'vfs: fat32 /dev/disks/vda1 mounted at /boot \(read-write' `
            -and $r.Log -match 'vfs: tmpfs tmpfs mounted at /tmp' -and $r.Log -match 'vfs: tmpfs tmpfs mounted at /run' `
            -and $r.Log -match 'vfs: devfs devfs mounted at /dev' -and $r.Log -match 'random: ChaCha20 DRBG seeded from' `
            -and $r.Log -match '\[test\] PCI enumeration \.\.\. ok' -and $r.Log -match '\[test\] block device I/O \.\.\. ok' `
            -and ([regex]::Matches($r.Log, '\[test\] (VFS|devfs|FAT32|ext4|concurrent)[^\n]*\.\.\. ok').Count -eq 7)
        $ok = $ok -and $r.Log -match 'vfs: ext4 /dev/disks/vda2 mounted at / \(read-write' -and $r.Log -match '\[test\] ext4 root through the VFS \.\.\. ok'
        Record 'GPT, ext4 /, FAT32 /boot, tmpfs, devfs and VFS self-tests' $ok ([regex]::Match($r.Log, 'storage: [^\n]*').Value)
        $fsOk = [regex]::Matches($r.Log, '\[user\] fs: [^\n]* \.\.\. ok').Count
        $ok = $fsOk -eq $ExpectedFsChecks -and $r.Log -notmatch '\[user\] fs: [^\n]*FAILED' -and $r.Log -match 'closed 11 open handles' `
            -and $r.Log -match 'as-user 1000: 12 of 12 permission checks passed'
        Record 'OSyx Std fs from ring 3 (all mounts, perms, devices)' $ok "$fsOk of $ExpectedFsChecks fs checks passed"

        # Program loader: init from /bin/init on the ext4 root, probe from
        # /usr/bin, argv / envp / auxv, execve (5 in a chain + 1), refusals.
        $execOk = [regex]::Matches($r.Log, '\[user\] exec: [^\n]* \.\.\. ok').Count
        $oldImages = [regex]::Matches($r.Log, 'process \d+ \(probe\): old image released \d+ frames, no leak').Count
        # -cnotmatch: one check is named "exec: failed execve ...".
        $ok = $execOk -eq $ExpectedExecChecks -and $r.Log -cnotmatch '\[user\] exec: [^\n]*FAILED' -and $oldImages -eq 6 `
            -and $r.Log -match 'process 1 \(init\): /bin/init spawned by 0' -and $r.Log -match '\(probe\): /usr/bin/probe spawned by' `
            -and $r.Log -match '\[test\] program loader: search path, permissions, bad images \.\.\. ok'
        Record 'ELF loader: spawn/execve from /bin and /usr/bin, argv, envp, auxv' $ok "$execOk of $ExpectedExecChecks exec checks passed, $oldImages execve image swaps"

        # Kernel objects from ring 3: pipes, channels, shared memory, waiting.
        $ipcOk = [regex]::Matches($r.Log, '\[user\] ipc: [^\n]* \.\.\. ok').Count
        $ok = $ipcOk -eq $ExpectedIpcChecks -and $r.Log -cnotmatch '\[user\] ipc: [^\n]*FAILED' -and $r.Log -match 'ipc: 0 live service\(s\) \[\], 0 shared frames mapped'
        Record 'pipes, channels, shared memory from ring 3' $ok "$ipcOk of $ExpectedIpcChecks ipc checks passed"

        # Native xHCI driver: controller up with MSI-X, boot keyboard and mouse
        # enumerated as input devices (QEMU has no PS/2 controller).
        $ok = $r.Log -match 'xhci0: running \(MSI-X interrupts\)' -and $r.Log -match 'input: device \d+ "QEMU USB Keyboard" \(keyboard\) attached as /dev/input/event\d' `
            -and $r.Log -match 'input: device \d+ "QEMU USB Mouse" \(mouse\) attached as /dev/input/event\d' -and $r.Log -match 'usb: controller 0 took [1-9]\d* interrupts'
        Record 'xHCI controller, USB keyboard and mouse enumeration' $ok ([regex]::Match($r.Log, 'usb: controller 0 took \d+ interrupts').Value)

        # The files init left on the ESP, read back on the host by mtools (an
        # independent FAT implementation) after QEMU has exited.
        $mtype = Find-Tool 'mtype' @('C:\msys64\ucrt64\bin\mtype.exe')
        $mcopy = Find-Tool 'mcopy' @('C:\msys64\ucrt64\bin\mcopy.exe')
        $ok = $false; $detail = 'mtools not found'
        if ($mtype -and $mcopy) {
            Push-Location $BuildDir
            try {
                $saved = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
                $img = 'osyx.img@@1M'
                $text = (& $mtype -i $img ::/osyx-out/hello-from-osyx.txt) -join "`n"
                $fromWorker = (& $mtype -i $img ::/osyx-out/from-worker.txt) -join "`n"
                if (Test-Path pattern.bin) { Remove-Item pattern.bin }
                & $mcopy -n -i $img ::/osyx-out/pattern.bin pattern.bin 2>&1 | Out-Null
                $ErrorActionPreference = $saved
                $patternOk = $false
                if (Test-Path pattern.bin) {
                    $got = [IO.File]::ReadAllBytes((Join-Path $BuildDir 'pattern.bin'))
                    $patternOk = $got.Length -eq 200000
                    for ($i = 0; $patternOk -and $i -lt $got.Length; $i++) {
                        if ($got[$i] -ne [byte](($i * 31 + 5) % 256)) { $patternOk = $false }
                    }
                    Remove-Item pattern.bin
                }
                $check = (Invoke-Checked 'fatcheck' { cargo run -q -p osyx-fat --example fatcheck -- $img }) -join ' '
                $ok = $text -eq 'Written by OSyx init through os_std::fs.' -and $fromWorker -match '^written by worker \d+$' `
                    -and $patternOk -and $check -match 'FSInfo accurate'
                $detail = "text=$($text -ne '') pattern=$patternOk; $check"
            } catch { $detail = "$_" } finally { Pop-Location }
        }
        Record 'host reads OSyx-written ESP files (mtools + fsck)' $ok $detail

        # The ext4 root OSyx wrote to, checked after shutdown by e2fsck and
        # read back by debugfs (e2fsprogs in WSL).
        $rootOffset = (1 + $EspSizeMiB) * 1MB
        $out = (Invoke-Checked 'check-root' { wsl.exe -d Ubuntu-24.04 -- bash (ConvertTo-WslPath (Join-Path $PSScriptRoot 'check-root.sh')) (ConvertTo-WslPath $DiskImage) $rootOffset }) -join "`n"
        $kv = @{}
        foreach ($line in $out -split "`n") { if ($line -match '^(\w+)=(.*)$') { $kv[$Matches[1]] = $Matches[2] } }
        $ok = $kv['e2fsck'] -eq '0' -and $kv['hello'] -eq 'Written by OSyx init through os_std::fs.' `
            -and $kv['worker'] -match '^written by worker \d+$' -and $kv['pattern_sha256'] -eq $kv['expected_sha256'] `
            -and $kv['deep'] -eq 'deep' -and $kv['link'] -eq 'Fast link dest: "moved/c/deep.txt"' `
            -and $kv['private_mode'] -eq 'Mode: 0640' -and $kv['temp_gone'] -eq '1'
        Record 'host checks the ext4 root OSyx wrote (e2fsck + debugfs)' $ok "e2fsck=$($kv['e2fsck']): $($kv['e2fsck_summary'])"

        # Power cut on the ext4 root while a removed file is still open: the
        # next boot must recover (journal, orphan list) to a clean volume.
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'test.exit boot.fast test.power-cut init.args=crash-prep' | Out-Null
        $r = Invoke-Boot 'power-cut' $TimeoutSec
        $cut = $r.Exit -eq 33 -and $r.Log -match 'hold-orphan \d+: holding removed /crash-orphan.bin open' `
            -and $r.Log -match 'test.power-cut: stopping without unmounting'
        $dump = (Invoke-Checked 'dumpe2fs' { wsl.exe -d Ubuntu-24.04 -- dumpe2fs -h "$(ConvertTo-WslPath $DiskImage)?offset=$rootOffset" }) -join "`n"
        $dirty = $dump -match 'needs_recovery' -and $dump -match 'First orphan inode:\s+[1-9]' -and $dump -match 'Filesystem state:\s+not clean'
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'test.exit boot.fast init.args=exit,0' -KeepRoot | Out-Null
        $r = Invoke-Boot 'after-power-cut' $TimeoutSec
        $recovered = $r.Exit -eq 33 -and $r.Log -match 'ext4 vda2: recovery replayed \d+ journal blocks, freed 1 orphaned inodes' `
            -and $r.Log -match 'self-tests: \d+ passed, 0 failed'
        $out = (Invoke-Checked 'check-root' { wsl.exe -d Ubuntu-24.04 -- bash (ConvertTo-WslPath (Join-Path $PSScriptRoot 'check-root.sh')) (ConvertTo-WslPath $DiskImage) $rootOffset }) -join "`n"
        $clean = $out -match '(?m)^e2fsck=0$'
        Record 'power cut with an open removed file, recovery at next boot' ($cut -and $dirty -and $recovered -and $clean) "cut=$cut dirty-on-disk=$dirty recovered=$recovered e2fsck-clean=$clean"

        # Without an entropy source /dev/random must not exist (no
        # predictable bytes); everything else still works.
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'test.exit boot.fast init.args=exit,0' | Out-Null
        $r = Invoke-Boot 'boot-no-entropy' $TimeoutSec -NoRng
        $ok = $r.Exit -eq 33 -and $r.Log -match 'random: no hardware entropy source; /dev/random is unavailable' `
            -and $r.Log -match '\[test\]   no entropy source: /dev/random absent' -and $r.Log -match 'self-tests: \d+ passed, 0 failed'
        Record 'no entropy source: /dev/random withheld' $ok "exit=$($r.Exit)"
        $userCases = @(
            @{ Name = 'user page fault kills process'; Args = 'fault'; Expect = '0x4000000e'
               Pattern = 'process 1 terminated: Page fault \(#PF\) at rip 0x[0-9a-f]+, error code 0x4' }
            @{ Name = 'user panic exits with 101'; Args = 'panic'; Expect = '101'
               Pattern = '\[init:1\] program panicked: panicked at' }
            @{ Name = 'user exit code propagates'; Args = 'exit,42'; Expect = '42'
               Pattern = 'process 1 \(init\) exited with status 42' }
        )
        foreach ($u in $userCases) {
            & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline "test.exit boot.fast init.args=$($u.Args) init.expect=$($u.Expect)" | Out-Null
            $r = Invoke-Boot "user-$($u.Args -replace ',', '-')" $TimeoutSec
            $ok = $r.Exit -eq 33 -and $r.Log -match $u.Pattern -and $r.Log -match ': ok' `
                -and $r.Log -match 'process 1 \(init\): released \d+ frames, no leak' -and $r.Log -notmatch 'KERNEL PANIC'
            Record $u.Name $ok "exit=$($r.Exit)"
        }

        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'test.exit boot.fast' -NoInit | Out-Null
        $r = Invoke-Boot 'boot-no-init' $TimeoutSec
        $ok = $r.Exit -eq 33 -and $r.Log -match 'no \\EFI\\OSYX\\BIN directory' -and $r.Log -match 'no init program "init" in /bin or /usr/bin' `
            -and $r.Log -notmatch 'process 1'
        Record 'boot without init program' $ok "exit=$($r.Exit)"

        # No usable root partition: the boot modules become /bin of the
        # fallback tmpfs root and init starts from there.
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'test.exit boot.fast root=vdz init.args=exit,0' | Out-Null
        $r = Invoke-Boot 'boot-fallback-root' $TimeoutSec
        $ok = $r.Exit -eq 33 -and $r.Log -match 'storage: root=vdz: no such block device' `
            -and $r.Log -match 'storage: boot module init installed as /bin/init' -and $r.Log -match 'process 1 \(init\): /bin/init spawned by 0' `
            -and $r.Log -match 'init: exit status 0x0, expected 0x0: ok' -and $r.Log -match 'self-tests: \d+ passed, 0 failed'
        Record 'fallback root: init from boot modules in /bin' $ok "exit=$($r.Exit)"

        # --- 3. controlled exceptions --------------------------------------
        $faults = @{
            pf = 'EXCEPTION: Page fault \(#PF\).*\n.*page fault address 0xffffd10000001000: non-present page, read access in kernel mode'
            gp = 'EXCEPTION: General protection fault \(#GP\)'
            ud = 'EXCEPTION: Invalid opcode \(#UD\)'
            de = 'EXCEPTION: Divide error \(#DE\)'
        }
        foreach ($kind in 'pf', 'gp', 'ud', 'de') {
            & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline "test.fault=$kind boot.fast" | Out-Null
            $r = Invoke-Boot "fault-$kind" $TimeoutSec
            $ok = $r.Exit -eq 37 -and $r.Log -match $faults[$kind] -and $r.Log -match 'KERNEL PANIC: '
            Record "exception diagnostics ($kind)" $ok "exit=$($r.Exit)"
        }

        # --- 4. invalid kernel image + loader error screen ------------------
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'boot.fast' | Out-Null
        $kernelPath = Join-Path $EspDir 'EFI\OSYX\KERNEL.ELF'
        $bytes = [IO.File]::ReadAllBytes($kernelPath)
        $bytes[4] = 1  # ELFCLASS32: must be rejected
        [IO.File]::WriteAllBytes($kernelPath, $bytes)
        New-DiskImage | Out-Null
        $shot = Join-Path $testDir 'loader-error.png'
        if (Test-Path $shot) { Remove-Item $shot }
        $r = Invoke-Boot 'bad-kernel' $TimeoutSec 'BOOT FAILED' $shot
        $ok = $r.Log -match 'BOOT FAILED: Kernel image is invalid: \\EFI\\OSYX\\KERNEL.ELF: not an ELF64 file' `
            -and $r.Log -notmatch 'jumping to kernel'
        Record 'invalid kernel rejected' $ok ''
        Record 'loader error screenshot' (Test-Path $shot) $shot

        # --- 5. kernel screen screenshot ------------------------------------
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline '' | Out-Null
        $shot = Join-Path $testDir 'kernel-running.png'
        if (Test-Path $shot) { Remove-Item $shot }
        $r = Invoke-Boot 'kernel-screen' $TimeoutSec 'entering idle loop' $shot
        $ok = (Test-Path $shot) -and $r.Log -match 'self-tests: \d+ passed, 0 failed' -and $r.Log -match 'user checks: \d+ passed, 0 failed'
        Record 'kernel screen screenshot' $ok $shot

        # --- 6. graphical session ---------------------------------------------
        # PRISM, the desktop shell and the applications driven through the
        # USB keyboard and mouse (scripts/desktop-test.ps1).
        $out = & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'desktop-test.ps1') -TimeoutSec $TimeoutSec 2>&1 | ForEach-Object { "$_" }
        foreach ($line in $out) {
            if ($line -match '^\[(PASS|FAIL)\] (.*)$') { $results.Add([pscustomobject]@{ Test = "desktop: $($Matches[2])"; Result = $Matches[1]; Detail = '' }); Write-Host $line }
        }
        $sum = $out | Select-String -Pattern 'desktop tests: (\d+) passed, (\d+) failed' | Select-Object -Last 1
        Record 'graphical session (PRISM + desktop) run' ($null -ne $sum -and $sum.Matches[0].Groups[2].Value -eq '0') $(if ($sum) { $sum.Line } else { 'no summary' })

        # --- 7. network ---------------------------------------------------------
        # VirtIO-Net, DHCP, ICMP, DNS, UDP and TCP against an independent host
        # process (scripts/net-test.ps1).
        $out = & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'net-test.ps1') 2>&1 | ForEach-Object { "$_" }
        foreach ($line in $out) {
            if ($line -match '^\[(PASS|FAIL)\] (.*)$') { $results.Add([pscustomobject]@{ Test = "net: $($Matches[2])"; Result = $Matches[1]; Detail = '' }); Write-Host $line }
        }
        $sum = $out | Select-String -Pattern 'network test: (\d+) passed, (\d+) failed' | Select-Object -Last 1
        Record 'network stack run' ($null -ne $sum -and $sum.Matches[0].Groups[2].Value -eq '0') $(if ($sum) { $sum.Line } else { 'no summary' })
        # Leave a usable default ESP behind.
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline '' | Out-Null
    }
} finally {
    Pop-Location
}

Write-Host ''
$results | Format-Table -AutoSize | Out-String | Write-Host
$failedCount = @($results | Where-Object { $_.Result -eq 'FAIL' }).Count
if ($failedCount -gt 0) { Write-Host "$failedCount test(s) FAILED"; exit 1 }
Write-Host 'All tests passed'
