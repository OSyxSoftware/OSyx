# Exercise compressed reclaim and restoration with real user page faults.
param([switch]$SkipBuild, [int]$MemoryMiB = 192, [int]$CpuCount = 2, [int]$TimeoutSec = 180)
. (Join-Path $PSScriptRoot 'common.ps1')
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -RootMiB 256 -Cmdline 'test.exit boot.fast init.args=tests,swap-stress'
}
$log = Join-Path $BuildDir 'swap-stress.serial.log'
if (Test-Path -LiteralPath $log) { Remove-Item -LiteralPath $log }
$vm = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -MemoryMiB $MemoryMiB -CpuCount $CpuCount)
try {
    if (-not $vm.WaitForExit($TimeoutSec * 1000)) { throw "swap stress timed out after $TimeoutSec seconds" }
    $serial = [IO.File]::ReadAllText($log)
    $counts = [regex]::Match($serial, 'swap: stats pages=(\d+) bytes=(\d+) out=(\d+) in=(\d+)')
    if ($vm.ExitCode -ne 33 -or $serial -notmatch 'swap-stress: verified \d+ pages' -or
        -not $counts.Success -or [long]$counts.Groups[1].Value -ne 0 -or [long]$counts.Groups[2].Value -ne 0 -or
        [long]$counts.Groups[3].Value -eq 0 -or [long]$counts.Groups[4].Value -eq 0) {
        throw "swap stress failed or did not exercise both reclaim and restoration; see $log"
    }
    Write-Host "PASS: $($counts.Value); log: $log"
} finally {
    if (-not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
}
