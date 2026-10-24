# Network integration test.
#
# Boots OSyx with a virtio-net NIC on QEMU's user-mode network and runs
# `init net,<tcp>,<udp>`: DHCP, ICMP echo to the gateway, DNS, UDP and TCP
# against echo servers in an independent host process (scripts/net-host.py,
# reached by the guest as 10.0.2.2), concurrency, refusal, loopback and
# socket cleanup. While init's TCP server runs, this script connects to it
# from the host through a QEMU port forward. Evidence comes from the
# guest's serial log and the host server's own log.
param(
    [switch]$SkipBuild,
    [int]$TimeoutSec = 240
)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$results = New-Object System.Collections.Generic.List[object]
$log = Join-Path $testDir 'net.serial.log'
$hostLog = Join-Path $testDir 'net-host.log'
$tcpPort = 7101; $udpPort = 7102; $fwdPort = 5555

function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    $results.Add([pscustomobject]@{ Test = $Name; Result = $(if ($Ok) { 'PASS' } else { 'FAIL' }); Detail = $Detail })
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
}

function Read-Shared([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    $fs = [IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite')
    try { return ((New-Object IO.StreamReader($fs)).ReadToEnd() -replace "`r", '') } finally { $fs.Close() }
}

function Wait-Log([string]$Pattern, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $m = [regex]::Match((Read-Shared $log), $Pattern)
        if ($m.Success) { return $m }
        if ($script:qemu.HasExited) { return $null }
        Start-Sleep -Milliseconds 200
    }
    return $null
}

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline "init.args=tests,net,$tcpPort,$udpPort test.exit" | Out-Host
}
foreach ($f in $log, $hostLog) { if (Test-Path $f) { Remove-Item $f } }

$py = (Get-Command python).Source
$server = Start-Process -FilePath $py -ArgumentList @((Join-Path $PSScriptRoot 'net-host.py'), $tcpPort, $udpPort, $hostLog) -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 800
try {
    $qargs = Get-QemuArgs -SerialLog $log -Headless -MemoryMiB 512 -HostFwd "hostfwd=tcp:127.0.0.1:$fwdPort-:7"
    $script:qemu = Start-Qemu $qargs

    # The host connects to the guest's server twice while it listens.
    $m = Wait-Log 'net: listening for the host on port 7' 180
    $fromHost = @()
    if ($m) {
        foreach ($i in 1..2) {
            $text = "hello $i from the host side"
            $ok = $false
            try {
                $c = New-Object System.Net.Sockets.TcpClient('127.0.0.1', $fwdPort)
                $s = $c.GetStream(); $s.ReadTimeout = 10000
                $bytes = [Text.Encoding]::ASCII.GetBytes($text)
                $s.Write($bytes, 0, $bytes.Length)
                $c.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send)
                $buf = New-Object byte[] 4096; $got = ''
                while (($n = $s.Read($buf, 0, $buf.Length)) -gt 0) { $got += [Text.Encoding]::ASCII.GetString($buf, 0, $n) }
                $ok = ($got -eq $text)
                $c.Close()
            } catch { $got = "error: $_" }
            $fromHost += $ok
            Record "host -> guest TCP connection $i through the port forward" $ok "echo: '$got'"
        }
    } else {
        Record 'host -> guest TCP connection' $false 'the guest server never started'
    }
    if (-not $script:qemu.WaitForExit($TimeoutSec * 1000)) {
        $script:qemu.Kill()
        Record 'QEMU finished' $false "no exit within $TimeoutSec s"
    }
} finally {
    if (-not $server.HasExited) { Stop-Process -Id $server.Id -Force }
}

$text = Read-Shared $log
$hostText = Read-Shared $hostLog
Copy-Item $log (Join-Path $testDir 'net-last.serial.log') -Force

$m = [regex]::Match($text, 'virtio-net eth0: \S+ at (\S+), MAC (\S+), MTU (\d+), link (\w+).*?(MSI-X vector 0x[0-9a-f]+|polled)')
Record 'VirtIO-Net initialised' ($m.Success -and $m.Groups[4].Value -eq 'up') $m.Value
$m = [regex]::Match($text, 'net: eth0: DHCP lease (\S+) from (\S+), router (\S+), DNS \[([^\]]*)\]')
Record 'DHCP lease from QEMU' ($m.Success -and $m.Groups[1].Value -eq '10.0.2.15/24') $m.Value
foreach ($chk in [regex]::Matches($text, '\[user\] (net: [^\n]*?) \.\.\. (ok|FAILED)')) {
    Record $chk.Groups[1].Value ($chk.Groups[2].Value -eq 'ok') ''
}
$m = [regex]::Match($text, '(\d+) packets transmitted, (\d+) received, (?:\d+ errors, )?(\d+)% packet loss[^\n]*\n[^\n]*rtt min/avg/max = ([\d./]+) ms')
Record '/bin/ping statistics show real replies' ($m.Success -and $m.Groups[2].Value -eq $m.Groups[1].Value) ($m.Value -replace "`n", ' | ')
$m = [regex]::Match($text, 'net: TCP echo of \d+ bytes took \d+ ms \(\d+ KiB/s each way\)')
Record 'TCP throughput measured' $m.Success $m.Value
$conns = @([regex]::Matches($hostText, 'tcp \S+ echoed (\d+) bytes') | Where-Object { [int]$_.Groups[1].Value -gt 0 }).Count
Record 'host server saw the guest''s TCP connections' ($conns -ge 5) "$conns connections with data in net-host.log"
$udp = @([regex]::Matches($hostText, 'udp \d+ bytes from')).Count
Record 'host server saw the guest''s UDP datagrams' ($udp -ge 5) "$udp datagrams in net-host.log"
$m = [regex]::Match($text, 'net checks: (\d+) passed, (\d+) failed')
Record 'guest net checks summary' ($m.Success -and $m.Groups[2].Value -eq '0') $m.Value
$m = [regex]::Match($text, 'irq: vector (0x[0-9a-f]+) fired (\d+) times')
$m2 = [regex]::Match($text, 'net: eth0 \(virtio-net at [^)]*\): (\d+) frames in, (\d+) out, (\d+) rx errors[^\n]*?(\d+) interrupts')
Record 'NIC frames and interrupts counted' ($m2.Success -and [int]$m2.Groups[1].Value -gt 0 -and [int]$m2.Groups[4].Value -gt 0) $m2.Value
Record 'no kernel panic' (-not ($text -match 'KERNEL PANIC')) ''
Record 'kernel exited cleanly' ($text -match 'test.exit: leaving QEMU \(success\)') ''

$results | Format-Table -AutoSize | Out-String | Write-Host
$failed = @($results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host "network test: $($results.Count - $failed) passed, $failed failed"
exit $failed
