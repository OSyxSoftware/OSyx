# Targeted NVMe I/O and completion-interrupt test on disposable media.
param([ValidateRange(10, 180)][int]$TimeoutSec = 90, [switch]$SkipBuild)
. (Join-Path $PSScriptRoot 'common.ps1')
$vm = $null
Push-Location $Root
try {
    if (-not $SkipBuild) {
        Invoke-Checked 'kernel build' { cargo build -p osyx-kernel --target x86_64-unknown-none --release } | Out-Null
    }
    $name = 'nvme-irq-' + [guid]::NewGuid().ToString('N')
    $stage = Join-Path $BuildDir "$name/esp"
    New-Item -ItemType Directory -Force "$stage/EFI/BOOT", "$stage/EFI/OSYX" | Out-Null
    Copy-Item -LiteralPath (Join-Path $EspDir 'EFI/BOOT/BOOTX64.EFI') -Destination "$stage/EFI/BOOT/BOOTX64.EFI"
    Copy-Item -LiteralPath 'target/x86_64-unknown-none/release/osyx-kernel' -Destination "$stage/EFI/OSYX/KERNEL.ELF"
    [IO.File]::WriteAllText("$stage/EFI/OSYX/CMDLINE.TXT", 'test.exit boot.fast init.args=tests')
    $diskPath = Join-Path $BuildDir "$name/scratch.img"
    $disk = [IO.File]::Open($diskPath, 'CreateNew', 'ReadWrite')
    try {
        $disk.SetLength(64MB)
        $disk.Position = 510
        $disk.WriteByte(0x55)
        $disk.WriteByte(0xaa)
    } finally { $disk.Dispose() }
    $log = Join-Path $BuildDir "$name/serial.log"
    $qargs = Get-QemuArgs -SerialLog $log -Headless -NoDisk -NoNet -NoUsb -MemoryMiB 1024 -CpuCount 2 -Accel tcg
    $qargs += @('-drive', "if=none,id=nvmedisk,format=raw,file=build/$name/scratch.img", '-device', 'nvme,drive=nvmedisk,serial=OSYXIRQTEST', '-drive', "if=virtio,format=raw,file=fat:rw:build/$name/esp")
    $vm = Start-Qemu $qargs
    if (-not (Wait-ForLog $log 'nvme0: \d+ completion interrupts, \d+ completions after sleeping' $TimeoutSec $vm)) { throw "NVMe test did not finish: $log" }
    if (-not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
    $output = [IO.File]::ReadAllText($log)
    foreach ($pattern in @('nvme0: completion interrupts via MSI-X', '\[test\]   nvme0n1 ', '\[test\] block device I/O \.\.\. ok', 'nvme0: [1-9]\d* completion interrupts, [1-9]\d* completions after sleeping')) {
        if ($output -notmatch $pattern) { throw "NVMe check failed: $pattern; log: $log" }
    }
    Write-Host "NVMe read/write/flush and interrupt sleep/wakeup passed; log: $log"
    Write-Host 'Process tests lack worker in this minimal boot; this is not the full system suite.'
} finally {
    if ($vm -and -not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
    Pop-Location
}
