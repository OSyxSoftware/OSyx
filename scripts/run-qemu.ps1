# Boots a private disk copy in QEMU + OVMF with a window. Serial output goes to
# build/serial.log (and is echoed here when -Follow is given).
param(
    [string]$Cmdline = '',
    [int]$MemoryMiB = 512,
    [switch]$NoBuild,
    [switch]$Headless,
    [switch]$Gdb,
    [switch]$Follow,
    [switch]$Gpu,
    [int]$QmpPort = 0
)
. (Join-Path $PSScriptRoot 'common.ps1')
if (-not $NoBuild) { & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline $Cmdline }
# Each interactive VM owns its disk for its entire lifetime. Builds/tests
# can replace build/osyx.img without changing this VM's filesystem blocks.
$runDir = Join-Path $BuildDir 'run'
New-Item -ItemType Directory -Force $runDir | Out-Null
$runName = 'osyx-' + [guid]::NewGuid().ToString('N') + '.img'
$runImage = Join-Path $runDir $runName
Copy-Item -LiteralPath $DiskImage -Destination $runImage
Write-Host "Session disk (kept after exit): $runImage"
$log = Join-Path $BuildDir 'serial.log'
if (Test-Path $log) { Remove-Item $log }
$qargs = Get-QemuArgs -SerialLog $log -Headless:$Headless -MemoryMiB $MemoryMiB -Gdb:$Gdb -Gpu:$Gpu -QmpPort $QmpPort -DiskFile ("build/run/$runName")
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
