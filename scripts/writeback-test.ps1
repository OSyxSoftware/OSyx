# Delayed write-back test: init writes a 22 000-byte file without fsync,
# the kernel's write-back thread must put it on the disk within seconds,
# then QEMU is killed (a power cut). The file is read back from the ext4
# root on the host with debugfs.
param([switch]$SkipBuild)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$failed = 0
function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
    if (-not $Ok) { $script:failed++ }
}
$disk = Join-Path $testDir 'writeback.img'
$log = Join-Path $testDir 'writeback.serial.log'
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline 'init.args=tests,dirty' -OutputImage $disk | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "write-back test image build failed ($LASTEXITCODE)" }
}
if (-not (Test-Path $disk)) { throw "write-back test image not found: $disk" }
Remove-Item $log -ErrorAction SilentlyContinue
function Text { if (-not (Test-Path $log)) { return '' }; $fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite'); try { (New-Object IO.StreamReader($fs)).ReadToEnd() } finally { $fs.Close() } }
function Wait([string]$Pattern, [int]$Seconds) {
    $end = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $end) { $m = [regex]::Match((Text), $Pattern); if ($m.Success) { return $m }; Start-Sleep -Milliseconds 300 }
    return $null
}
Push-Location $Root
try { $vm = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -NoNet -DiskFile ($disk.Substring($Root.Length + 1))) } finally { Pop-Location }
try {
    $w = Wait 'WRITEBACK_WRITTEN (\d+) bytes' 90
    Record 'file written without fsync' ($null -ne $w) $(if ($w) { $w.Value } else { '' })
    # An earlier writeback (e.g. during boot) cannot prove this file was flushed.
    $b = Wait 'WRITEBACK_WRITTEN \d+ bytes[\s\S]*?writeback: / \(\S+\) written' 20
    Record 'write-back thread wrote the root within seconds' ($null -ne $b) $(if ($b) { $b.Value } else { '' })
} finally {
    # Power cut: no unmount, no sync.
    if (-not $vm.HasExited) { $vm.Kill() }
    $vm.WaitForExit(10000) | Out-Null
}
$ErrorActionPreference = 'Continue'
$offset = (1 + $Script:EspSizeMiB) * 1MB
$img = (ConvertTo-WslPath $disk) + "?offset=$offset"
$content = (wsl.exe -d Ubuntu-24.04 -- debugfs -R 'cat /home/user/writeback.txt' $img 2>$null) -join "`n"
$lines = @($content -split "`n" | Where-Object { $_ -eq 'written without fsync' }).Count
Record 'after the power cut the file is on the disk' ($lines -eq 1000) "$lines of 1000 lines"
Remove-Item $disk -ErrorAction SilentlyContinue
Write-Host "write-back test: $failed failed"
exit $failed
