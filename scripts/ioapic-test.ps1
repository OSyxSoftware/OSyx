# Exercise an ISA override and real wired IRQ delivery through QEMU's I/O APIC.
param([ValidateRange(10, 180)][int]$TimeoutSec = 90, [switch]$SkipBuild)
. (Join-Path $PSScriptRoot 'common.ps1')
$vm = $null
Push-Location $Root
try {
    if (-not $SkipBuild) {
        Invoke-Checked 'kernel build' { cargo build -p osyx-kernel --target x86_64-unknown-none --release } | Out-Null
    }
    $name = 'ioapic-' + [guid]::NewGuid().ToString('N')
    $stage = Join-Path $BuildDir "$name/esp"
    New-Item -ItemType Directory -Force "$stage/EFI/BOOT", "$stage/EFI/OSYX" | Out-Null
    Copy-Item -LiteralPath (Join-Path $EspDir 'EFI/BOOT/BOOTX64.EFI') -Destination "$stage/EFI/BOOT/BOOTX64.EFI"
    Copy-Item -LiteralPath 'target/x86_64-unknown-none/release/osyx-kernel' -Destination "$stage/EFI/OSYX/KERNEL.ELF"
    [IO.File]::WriteAllText("$stage/EFI/OSYX/CMDLINE.TXT", 'test.exit boot.fast init.args=tests test.ioapic')
    $log = Join-Path $BuildDir "$name/serial.log"
    $qargs = Get-QemuArgs -SerialLog $log -Headless -NoDisk -NoNet -NoUsb -MemoryMiB 1024 -CpuCount 2 -Accel tcg
    $qargs += @('-drive', "if=virtio,format=raw,file=fat:rw:build/$name/esp")
    $vm = Start-Qemu $qargs
    if (-not (Wait-ForLog $log 'self-tests: \d+ passed, \d+ failed' $TimeoutSec $vm)) { throw "I/O APIC test did not finish: $log" }
    if (-not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
    $output = [IO.File]::ReadAllText($log)
    foreach ($pattern in @('ioapic: GSI 0\.\.23', 'ioapic: PIT delivered [1-9]\d* wired interrupts', '\[test\] I/O APIC wired interrupt routing \.\.\. ok')) {
        if ($output -notmatch $pattern) { throw "I/O APIC check failed: $pattern; log: $log" }
    }
    Write-Host "I/O APIC wired IRQ routing passed; log: $log"
    Write-Host 'Minimal FAT boot omits worker and cannot pass disk-write checks; this is not the full system suite.'
} finally {
    if ($vm -and -not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
    Pop-Location
}
