# Network benchmark: TCP and UDP throughput and latency in QEMU.
#
# Boots OSyx headless with a NIC on QEMU's user-mode network and runs
# `init netbench,<tcp>,<udp>,<MiB>` against scripts/net-bench-host.py
# (10.0.2.2 from the guest) and over loopback inside the guest. Every
# `netbench: <name> <value> <unit>` line of the serial log becomes one
# result; with -Runs > 1 the median of each is reported.
#
# Comparing builds: -Baseline <image> boots that disk image (a saved copy of
# an earlier build/osyx.img) and the candidate in turn, run after run, so a
# change in host load hits both alike, and prints both medians with the
# gain. -Save writes the candidate's results to build/test/netbench-<label>.json
# and -Compare prints them next to an earlier saved run.
#
# -Only runs just the benchmarks whose name starts with it; -Prof <seconds>
# builds with frame pointers, boots with the kernel profiler and renders
# build/flame/netbench (scripts/flamegraph.py); -Pcap records each run's
# traffic to build/test/netbench.<run>.pcap.
param(
    [switch]$SkipBuild,
    [ValidateSet('virtio', 'e1000', 'e1000e', 'rtl8139')][string]$Nic = 'virtio',
    [ValidateRange(1, 16)][int]$Cpus = 1,
    # Bytes per TCP transfer (loopback moves four times as much).
    [ValidateRange(1, 1024)][int]$MiB = 16,
    [ValidateRange(1, 20)][int]$Runs = 3,
    [string]$Only = '',
    [ValidateRange(0, 600)][int]$Prof = 0,
    [string]$Save = '',
    [string]$Compare = '',
    [ValidateSet('auto', 'whpx', 'tcg')][string]$Accel = 'auto',
    [switch]$Pcap,
    # The candidate disk image (default build/osyx.img; implies -SkipBuild).
    [string]$Image = '',
    [string]$Baseline = '',
    [int]$TimeoutSec = 300
)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$tcpPort = 7103; $udpPort = 7104

function Read-Shared([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    $fs = [IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite')
    try { return ((New-Object IO.StreamReader($fs)).ReadToEnd() -replace "`r", '') } finally { $fs.Close() }
}

if ($Image) { $SkipBuild = $true }
if (-not $SkipBuild) {
    $cmdline = "init.args=tests,netbench,$tcpPort,$udpPort,$MiB$(if ($Only) { ",$Only" }) test.exit"
    $fp = $env:OSYX_FRAME_POINTERS
    if ($Prof) { $cmdline += " prof=$Prof"; $env:OSYX_FRAME_POINTERS = '1' }
    try { & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline $cmdline | Out-Host } finally { $env:OSYX_FRAME_POINTERS = $fp }
}
$candidate = if ($Image) { $Image } else { Join-Path $BuildDir 'osyx.img' }

# Results per image: name -> samples, plus units and the order seen.
$sets = [ordered]@{}
foreach ($k in @(if ($Baseline) { 'baseline' }) + 'candidate') {
    $sets[$k] = @{ samples = @{}; order = (New-Object System.Collections.Generic.List[string]) }
}
$units = @{}
$failures = 0

# Boots `$disk` once and adds its results to `$set`.
function Invoke-Run([string]$disk, $set, [string]$tag) {
    $log = Join-Path $testDir "netbench.$tag.serial.log"
    $hostLog = Join-Path $testDir "netbench-host.$tag.log"
    foreach ($f in $log, $hostLog) { if (Test-Path $f) { Remove-Item $f } }
    $py = (Get-Command python).Source
    $server = Start-Process -FilePath $py -ArgumentList @((Join-Path $PSScriptRoot 'net-bench-host.py'), $tcpPort, $udpPort, $hostLog) -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 800
    try {
        $nicDevice = @{ virtio = 'virtio-net-pci'; e1000 = 'e1000'; e1000e = 'e1000e'; rtl8139 = 'rtl8139' }[$Nic]
        $qargs = Get-QemuArgs -SerialLog $log -Headless -MemoryMiB 1024 -Nic $nicDevice -CpuCount $Cpus -Accel $Accel -DiskFile $disk
        if ($Pcap) { $qargs += @('-object', "filter-dump,id=dump0,netdev=n0,file=$(Join-Path $testDir "netbench.$tag.pcap")") }
        $qemu = Start-Qemu $qargs
        if (-not $qemu.WaitForExit($TimeoutSec * 1000)) {
            $qemu.Kill()
            Write-Host "${tag}: no exit within $TimeoutSec s"
            $script:failures++
        }
    } finally {
        if (-not $server.HasExited) { Stop-Process -Id $server.Id -Force }
    }
    $text = Read-Shared $log
    if ($text -match 'KERNEL PANIC') { Write-Host "${tag}: kernel panic"; $script:failures++ }
    $m = [regex]::Match($text, 'netbench: done, (\d+) failed')
    if (-not $m.Success -or $m.Groups[1].Value -ne '0') { Write-Host "${tag}: $($m.Value)"; $script:failures++ }
    foreach ($r in [regex]::Matches($text, 'netbench: (\w+) ([\d.]+) (\S+)')) {
        $name = $r.Groups[1].Value
        if (-not $set.samples.ContainsKey($name)) { $set.samples[$name] = New-Object System.Collections.Generic.List[double]; $set.order.Add($name) }
        $set.samples[$name].Add([double]::Parse($r.Groups[2].Value, [Globalization.CultureInfo]::InvariantCulture))
        $script:units[$name] = $r.Groups[3].Value
    }
    foreach ($r in [regex]::Matches($text, 'netbench: (\w+) failed')) { Write-Host "${tag}: $($r.Value)" }
    Write-Host ("{0}: {1} results" -f $tag, ([regex]::Matches($text, 'netbench: \w+ [\d.]+ ')).Count)
}

foreach ($run in 1..$Runs) {
    if ($Baseline) { Invoke-Run $Baseline $sets['baseline'] "baseline.$run" }
    Invoke-Run $candidate $sets['candidate'] "$run"
}

function Median([System.Collections.Generic.List[double]]$v) {
    $s = @($v | Sort-Object)
    if ($s.Count % 2) { return $s[[int][math]::Floor($s.Count / 2)] }
    return ($s[$s.Count / 2 - 1] + $s[$s.Count / 2]) / 2
}

function Get-Result($set) {
    $r = [ordered]@{}
    foreach ($name in $set.order) {
        $r[$name] = [ordered]@{ median = (Median $set.samples[$name]); unit = $units[$name]; samples = @($set.samples[$name]) }
    }
    return $r
}

$result = Get-Result $sets['candidate']
$before = $null
if ($Baseline) {
    $before = Get-Result $sets['baseline']
} elseif ($Compare) {
    $path = if (Test-Path $Compare) { $Compare } else { Join-Path $testDir "netbench-$Compare.json" }
    $before = Get-Content $path -Raw | ConvertFrom-Json
}
$rows = foreach ($name in $result.Keys) {
    $r = $result[$name]
    $row = [ordered]@{ Benchmark = $name; Median = ('{0:N2}' -f $r.median); Unit = $r.unit; Samples = (($r.samples | ForEach-Object { '{0:N1}' -f $_ }) -join ' ') }
    $b = if ($before -is [System.Collections.IDictionary]) { $before[$name] } elseif ($before) { $before.$name } else { $null }
    if ($b) {
        $bm = [double]$b.median
        $row['Before'] = '{0:N2}' -f $bm
        if ($Baseline) { $row['Before samples'] = (($b.samples | ForEach-Object { '{0:N1}' -f $_ }) -join ' ') }
        # Lower is better for times, higher for rates.
        if ($bm -gt 0) {
            $x = if ($r.unit -eq 'us') { $bm / [math]::Max($r.median, 1e-9) } else { $r.median / $bm }
            $row['Gain'] = '{0:N2}x' -f $x
        }
    }
    [pscustomobject]$row
}
$rows | Format-Table -AutoSize | Out-String -Width 250 | Write-Host
if ($Prof) {
    $flame = Join-Path $BuildDir 'flame/netbench'
    python (Join-Path $PSScriptRoot 'flamegraph.py') (Join-Path $testDir "netbench.$Runs.serial.log") --out $flame --title 'OSyx netbench' | Out-Host
}
if ($Save) {
    $out = Join-Path $testDir "netbench-$Save.json"
    $result | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $out
    Write-Host "saved $out"
    if ($Baseline) {
        $out = Join-Path $testDir "netbench-$Save-baseline.json"
        $before | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $out
        Write-Host "saved $out"
    }
}
Write-Host "network benchmark: $Runs run(s)$(if ($Baseline) { ' each' }), $failures failure(s)"
exit $failures
