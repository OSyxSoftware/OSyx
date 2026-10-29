# USB hub test: the USB keyboard and mouse sit behind a hub (QEMU usb-hub
# on root port 1 of the xHCI controller), not on root ports. The kernel
# must configure the hub, attach both devices through it, and a key typed
# on that keyboard must reach the desktop (Super opens the launcher).
param([switch]$SkipBuild, [int]$TimeoutSec = 120)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$failed = 0
function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
    if (-not $Ok) { $script:failed++ }
}
$log = Join-Path $testDir 'usbhub.serial.log'
$disk = Join-Path $testDir 'usbhub.img'
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline 'init.args=tests,desktop' -OutputImage $disk | Out-Null
} else {
    Copy-Item -LiteralPath $DiskImage -Destination $disk
}
Remove-Item $log -ErrorAction SilentlyContinue
$qmp = Get-Random -Minimum 20000 -Maximum 40000
$a = Get-QemuArgs -SerialLog $log -Headless -NoUsb -NoNet -MemoryMiB 1024 -QmpPort $qmp -DiskFile ($disk.Substring($Root.Length + 1))
$a += @('-device', 'qemu-xhci,id=xhci', '-device', 'usb-hub,bus=xhci.0,port=1,id=hub',
        '-device', 'usb-kbd,bus=xhci.0,port=1.1,id=kbd', '-device', 'usb-mouse,bus=xhci.0,port=1.2,id=mouse')
function Text { if (-not (Test-Path $log)) { return '' }; $fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite'); try { (New-Object IO.StreamReader($fs)).ReadToEnd() } finally { $fs.Close() } }
function Wait([string]$Pattern, [int]$Seconds) {
    $end = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $end) { $m = [regex]::Match((Text), $Pattern); if ($m.Success) { return $m }; Start-Sleep -Milliseconds 300 }
    return $null
}
Push-Location $Root
try { $vm = Start-Qemu $a } finally { Pop-Location }
try {
    $m = Wait 'usb [^:]+: hub with (\d+) ports' $TimeoutSec
    Record 'hub configured' ($null -ne $m) $(if ($m) { $m.Value } else { '' })
    $k = Wait '"QEMU USB Keyboard" \(keyboard\) attached' 30
    $mo = Wait '"QEMU USB Mouse" \(mouse\) attached' 30
    Record 'keyboard and mouse behind the hub attached' ($null -ne $k -and $null -ne $mo) ''
    $p1 = Wait 'usb [^\n]*\(QEMU USB Hub\): port 1: [^\n]* device connected' 5
    $p2 = Wait 'usb [^\n]*\(QEMU USB Hub\): port 2: [^\n]* device connected' 5
    Record 'devices enumerated through hub ports 1 and 2' ($null -ne $p1 -and $null -ne $p2) ''
    $null = Wait 'desktop: shell ready' 60
    Start-Sleep -Seconds 2
    Invoke-Qmp $qmp '{"execute":"send-key","arguments":{"keys":[{"type":"qcode","data":"meta_l"}],"hold-time":60}}' | Out-Null
    $l = Wait 'desktop: launcher opened' 15
    Record 'a key on the keyboard behind the hub reaches the desktop' ($null -ne $l) ''
} finally {
    if (-not $vm.HasExited) { $vm.Kill() }
    Remove-Item $disk -ErrorAction SilentlyContinue
}
Record 'no kernel panic' ((Text) -notmatch 'KERNEL PANIC') ''
Write-Host "usb hub test: $failed failed"
exit $failed
