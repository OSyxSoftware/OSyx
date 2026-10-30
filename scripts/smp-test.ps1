# Real multicore userspace execution, affinity, atomics and TLB shootdowns,
# plus scheduler priorities (inheritance, boost). Pointer events are sent
# over QMP all along so the input boost check has something to wait for.
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
    $qmp = Get-Random -Minimum 20000 -Maximum 40000
    $vm = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -MemoryMiB 1024 -CpuCount $count -DiskFile $sessionDisk -QmpPort $qmp)
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
        $move = '{"execute":"input-send-event","arguments":{"events":[{"type":"rel","data":{"axis":"x","value":1}}]}}'
        while (-not $vm.HasExited -and [DateTime]::UtcNow -lt $deadline) {
            try { [void](Invoke-Qmp $qmp $move) } catch { }
            Start-Sleep -Milliseconds 100
        }
        $finished = $vm.WaitForExit(1000)
        if (-not $finished) { $vm.Kill(); $vm.WaitForExit() }
        $text = [IO.File]::ReadAllText($log)
        $seen = @([regex]::Matches($text, 'SMP_CPU logical=(\d+) apic=(\d+) online=(\d+)'))
        $ok = $finished -and $vm.ExitCode -eq 33 -and $text -match 'smp checks: 10 passed, 0 failed' -and $seen.Count -eq $count -and $text -notmatch 'KERNEL PANIC'
        if (-not $ok) { $failed++; Write-Host "[FAIL] SMP with $count CPUs: $log" }
        else { Write-Host "[PASS] SMP with $count CPUs: distinct APICs, affinity, atomics, syscall contention, TLB invalidation" }
    } finally {
        if (-not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
    }
}
exit $failed
