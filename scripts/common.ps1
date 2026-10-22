# Shared helpers for the OSyx PowerShell scripts. Dot-source this file.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Script:Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Script:BuildDir = Join-Path $Root 'build'
$Script:EspDir = Join-Path $BuildDir 'esp'

function Find-Tool([string]$Name, [string[]]$Candidates) {
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($c in $Candidates) { if (Test-Path $c) { return $c } }
    return $null
}

$Script:Qemu = Find-Tool 'qemu-system-x86_64' @(
    'C:\msys64\ucrt64\bin\qemu-system-x86_64.exe',
    'C:\Program Files\qemu\qemu-system-x86_64.exe')

function Find-Ovmf {
    $dirs = @('C:\msys64\ucrt64\share\qemu', 'C:\Program Files\qemu\share')
    if ($Script:Qemu) { $dirs = @((Join-Path (Split-Path (Split-Path $Script:Qemu)) 'share\qemu')) + $dirs }
    foreach ($d in $dirs) {
        $code = Join-Path $d 'edk2-x86_64-code.fd'
        $vars = Join-Path $d 'edk2-i386-vars.fd'
        if ((Test-Path $code) -and (Test-Path $vars)) { return @{ Code = $code; Vars = $vars } }
    }
    return $null
}
$Script:Ovmf = Find-Ovmf

# Runs a native command block and fails on a non-zero exit code. Native stderr
# (cargo prints progress there) must not count as an error, so the
# preference is relaxed while the block runs.
function Invoke-Checked([string]$What, [scriptblock]$Block) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Block 2>&1 | ForEach-Object { "$_" } } finally { $ErrorActionPreference = $saved }
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

# Returns the QEMU argument list for booting build/esp.
function Get-QemuArgs([string]$SerialLog, [switch]$Headless, [int]$MemoryMiB = 512, [int]$QmpPort = 0, [switch]$Gdb) {
    if (-not $Script:Qemu) { throw 'qemu-system-x86_64 not found (run scripts/check-env.ps1)' }
    if (-not $Script:Ovmf) { throw 'OVMF (edk2-x86_64-code.fd) not found' }
    $vars = Join-Path $BuildDir 'OVMF_VARS.fd'
    if (-not (Test-Path $vars)) { Copy-Item $Script:Ovmf.Vars $vars }
    $a = @(
        '-machine', 'q35',
        '-cpu', 'qemu64,+nx',
        '-m', "$MemoryMiB",
        '-drive', "if=pflash,format=raw,unit=0,readonly=on,file=$($Script:Ovmf.Code -replace '\\','/')",
        '-drive', "if=pflash,format=raw,unit=1,file=$($vars -replace '\\','/')",
        # vvfat exposes build/esp as a read-only FAT disk; relative path because
        # the drive-letter colon would be parsed as an option separator.
        # virtio-blk because IDE disks cannot be read-only.
        '-drive', 'if=virtio,format=raw,readonly=on,file=fat:build/esp',
        '-net', 'none',
        '-serial', "file:$($SerialLog -replace '\\','/')",
        '-device', 'isa-debug-exit,iobase=0xf4,iosize=0x04',
        '-no-reboot'
    )
    if ($Headless) { $a += @('-display', 'none') }
    if ($QmpPort -gt 0) { $a += @('-qmp', "tcp:127.0.0.1:$QmpPort,server,nowait") }
    if ($Gdb) { $a += @('-s', '-S') }
    return $a
}

function Start-Qemu([string[]]$QemuArgs) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Script:Qemu
    $psi.WorkingDirectory = $Script:Root
    $psi.UseShellExecute = $false
    $psi.Arguments = ($QemuArgs | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -join ' '
    return [System.Diagnostics.Process]::Start($psi)
}

# Sends one QMP command (after capability negotiation) and returns the reply.
function Invoke-Qmp([int]$Port, [string]$Json) {
    $client = New-Object System.Net.Sockets.TcpClient('127.0.0.1', $Port)
    try {
        $stream = $client.GetStream()
        $reader = New-Object System.IO.StreamReader($stream)
        $writer = New-Object System.IO.StreamWriter($stream)
        $writer.AutoFlush = $true
        [void]$reader.ReadLine()  # greeting
        $writer.WriteLine('{"execute":"qmp_capabilities"}')
        [void]$reader.ReadLine()
        $writer.WriteLine($Json)
        while ($true) {
            $line = $reader.ReadLine()
            if ($null -eq $line -or $line -match '"return"|"error"') { return $line }
        }
    } finally { $client.Close() }
}

function Save-Screenshot([int]$Port, [string]$Path) {
    $p = $Path -replace '\\', '/'
    return Invoke-Qmp $Port "{`"execute`":`"screendump`",`"arguments`":{`"filename`":`"$p`",`"format`":`"png`"}}"
}

function Wait-ForLog([string]$Log, [string]$Pattern, [int]$TimeoutSec, [System.Diagnostics.Process]$Proc) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path $Log) -and (Select-String -Path $Log -Pattern $Pattern -Quiet)) { return $true }
        if ($Proc -and $Proc.HasExited) { return $false }
        Start-Sleep -Milliseconds 200
    }
    return $false
}
