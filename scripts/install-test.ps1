# End-to-end installation test in QEMU. Only image files are written: the
# live USB image and a blank target disk under build/test (never a
# physical disk).
#
#  1. Live phase: QEMU boots build/osyx-live-usb.img as a USB stick (xHCI
#     usb-storage) with a blank virtio disk. init starts the session with
#     OSyx Setup in unattended mode (`--auto`, confirmed with
#     `--confirm-erase vda`), which partitions, formats and fills the disk,
#     then restarts (QEMU runs with -no-reboot, so it exits).
#  2. The target disk is checked on the host: GPT (sgdisk -v), the ESP
#     (fatcheck) and the root (e2fsck -fn, debugfs), all in WSL/cargo.
#  3. Installed phase: QEMU boots the target disk alone and the serial log
#     must show the ext4 root on vda2, the configured host name, the
#     desktop session and the first-boot service.
param(
    [switch]$SkipBuild,
    [int]$TimeoutSec = 600,
    [int]$DiskMiB = 4096,
    [int]$MemoryMiB = 2048,
    [switch]$Zen
)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
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
function Wait-Pattern([string]$Log, [string]$Pattern, [int]$Seconds, $Proc) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $m = [regex]::Match((Read-Shared $Log), $Pattern)
        if ($m.Success) { return $m }
        if ($Proc.HasExited) { return [regex]::Match((Read-Shared $Log), $Pattern) }
        Start-Sleep -Milliseconds 500
    }
    return $null
}

$usb = Join-Path $BuildDir 'osyx-live-usb.img'
# init.args is one kernel option: no spaces in these values.
$auto = 'setup,--auto,--disk,vda,--confirm-erase,vda,--hostname,osyx-test,--user,tester,--full-name,Tester,--password,secret1,--reboot'
if ($Zen) { $auto += ',--zen' }
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -Live -Cmdline "live init.args=$auto" | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'live build failed' }
}
if (-not (Test-Path $usb)) { throw "$usb is missing (build with -Live)" }
$target = Join-Path $testDir 'install-target.img'
$fs = [IO.File]::Create($target); $fs.SetLength([int64]$DiskMiB * 1MB); $fs.Close()

# ---- 1. live phase ---------------------------------------------------------------
$liveLog = Join-Path $testDir 'install-live.serial.log'
Remove-Item $liveLog -ErrorAction SilentlyContinue
$qmp = Get-Random -Minimum 20000 -Maximum 40000
$args1 = Get-QemuArgs -SerialLog $liveLog -Headless -MemoryMiB $MemoryMiB -QmpPort $qmp -DiskFile ($target.Substring($Root.Length + 1)) -NoNet
$args1 += @('-drive', "if=none,id=stick,format=raw,readonly=on,file=$(($usb.Substring($Root.Length + 1)) -replace '\\','/')",
            '-device', 'usb-storage,bus=xhci.0,drive=stick,bootindex=0')
Push-Location $Root
try { $vm = Start-Qemu $args1 } finally { Pop-Location }
try {
    $m = Wait-Pattern $liveLog 'setup: started' 120 $vm
    Record 'live system starts OSyx Setup' ($null -ne $m -and $m.Success) ''
    Start-Sleep -Seconds 4
    Save-Screenshot $qmp (Join-Path $testDir 'install-progress.png') | Out-Null
    $m = Wait-Pattern $liveLog 'SETUP_RESULT (\w+)(.*)' $TimeoutSec $vm
    $ok = $m -and $m.Success -and $m.Groups[1].Value -eq 'ok'
    Record 'installation finished' $ok $(if ($m) { $m.Value } else { 'no result' })
    if (-not $vm.HasExited) { Save-Screenshot $qmp (Join-Path $testDir 'install-finish.png') | Out-Null }
    $vm.WaitForExit(60000) | Out-Null
    Record 'live system restarted after the installation' ($vm.HasExited) ''
} finally {
    if (-not $vm.HasExited) { $vm.Kill() }
}
$t = Read-Shared $liveLog
Record 'the live medium was refused as a target' ($t -match 'setup: disk \S+ .* in use by the running system' -or $t -notmatch 'setup: disk sd') ''
Record 'no kernel panic in the live phase' ($t -notmatch 'KERNEL PANIC') ''

# ---- 2. host checks ------------------------------------------------------------------
# Tools in WSL write their banners to stderr; that is not a failure here.
$ErrorActionPreference = 'Continue'
$wslImg = ConvertTo-WslPath $target
$gpt = (wsl.exe -d Ubuntu-24.04 -- sgdisk -v $wslImg 2>&1) -join "`n"
Record 'GPT valid (sgdisk -v)' ($gpt -match 'No problems found') ''
$parts = (wsl.exe -d Ubuntu-24.04 -- sgdisk -p $wslImg 2>&1) -join "`n"
$rootStart = [regex]::Match($parts, '(?m)^\s+2\s+(\d+)\s+\d+').Groups[1].Value
$espStart = [regex]::Match($parts, '(?m)^\s+1\s+(\d+)\s+(\d+)')
if ($rootStart) {
    $fsck = (wsl.exe -d Ubuntu-24.04 -- e2fsck -fn "$($wslImg)?offset=$([int64]$rootStart * 512)" 2>&1) -join "`n"
    Record 'root ext4 clean (e2fsck -fn)' ($LASTEXITCODE -eq 0) ([regex]::Match($fsck, 'OSyx: .*').Value)
    $host_ = (wsl.exe -d Ubuntu-24.04 -- debugfs -R 'cat /etc/hostname' "$($wslImg)?offset=$([int64]$rootStart * 512)" 2>$null) -join ''
    Record 'host name written' ($host_.Trim() -eq 'osyx-test') $host_.Trim()
    $passwd = (wsl.exe -d Ubuntu-24.04 -- debugfs -R 'cat /etc/passwd' "$($wslImg)?offset=$([int64]$rootStart * 512)" 2>$null) -join "`n"
    Record 'user account written' ($passwd -match 'tester:x:1000:1000:Tester:/home/user') ''
    $desk = (wsl.exe -d Ubuntu-24.04 -- debugfs -R 'ls /home/user/Desktop' "$($wslImg)?offset=$([int64]$rootStart * 512)" 2>$null) -join ' '
    Record 'installed desktop has no installer shortcut' ($desk -match '10-terminal' -and $desk -notmatch 'setup') $desk.Trim()
}
if ($espStart.Success) {
    $esp = Join-Path $testDir 'install-esp.img'
    $src = [IO.File]::OpenRead($target); $dst = [IO.File]::Create($esp)
    try {
        [void]$src.Seek([int64]$espStart.Groups[1].Value * 512, 'Begin')
        $left = ([int64]$espStart.Groups[2].Value - [int64]$espStart.Groups[1].Value + 1) * 512
        $buf = New-Object byte[] (4MB)
        while ($left -gt 0) { $n = $src.Read($buf, 0, [int][math]::Min($buf.Length, $left)); $dst.Write($buf, 0, $n); $left -= $n }
    } finally { $src.Close(); $dst.Close() }
    $fc = (cargo run -q -p osyx-fat --example fatcheck -- $esp 2>&1) -join ' '
    Record 'ESP FAT32 consistent (fatcheck)' ($LASTEXITCODE -eq 0) $fc
    Remove-Item $esp
}

# ---- 3. installed phase ---------------------------------------------------------------
$diskLog = Join-Path $testDir 'install-boot.serial.log'
Remove-Item $diskLog -ErrorAction SilentlyContinue
$qmp2 = Get-Random -Minimum 20000 -Maximum 40000
$args2 = Get-QemuArgs -SerialLog $diskLog -Headless -MemoryMiB $(if ($Zen) { [math]::Max($MemoryMiB, 6144) } else { $MemoryMiB }) -QmpPort $qmp2 -DiskFile ($target.Substring($Root.Length + 1))
Push-Location $Root
try { $vm2 = Start-Qemu $args2 } finally { Pop-Location }
try {
    $m = Wait-Pattern $diskLog 'storage: .*root.*ext4 on vda2|root: ext4 on vda2|ext4 on vda2' 90 $vm2
    Record 'installed system boots with its ext4 root on vda2' ($null -ne $m -and $m.Success) ''
    $m = Wait-Pattern $diskLog 'prism: compositor running' 90 $vm2
    Record 'installed desktop session starts' ($null -ne $m -and $m.Success) ''
    $m = Wait-Pattern $diskLog 'firstboot: ' 60 $vm2
    Record 'first-boot service runs' ($null -ne $m -and $m.Success) ''
    if ($Zen) {
        $m = Wait-Pattern $diskLog 'firstboot: (downloaded \d+ bytes, SHA-256 verified|using the verified installer)' 900 $vm2
        Record 'official Zen installer downloaded over HTTPS and verified' ($null -ne $m -and $m.Success) $(if ($m) { $m.Value } else { '' })
        $m = Wait-Pattern $diskLog 'firstboot: unpacked (\d+) files .* CRC32 verified' 1800 $vm2
        Record 'Zen unpacked from the official installer (CRC32 verified)' ($null -ne $m -and $m.Success) $(if ($m) { $m.Value } else { '' })
        $m = Wait-Pattern $diskLog 'FIRSTBOOT_RESULT (\w+)' 600 $vm2
        Record 'Zen installed and registered by the first-boot service' ($null -ne $m -and $m.Groups[1].Value -eq 'ok') $(if ($m) { $m.Value } else { 'no result' })
        # Launch Zen from the AXIS desktop: Super opens the launcher, the
        # typed name filters it, Enter starts the entry firstboot added.
        $m = Wait-Pattern $diskLog 'desktop: shell ready' 60 $vm2
        Start-Sleep -Seconds 3
        $key = { param([string[]]$Codes)
            $list = ($Codes | ForEach-Object { "{`"type`":`"qcode`",`"data`":`"$_`"}" }) -join ','
            Invoke-Qmp $qmp2 "{`"execute`":`"send-key`",`"arguments`":{`"keys`":[$list],`"hold-time`":40}}" | Out-Null
            Start-Sleep -Milliseconds 150
        }
        & $key @('meta_l')
        $m = Wait-Pattern $diskLog 'desktop: launcher opened' 20 $vm2
        foreach ($c in 'zen'.ToCharArray()) { & $key @([string]$c) }
        Start-Sleep -Milliseconds 500
        & $key @('ret')
        $m = Wait-Pattern $diskLog 'desktop: launched Zen Browser' 30 $vm2
        Record 'Zen Browser launched from the AXIS launcher' ($null -ne $m -and $m.Success) $(if ($m) { $m.Value } else { '' })
        $m = Wait-Pattern $diskLog 'prism: window \d+ "Zen Browser" mapped' 600 $vm2
        Record 'Zen Browser window appears' ($null -ne $m -and $m.Success) $(if ($m) { $m.Value } else { '' })
        Start-Sleep -Seconds 20
        Save-Screenshot $qmp2 (Join-Path $testDir 'install-zen.png') | Out-Null
    }
    Start-Sleep -Seconds 10
    Save-Screenshot $qmp2 (Join-Path $testDir 'install-installed.png') | Out-Null
} finally {
    if (-not $vm2.HasExited) { $vm2.Kill() }
}
$t2 = Read-Shared $diskLog
Record 'no kernel panic in the installed system' ($t2 -notmatch 'KERNEL PANIC') ''

$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object Result -eq 'FAIL').Count
Write-Host ("install test: {0} passed, {1} failed" -f ($results.Count - $failed), $failed)
exit $failed
