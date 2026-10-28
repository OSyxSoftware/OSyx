# Boots a private disk copy in QEMU + OVMF with a window. Serial output goes to
# build/serial.log (and is echoed here when -Follow is given).
param(
    [string]$Cmdline = 'init.args=desktop',
    [int]$MemoryMiB = 8196,
    [switch]$NoBuild,
    [switch]$Headless,
    [switch]$Gdb,
    [switch]$Follow,
    [switch]$Gpu,
    [int]$QmpPort = 0,
    [switch]$NoZen,
    # Root filesystem size for a fresh session image. The old 1 GiB default
    # left no room for application downloads.
    [ValidateRange(512, 16384)][int]$RootMiB = 8192,
    [ValidateRange(1,16)][int]$CpuCount = 4,
    [ValidateSet('auto','whpx','tcg')][string]$Accel = 'auto'
)
. (Join-Path $PSScriptRoot 'common.ps1')
# Each interactive VM owns its disk for its entire lifetime. Builds/tests
# can replace build/osyx.img without changing this VM's filesystem blocks.
$runDir = Join-Path $BuildDir 'run'
New-Item -ItemType Directory -Force $runDir | Out-Null
$runName = 'osyx-' + [guid]::NewGuid().ToString('N') + '.img'
$runImage = Join-Path $runDir $runName
if (-not $NoBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline $Cmdline -RootMiB $RootMiB -NoZen:$NoZen -OutputImage $runImage
} else {
    $copyLock = Enter-BuildLock
    try { Copy-Item -LiteralPath $DiskImage -Destination $runImage }
    finally { $copyLock.ReleaseMutex(); $copyLock.Dispose() }
}
Write-Host "Session disk (kept after exit): $runImage"
$log = Join-Path $BuildDir 'serial.log'
if (Test-Path $log) { Remove-Item $log }
$qargs = Get-QemuArgs -SerialLog $log -Headless:$Headless -MemoryMiB $MemoryMiB -Gdb:$Gdb -Gpu:$Gpu -QmpPort $QmpPort -DiskFile ("build/run/$runName") -CpuCount $CpuCount -Accel $Accel
if ($Gdb) { Write-Host 'GDB stub on tcp::1234, CPU halted until a debugger continues it' }
Write-Host "qemu $($qargs -join ' ')"
$p = Start-Qemu $qargs
if ($Follow) {
    $pos = 0
    while (-not $p.HasExited) {
        Start-Sleep -Milliseconds 250
        if (Test-Path $log) {
            $stream = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite')
            try { $text = [IO.StreamReader]::new($stream).ReadToEnd() } finally { $stream.Dispose() }
            if ($text.Length -gt $pos) { Write-Host -NoNewline $text.Substring($pos); $pos = $text.Length }
        }
    }
} else {
    $p.WaitForExit()
}
Write-Host "QEMU exited with status $($p.ExitCode)"
