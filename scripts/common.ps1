# Shared helpers for the OSyx PowerShell scripts. Dot-source this file.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Script:Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Script:BuildDir = Join-Path $Root 'build'
$Script:EspDir = Join-Path $BuildDir 'esp'
$Script:DiskImage = Join-Path $BuildDir 'osyx.img'
$Script:EspImage = Join-Path $BuildDir 'esp.img'
$Script:RootImage = Join-Path $BuildDir 'root.img'
$Script:EspSizeMiB = 64

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

$Script:RootSizeMiB = 128

# Path of a Windows file as WSL sees it.
function ConvertTo-WslPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path) -replace '\\', '/'
    return '/mnt/' + $full.Substring(0, 1).ToLower() + $full.Substring(2)
}

# Builds build/root.img (ext4) with mke2fs inside WSL; `$BinDir` holds the
# user programs copied to /bin.
function New-RootImage([string]$BinDir) {
    $wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
    if (-not $wsl) { throw 'WSL with e2fsprogs is needed to build the ext4 root image' }
    $script = ConvertTo-WslPath (Join-Path $PSScriptRoot 'mkrootfs.sh')
    $bin = ConvertTo-WslPath $BinDir
    $out = ConvertTo-WslPath $Script:RootImage
    Invoke-Checked 'root image' { wsl.exe -d Ubuntu-24.04 -u root -- bash $script $bin $out $Script:RootSizeMiB }
}

# Copies the root partition out of build/osyx.img into build/root.img.
function Save-RootPartition {
    $offset = (1 + $Script:EspSizeMiB) * 1MB
    $len = (Get-Item $Script:RootImage).Length
    $src = [IO.File]::OpenRead($Script:DiskImage)
    $dst = [IO.File]::Create($Script:RootImage)
    try {
        [void]$src.Seek($offset, 'Begin')
        $buf = New-Object byte[] (4MB)
        $left = $len
        while ($left -gt 0) {
            $n = $src.Read($buf, 0, [int][math]::Min($buf.Length, $left))
            if ($n -le 0) { throw 'disk image shorter than expected' }
            $dst.Write($buf, 0, $n)
            $left -= $n
        }
    } finally { $src.Close(); $dst.Close() }
}

# Packs build/esp into a FAT32 image and assembles build/osyx.img: GPT with
# the EFI system partition and, if build/root.img exists, the ext4 root.
function New-DiskImage {
    Invoke-Checked 'ESP image' { cargo run -q -p osyx-fat --example mkfat32 -- $Script:EspImage $Script:EspSizeMiB OSYX-ESP $Script:EspDir }
    $parts = @("esp:$($Script:EspImage)")
    $size = $Script:EspSizeMiB + 2
    if (Test-Path $Script:RootImage) {
        $parts += "root:$($Script:RootImage)"
        $size += [int][math]::Ceiling((Get-Item $Script:RootImage).Length / 1MB)
    }
    Invoke-Checked 'disk image' { cargo run -q -p osyx-part --example mkdisk -- $Script:DiskImage $size @parts }
}

# Returns the QEMU argument list for booting build/osyx.img.
function Get-QemuArgs([string]$SerialLog, [switch]$Headless, [int]$MemoryMiB = 512, [int]$QmpPort = 0, [switch]$Gdb, [switch]$NoRng, [switch]$NoUsb, [switch]$NoNet, [string]$HostFwd = '', [switch]$Gpu, [string]$GpuSize = '') {
    if (-not $Script:Qemu) { throw 'qemu-system-x86_64 not found (run scripts/check-env.ps1)' }
    if (-not $Script:Ovmf) { throw 'OVMF (edk2-x86_64-code.fd) not found' }
    # Fresh firmware variables every boot: stale boot entries for devices
    # that no longer exist would otherwise send OVMF to its shell.
    $vars = Join-Path $BuildDir 'OVMF_VARS.fd'
    Copy-Item $Script:Ovmf.Vars $vars -Force
    $a = @(
        # No PS/2 controller: keyboard and mouse input can only arrive over USB.
        '-machine', 'q35,i8042=off',
        '-cpu', 'qemu64,+nx',
        '-m', "$MemoryMiB",
        '-drive', "if=pflash,format=raw,unit=0,readonly=on,file=$($Script:Ovmf.Code -replace '\\','/')",
        '-drive', "if=pflash,format=raw,unit=1,file=$($vars -replace '\\','/')",
        # Relative path: the drive-letter colon would be parsed as an option separator.
        '-drive', 'if=virtio,format=raw,file=build/osyx.img',
        '-serial', "file:$($SerialLog -replace '\\','/')",
        '-device', 'isa-debug-exit,iobase=0xf4,iosize=0x04',
        '-no-reboot'
    )
    if (-not $NoRng) { $a += @('-device', 'virtio-rng-pci') }
    # A virtio-net NIC on QEMU's user-mode network (DHCP 10.0.2.15, gateway
    # and host at 10.0.2.2, DNS 10.0.2.3); $HostFwd adds hostfwd rules.
    if ($NoNet) {
        $a += @('-net', 'none')
    } else {
        $nd = 'user,id=n0'
        if ($HostFwd) { $nd += ',' + $HostFwd }
        $a += @('-netdev', $nd, '-device', 'virtio-net-pci,netdev=n0,mac=52:54:00:12:34:56')
    }
    # USB input through the native xHCI driver (no PS/2 driver exists, so
    # the keyboard and mouse can only reach OSyx this way).
    if (-not $NoUsb) {
        $a += @('-device', 'qemu-xhci,id=xhci', '-device', 'usb-kbd,bus=xhci.0,id=kbd', '-device', 'usb-mouse,bus=xhci.0,id=mouse')
    }
    # VirtIO-GPU as the only display device (no VGA): the desktop must be
    # presented through the virtio-gpu driver, the GOP framebuffer is gone.
    if ($Gpu) {
        $dev = 'virtio-gpu-pci'
        if ($GpuSize) { $w, $h = $GpuSize -split 'x'; $dev += ",xres=$w,yres=$h" }
        $a += @('-vga', 'none', '-device', $dev)
    }
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
