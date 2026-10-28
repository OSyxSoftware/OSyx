# Real multicore userspace execution, affinity, atomics and TLB shootdowns.
param([switch]$SkipBuild, [int]$TimeoutSec = 90, [int[]]$CpuCounts = @(2,4))
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$sessionDisk = Join-Path $testDir ('smp-session-' + [Guid]::NewGuid().ToString('N') + '.img')
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline 'init.args=tests,smp test.exit boot.fast' -OutputImage $sessionDisk | Out-Null
} else {
    $copyLock = Enter-BuildLock
    try { Copy-Item -LiteralPath $DiskImage -Destination $sessionDisk }
    finally { $copyLock.ReleaseMutex(); $copyLock.Dispose() }
}
$failed = 0
foreach ($count in $CpuCounts) {
    if ($count -lt 2 -or $count -gt 16) { throw 'SMP test needs 2 through 16 CPUs' }
    $log = Join-Path $testDir "smp-$count.serial.log"
    $vm = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -MemoryMiB 1024 -CpuCount $count -DiskFile $sessionDisk)
    try {
        $finished = $vm.WaitForExit($TimeoutSec * 1000)
        if (-not $finished) { $vm.Kill(); $vm.WaitForExit() }
        $text = [IO.File]::ReadAllText($log)
        $seen = @([regex]::Matches($text, 'SMP_CPU logical=(\d+) apic=(\d+) online=(\d+)'))
        $ok = $finished -and $vm.ExitCode -eq 33 -and $text -match 'smp checks: 5 passed, 0 failed' -and $seen.Count -eq $count -and $text -notmatch 'KERNEL PANIC'
        if (-not $ok) { $failed++; Write-Host "[FAIL] SMP with $count CPUs: $log" }
        else { Write-Host "[PASS] SMP with $count CPUs: distinct APICs, affinity, atomics, syscall contention, TLB invalidation" }
    } finally {
        if (-not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
    }
}
exit $failed
