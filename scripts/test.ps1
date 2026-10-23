# Full OSyx test suite:
#   1. host unit tests (cargo test) and clippy
#   2. QEMU boot test: loader -> kernel -> in-kernel self-tests (incl. threads,
#      preemption, FPU, PCI, virtio-blk, VFS) -> ring-3 init running its checks
#      (incl. os_std::fs on the FAT32 data disk) with worker child processes
#      -> exit 33; leak and preemption checks on the log; the files OSyx wrote
#      are then read back on the host with mtools; variants for user faults,
#      user panics, exit codes, no data disk and a boot without programs
#   3. controlled CPU exception tests (#PF, #GP, #UD, #DE) -> diagnostics + exit 37
#   4. invalid kernel image -> boot loader error screen
#   5. screenshots of the kernel screen and the loader error screen via QMP
# Serial logs and screenshots are kept in build/test/.
param(
    [switch]$SkipHost,
    [switch]$SkipQemu,
    [int]$TimeoutSec = 120
)
. (Join-Path $PSScriptRoot 'common.ps1')
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
function Invoke-Boot([string]$Name, [int]$Timeout, [string]$WaitPattern = '', [string]$Screenshot = '', [switch]$NoDataDisk) {
    $log = Join-Path $testDir "$Name.serial.log"
    if (Test-Path $log) { Remove-Item $log }
    $port = 0
    if ($Screenshot) { $port = Get-Random -Minimum 20000 -Maximum 40000 }
    $p = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -QmpPort $port -NoDataDisk:$NoDataDisk)
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
        try { $out = (Invoke-Checked 'cargo test' { cargo test --workspace --exclude osyx-kernel --exclude osyx-uefi-loader --exclude osyx-apps }) -join "`n"; $ok = $true } catch { $ok = $false; $out += $_ }
        $passed = ([regex]::Matches($out, 'test result: ok\. (\d+) passed') | ForEach-Object { [int]$_.Groups[1].Value } | Measure-Object -Sum).Sum
        Record 'host unit tests' $ok "$passed tests passed"
        Write-Host '==> clippy (host crates + freestanding targets)'
        $ok = $true
        try {
            Invoke-Checked 'clippy host' { cargo clippy --workspace --exclude osyx-kernel --exclude osyx-uefi-loader --exclude osyx-apps --all-targets -- -D warnings } | Out-Null
            Invoke-Checked 'clippy user' { cargo clippy -p osyx-apps -p os-std -p os-sys --target x86_64-unknown-none --target-dir target/user -- -D warnings } | Out-Null
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
            -and $r.Log -match 'scheduler: \d+ context switches, 2 threads alive'
        Record 'process teardown without leaks' $ok "$released processes released cleanly"

        # Preemption between user processes: three children that never yield
        # must all have started before the first one finishes.
        $lines = $r.Log -split "`n"
        $starts = @($lines | Select-String -Pattern 'spin \d+: started').Count
        $firstDone = ($lines | Select-String -Pattern 'spin \d+: finished' | Select-Object -First 1).LineNumber
        $lastStart = ($lines | Select-String -Pattern 'spin \d+: started' | Select-Object -Last 1).LineNumber
        $took = [regex]::Match($r.Log, '3 x 300 ms spinners finished in (\d+) ms')
        $ok = $starts -eq 3 -and $firstDone -and $lastStart -lt $firstDone -and $took.Success -and [int]$took.Groups[1].Value -lt 650
        Record 'preemptive scheduling of user processes' $ok $took.Value

        # Storage: PCI scan, both virtio-blk disks, the FAT32 data disk at /,
        # and the kernel-side block and VFS self-tests.
        $ok = $r.Log -match 'virtio-blk vda: .*read-only' -and $r.Log -match 'virtio-blk vdb: .*131072 sectors' `
            -and $r.Log -match 'vfs: vdb mounted at / \(FAT32 "OSYXDATA"' `
            -and $r.Log -match '\[test\] PCI enumeration \.\.\. ok' -and $r.Log -match '\[test\] block device I/O \.\.\. ok' `
            -and $r.Log -match '\[test\] VFS files and directories \.\.\. ok'
        Record 'PCI, virtio-blk, block layer and VFS mount' $ok ([regex]::Match($r.Log, 'storage: [^\n]*').Value)
        $fsOk = [regex]::Matches($r.Log, '\[user\] fs: [^\n]* \.\.\. ok').Count
        $ok = $fsOk -eq 12 -and $r.Log -notmatch '\[user\] fs: [^\n]*FAILED' -and $r.Log -match 'closed 11 open handles'
        Record 'OSyx Std fs from ring 3 (files, dirs, handles)' $ok "$fsOk fs checks passed"

        # The files init left on the data disk, read back on the host by
        # mtools (an independent FAT implementation) after QEMU has exited.
        $mtype = Find-Tool 'mtype' @('C:\msys64\ucrt64\bin\mtype.exe')
        $mcopy = Find-Tool 'mcopy' @('C:\msys64\ucrt64\bin\mcopy.exe')
        $ok = $false; $detail = 'mtools not found'
        if ($mtype -and $mcopy) {
            Push-Location $BuildDir
            try {
                $saved = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
                $text = (& $mtype -i data.img ::/osyx-out/hello-from-osyx.txt) -join "`n"
                $fromWorker = (& $mtype -i data.img ::/osyx-out/from-worker.txt) -join "`n"
                if (Test-Path pattern.bin) { Remove-Item pattern.bin }
                & $mcopy -n -i data.img ::/osyx-out/pattern.bin pattern.bin 2>&1 | Out-Null
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
                $check = (Invoke-Checked 'fatcheck' { cargo run -q -p osyx-fat --example fatcheck -- data.img }) -join ' '
                $ok = $text -eq 'Written by OSyx init through os_std::fs.' -and $fromWorker -match '^written by worker \d+$' `
                    -and $patternOk -and $check -match 'FSInfo accurate'
                $detail = "text=$($text -ne '') pattern=$patternOk; $check"
            } catch { $detail = "$_" } finally { Pop-Location }
        }
        Record 'host reads OSyx-written files (mtools + fsck)' $ok $detail

        # Without the data disk the storage self-tests and init's fs checks
        # must skip cleanly rather than fail.
        $r = Invoke-Boot 'boot-no-data-disk' $TimeoutSec -NoDataDisk
        $user = [regex]::Match($r.Log, 'user checks: (\d+) passed, (\d+) failed')
        $ok = $r.Exit -eq 33 -and $r.Log -match 'self-tests: \d+ passed, 0 failed' -and $user.Success `
            -and $user.Groups[2].Value -eq '0' -and $r.Log -match 'no volume mounted at /: file system checks skipped' `
            -and $r.Log -match 'skipped: no writable disk attached'
        Record 'boot without data disk' $ok "exit=$($r.Exit) $($user.Value)"

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
        $ok = $r.Exit -eq 33 -and $r.Log -match 'no \\EFI\\OSYX\\BIN directory' -and $r.Log -match 'no init program' `
            -and $r.Log -notmatch 'process 1'
        Record 'boot without init program' $ok "exit=$($r.Exit)"

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
