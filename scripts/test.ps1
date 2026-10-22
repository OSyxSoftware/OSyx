# Full OSyx test suite:
#   1. host unit tests (cargo test) and clippy
#   2. QEMU boot test: loader -> kernel -> in-kernel self-tests -> exit 33
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
function Invoke-Boot([string]$Name, [int]$Timeout, [string]$WaitPattern = '', [string]$Screenshot = '') {
    $log = Join-Path $testDir "$Name.serial.log"
    if (Test-Path $log) { Remove-Item $log }
    $port = 0
    if ($Screenshot) { $port = Get-Random -Minimum 20000 -Maximum 40000 }
    $p = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -QmpPort $port)
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
        try { $out = (Invoke-Checked 'cargo test' { cargo test --workspace --exclude osyx-kernel --exclude osyx-uefi-loader }) -join "`n"; $ok = $true } catch { $ok = $false; $out += $_ }
        $passed = ([regex]::Matches($out, 'test result: ok\. (\d+) passed') | ForEach-Object { [int]$_.Groups[1].Value } | Measure-Object -Sum).Sum
        Record 'host unit tests' $ok "$passed tests passed"
        Write-Host '==> clippy (host crates + freestanding targets)'
        $ok = $true
        try {
            Invoke-Checked 'clippy host' { cargo clippy --workspace --exclude osyx-kernel --exclude osyx-uefi-loader --all-targets -- -D warnings } | Out-Null
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
        $ok = (Test-Path $shot) -and $r.Log -match 'self-tests: \d+ passed, 0 failed'
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
