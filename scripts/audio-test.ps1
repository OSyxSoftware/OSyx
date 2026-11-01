# Sound card integration test.
#
# Boots OSyx with `init tests,audio` on QEMU's Intel HD Audio controller
# (intel-hda) and duplex codec (hda-duplex) on QEMU's 'none' audio backend,
# which takes played samples at the card's rate and records silence at it
# (the 'wav' backend cannot record). init checks the kernel's nodes,
# recording pace, playback and recording together and the mixer, then runs
# wavedev.exe (winmm devices). Evidence: the serial log, including the
# driver's lines for the playback and recording rings it started.
param(
    [switch]$SkipBuild,
    [int]$TimeoutSec = 240,
    [ValidateRange(1,16)][int]$CpuCount = 2
)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$results = New-Object System.Collections.Generic.List[object]
$log = Join-Path $testDir 'audio.serial.log'

function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    $results.Add([pscustomobject]@{ Test = $Name; Result = $(if ($Ok) { 'PASS' } else { 'FAIL' }); Detail = $Detail })
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
}

$sessionDisk = Join-Path $testDir ('audio-session-' + [Guid]::NewGuid().ToString('N') + '.img')
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline 'init.args=tests,audio test.exit' -OutputImage $sessionDisk | Out-Host
} else {
    $copyLock = Enter-BuildLock
    try { Copy-Item -LiteralPath $DiskImage -Destination $sessionDisk }
    finally { $copyLock.ReleaseMutex(); $copyLock.Dispose() }
}
foreach ($f in @($log)) { if (Test-Path $f) { Remove-Item $f } }
$qemu = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -MemoryMiB 1024 -CpuCount $CpuCount -DiskFile $sessionDisk -NoNet -Audio none)
if (-not $qemu.WaitForExit($TimeoutSec * 1000)) {
    $qemu.Kill(); $qemu.WaitForExit()
    Record 'run finished in time' $false "no exit within $TimeoutSec s"
}
$fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite')
try { $t = (New-Object IO.StreamReader($fs)).ReadToEnd() -replace "`r", '' } finally { $fs.Close() }

Record 'the kernel found a card that plays and records' ($t -match 'audio: audio0 is \S+ \([0-9a-f:]+\), plays, records') ''
foreach ($m in [regex]::Matches($t, '\[user\] (.+?) \.\.\. (ok|FAILED)')) {
    Record ("init: " + $m.Groups[1].Value) ($m.Groups[2].Value -eq 'ok') ''
}
foreach ($m in [regex]::Matches($t, '\[winrun:\d+\]\s+(ok|FAIL)\s+(.+)')) {
    Record ("wavedev: " + $m.Groups[2].Value.Trim()) ($m.Groups[1].Value -eq 'ok') ''
}
$sum = [regex]::Match($t, 'audio checks: (\d+) passed, (\d+) failed')
Record 'init audio checks summary' ($sum.Success -and $sum.Groups[2].Value -eq '0') $sum.Value

Record 'the driver started a playback ring at 48 kHz' ($t -match 'azalia\d+: playing 48000 Hz') ''
Record 'the driver started a recording ring at 48 kHz' ($t -match 'azalia\d+: recording 48000 Hz') ''
Record 'no kernel panic' ($t -notmatch 'KERNEL PANIC') ''
$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object Result -eq 'FAIL').Count
Write-Host ("audio test: {0} passed, {1} failed" -f ($results.Count - $failed), $failed)
Remove-Item -LiteralPath $sessionDisk -ErrorAction SilentlyContinue
exit $failed
