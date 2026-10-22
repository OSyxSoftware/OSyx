# Boots build/esp in QEMU + OVMF with a window. Serial output goes to
# build/serial.log (and is echoed here when -Follow is given).
param(
    [string]$Cmdline = '',
    [int]$MemoryMiB = 512,
    [switch]$NoBuild,
    [switch]$Headless,
    [switch]$Gdb,
    [switch]$Follow
)
. (Join-Path $PSScriptRoot 'common.ps1')
if (-not $NoBuild) { & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline $Cmdline }
$log = Join-Path $BuildDir 'serial.log'
if (Test-Path $log) { Remove-Item $log }
$qargs = Get-QemuArgs -SerialLog $log -Headless:$Headless -MemoryMiB $MemoryMiB -Gdb:$Gdb
if ($Gdb) { Write-Host 'GDB stub on tcp::1234, CPU halted until a debugger continues it' }
Write-Host "qemu $($qargs -join ' ')"
$p = Start-Qemu $qargs
if ($Follow) {
    $pos = 0
    while (-not $p.HasExited) {
        Start-Sleep -Milliseconds 250
        if (Test-Path $log) {
            $text = [IO.File]::ReadAllText($log)
            if ($text.Length -gt $pos) { Write-Host -NoNewline $text.Substring($pos); $pos = $text.Length }
        }
    }
} else {
    $p.WaitForExit()
}
Write-Host "QEMU exited with status $($p.ExitCode)"
