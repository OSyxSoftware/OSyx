# Windows compatibility integration test.
#
# Boots OSyx with `init win,<tcp>,<udp>`: init runs the PE32+ test programs
# of compatibility/win32/tests under /bin/winrun (minimal, hello, fileio,
# threads + windll.dll, winsock against the host echo servers of
# scripts/net-host.py, and the Win32 GUI program inside a PRISM session).
# Each program prints its own "ok"/"FAIL" lines and a summary; init checks
# the exit codes. Evidence comes from the serial log.
param(
    [switch]$SkipBuild,
    [int]$TimeoutSec = 300
)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$results = New-Object System.Collections.Generic.List[object]
$log = Join-Path $testDir 'win.serial.log'
$hostLog = Join-Path $testDir 'win-net-host.log'
$tcpPort = 7111; $udpPort = 7112

function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    $results.Add([pscustomobject]@{ Test = $Name; Result = $(if ($Ok) { 'PASS' } else { 'FAIL' }); Detail = $Detail })
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
}

function Read-Shared([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    $fs = [IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite')
    try { return ((New-Object IO.StreamReader($fs)).ReadToEnd() -replace "`r", '') } finally { $fs.Close() }
}

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline "init.args=win,$tcpPort,$udpPort test.exit" | Out-Host
}
foreach ($f in $log, $hostLog) { if (Test-Path $f) { Remove-Item $f } }
$py = (Get-Command python).Source
$server = Start-Process -FilePath $py -ArgumentList @((Join-Path $PSScriptRoot 'net-host.py'), $tcpPort, $udpPort, $hostLog) -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 800
try {
    $port = Get-Random -Minimum 20000 -Maximum 40000
    $qemu = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -MemoryMiB 1024 -QmpPort $port)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $shot = $false
    while (-not $qemu.HasExited -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 300
        if (-not $shot -and (Read-Shared $log) -match 'gui: WM_PAINT 2') {
            Save-Screenshot $port (Join-Path $testDir 'win-gui.png') | Out-Null
            $shot = $true
        }
    }
    if (-not $qemu.HasExited) { $qemu.Kill(); Record 'run finished in time' $false "no exit within $TimeoutSec s" }
} finally {
    if ($server -and -not $server.HasExited) { $server.Kill() }
}
$t = Read-Shared $log
# Every program's own checks (lines "  ok   ..." / "  FAIL ..." from winrun).
foreach ($m in [regex]::Matches($t, '\[winrun:\d+\]\s+(ok|FAIL)\s+(.+)')) {
    Record ("win32: " + $m.Groups[2].Value.Trim()) ($m.Groups[1].Value -eq 'ok') ''
}
foreach ($p in 'hello', 'fileio', 'threads', 'winsock', 'gui', 'sysadmin', 'version', 'synch', 'security', 'exceptions', 'mapping') {
    $s = [regex]::Match($t, "\[winrun:\d+\] ${p}: (\d+) passed, (\d+) failed")
    Record "$p.exe summary" ($s.Success -and $s.Groups[2].Value -eq '0') $s.Value
}
foreach ($m in [regex]::Matches($t, '\[user\] (.+?) \.\.\. (ok|FAILED)')) {
    Record ("init: " + $m.Groups[1].Value) ($m.Groups[2].Value -eq 'ok') ''
}
$sum = [regex]::Match($t, 'win checks: (\d+) passed, (\d+) failed')
Record 'init win checks summary' ($sum.Success -and $sum.Groups[2].Value -eq '0') $sum.Value
$h = Read-Shared $hostLog
Record 'host server saw the Winsock TCP echo and UDP datagram' ($h -match 'tcp \S+ echoed 36 bytes' -and $h -match "udp 4 bytes from \S+: b'udp!'") ''
Record 'no kernel panic' ($t -notmatch 'KERNEL PANIC') ''
$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object Result -eq 'FAIL').Count
Write-Host ("windows test: {0} passed, {1} failed" -f ($results.Count - $failed), $failed)
exit $failed
