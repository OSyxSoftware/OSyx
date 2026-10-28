# ACPI power test: boots OSyx with `init.args=tests,poweroff` and then
# `tests,reboot` and checks that QEMU ends because the guest switched itself
# off (S5 from the FADT and the DSDT's \_S5) or reset itself (FADT reset
# register; QEMU runs with -no-reboot, so a reset also ends it).
param([switch]$SkipBuild)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$failed = 0
function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
    if (-not $Ok) { $script:failed++ }
}
foreach ($mode in 'poweroff', 'reboot') {
    $disk = Join-Path $testDir "power-$mode.img"
    & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline "init.args=tests,$mode" -OutputImage $disk | Out-Null
    $log = Join-Path $testDir "power-$mode.serial.log"
    Remove-Item $log -ErrorAction SilentlyContinue
    Push-Location $Root
    try { $vm = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -DiskFile ($disk.Substring($Root.Length + 1)) -NoNet) } finally { Pop-Location }
    $exited = $vm.WaitForExit(120000)
    if (-not $exited) { $vm.Kill() }
    $fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite')
    try { $t = (New-Object IO.StreamReader($fs)).ReadToEnd() } finally { $fs.Close() }
    if ($mode -eq 'poweroff') {
        Record 'power off: S5 values come from the DSDT' ($t -match 'entering ACPI S5 \(PM1a_CNT 0x[0-9a-f]+, SLP_TYPa \d+, from the DSDT\)') ([regex]::Match($t, 'entering ACPI S5.*').Value)
        Record 'power off: the machine switched itself off' $exited ''
    } else {
        Record 'reboot: FADT reset register used' ($t -match 'FADT reset register') ([regex]::Match($t, 'FADT reset register.*').Value)
        Record 'reboot: the machine reset itself' $exited ''
    }
    Record "$mode`: volumes unmounted cleanly first" ($t -match 'power: .*unmounting volumes') ''
    Remove-Item $disk -ErrorAction SilentlyContinue
}
Write-Host "power test: $failed failed"
exit $failed
