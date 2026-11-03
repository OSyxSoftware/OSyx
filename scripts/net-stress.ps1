# Network overload and recovery test.
#
# Boots OSyx with a NIC on QEMU's user-mode network and runs
# `init netstress,<tcp>,<udp>` (userspace/apps/src/net_stress.rs) against
# scripts/net-bench-host.py: the network is checked, overloaded (hundreds of
# connections the host never closes, full buffers both ways, UDP floods),
# and must then work again by itself and after a reconnect (DHCP again, and
# down then DHCP), with no reboot. Evidence comes from the serial log.
param(
    [switch]$SkipBuild,
    [ValidateSet('virtio', 'e1000', 'e1000e', 'rtl8139')][string]$Nic = 'virtio',
    [ValidateRange(1, 16)][int]$Cpus = 1,
    # Boot this disk image instead of build/osyx.img (implies -SkipBuild).
    [string]$Image = '',
    # Record the guest's traffic to build/test/netstress.pcap.
    [switch]$Pcap,
    [int]$TimeoutSec = 420
)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$tcpPort = 7103; $udpPort = 7104
$log = Join-Path $testDir 'netstress.serial.log'
$hostLog = Join-Path $testDir 'netstress-host.log'
$results = New-Object System.Collections.Generic.List[object]

function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    $results.Add([pscustomobject]@{ Test = $Name; Result = $(if ($Ok) { 'PASS' } else { 'FAIL' }); Detail = $Detail })
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
}

function Read-Shared([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    $fs = [IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite')
    try { return ((New-Object IO.StreamReader($fs)).ReadToEnd() -replace "`r", '') } finally { $fs.Close() }
}

if ($Image) { $SkipBuild = $true }
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline "init.args=tests,netstress,$tcpPort,$udpPort test.exit" | Out-Host
}
foreach ($f in $log, $hostLog) { if (Test-Path $f) { Remove-Item $f } }
$py = (Get-Command python).Source
$server = Start-Process -FilePath $py -ArgumentList @((Join-Path $PSScriptRoot 'net-bench-host.py'), $tcpPort, $udpPort, $hostLog) -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 800
try {
    $nicDevice = @{ virtio = 'virtio-net-pci'; e1000 = 'e1000'; e1000e = 'e1000e'; rtl8139 = 'rtl8139' }[$Nic]
    $disk = if ($Image) { $Image } else { Join-Path $BuildDir 'osyx.img' }
    $qargs = Get-QemuArgs -SerialLog $log -Headless -MemoryMiB 1024 -Nic $nicDevice -CpuCount $Cpus -DiskFile $disk
    if ($Pcap) { $qargs += @('-object', "filter-dump,id=dump0,netdev=n0,file=$(Join-Path $testDir 'netstress.pcap')") }
    $qemu = Start-Qemu $qargs
    if (-not $qemu.WaitForExit($TimeoutSec * 1000)) {
        $qemu.Kill()
        Record 'QEMU finished' $false "no exit within $TimeoutSec s"
    }
} finally {
    if (-not $server.HasExited) { Stop-Process -Id $server.Id -Force }
}
$text = Read-Shared $log
Copy-Item $log (Join-Path $testDir 'netstress-last.serial.log') -Force
foreach ($m in [regex]::Matches($text, 'netstress: (\(info\) [^\n]*)')) { Write-Host $m.Groups[1].Value }
foreach ($m in [regex]::Matches($text, 'netstress: ([^\n]*?) (ok|FAILED) ?([^\n]*)')) {
    Record $m.Groups[1].Value ($m.Groups[2].Value -eq 'ok') $m.Groups[3].Value
}
$m = [regex]::Match($text, 'netstress: done, (\d+) failed')
Record 'guest stress summary' ($m.Success -and $m.Groups[1].Value -eq '0') $m.Value
foreach ($m in [regex]::Matches($text, 'net: [^\n]*(stalled|reset)[^\n]*')) { Write-Host "kernel: $($m.Value)" }
Record 'no kernel panic' (-not ($text -match 'KERNEL PANIC')) ''
Record 'kernel exited cleanly' ($text -match 'test.exit: leaving QEMU \(success\)') ''
$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host "network stress test: $($results.Count - $failed) passed, $failed failed"
exit $failed
