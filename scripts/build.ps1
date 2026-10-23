# Builds BOOTX64.EFI and the kernel, validates the kernel image with the boot
# loader's own ELF rules and assembles the EFI system partition in build/esp.
param(
    [string]$Cmdline = '',
    [switch]$DebugBuild,
    # Leave \EFI\OSYX\BIN out of the ESP (kernel boots without user programs).
    [switch]$NoInit
)
. (Join-Path $PSScriptRoot 'common.ps1')
Push-Location $Root
try {
    $profileArgs = @(if (-not $DebugBuild) { '--release' })
    $profileDir = if ($DebugBuild) { 'debug' } else { 'release' }

    Write-Host '==> building boot loader (x86_64-unknown-uefi)'
    Invoke-Checked 'loader build' { cargo build -p osyx-uefi-loader --target x86_64-unknown-uefi @profileArgs }
    Write-Host '==> building kernel (x86_64-unknown-none)'
    Invoke-Checked 'kernel build' { cargo build -p osyx-kernel --target x86_64-unknown-none @profileArgs }

    # User programs use the small code model at low addresses; a separate
    # target dir keeps their flags from invalidating the kernel build cache.
    Write-Host '==> building user programs (x86_64-unknown-none, user code model)'
    $savedFlags = $env:RUSTFLAGS
    $env:RUSTFLAGS = '-C relocation-model=static -C code-model=small'
    try {
        Invoke-Checked 'user build' { cargo build -p osyx-apps --target x86_64-unknown-none --target-dir target/user @profileArgs }
    } finally { $env:RUSTFLAGS = $savedFlags }
    $userBins = @('init', 'worker')
    foreach ($bin in $userBins) {
        $path = Join-Path $Root "target\user\x86_64-unknown-none\$profileDir\$bin"
        Invoke-Checked "$bin validation" { cargo run -q -p osyx-elf --example inspect -- --user $path }
    }

    $efi = Join-Path $Root "target\x86_64-unknown-uefi\$profileDir\osyx-uefi-loader.efi"
    $kernel = Join-Path $Root "target\x86_64-unknown-none\$profileDir\osyx-kernel"
    Write-Host '==> validating kernel image'
    Invoke-Checked 'kernel validation' { cargo run -q -p osyx-elf --example inspect -- $kernel }

    New-Item -ItemType Directory -Force (Join-Path $EspDir 'EFI\BOOT'), (Join-Path $EspDir 'EFI\OSYX') | Out-Null
    Copy-Item $efi (Join-Path $EspDir 'EFI\BOOT\BOOTX64.EFI') -Force
    Copy-Item $kernel (Join-Path $EspDir 'EFI\OSYX\KERNEL.ELF') -Force
    # Every file in \EFI\OSYX\BIN becomes a boot module that `spawn` can start.
    $binDir = Join-Path $EspDir 'EFI\OSYX\BIN'
    if (Test-Path $binDir) { Remove-Item $binDir -Recurse -Force }
    $legacyInit = Join-Path $EspDir 'EFI\OSYX\INIT.ELF'
    if (Test-Path $legacyInit) { Remove-Item $legacyInit }
    if (-not $NoInit) {
        New-Item -ItemType Directory -Force $binDir | Out-Null
        foreach ($bin in $userBins) {
            Copy-Item (Join-Path $Root "target\user\x86_64-unknown-none\$profileDir\$bin") (Join-Path $binDir $bin) -Force
        }
    }
    # Writable FAT32 data disk (second virtio-blk drive, mounted at / by the
    # kernel), recreated on every build so each boot starts from known files.
    # init checks these contents; keep them in sync with userspace init.rs.
    Write-Host '==> creating FAT32 data disk'
    $seed = Join-Path $BuildDir 'data-seed'
    if (Test-Path $seed) { Remove-Item $seed -Recurse -Force }
    New-Item -ItemType Directory -Force (Join-Path $seed 'Docs') | Out-Null
    [IO.File]::WriteAllText((Join-Path $seed 'hello.txt'), "Hello from the build host!`n")
    [IO.File]::WriteAllText((Join-Path $seed 'Docs\Read Me Long Name.txt'), "Long file names work.`n")
    $bytes = New-Object byte[] 150000
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [byte](($i * 13 + 7) % 256) }
    [IO.File]::WriteAllBytes((Join-Path $seed 'seed.bin'), $bytes)
    Invoke-Checked 'data disk' { cargo run -q -p osyx-fat --example mkfat32 -- $DataImage 64 OSYXDATA $seed }

    $cmdFile = Join-Path $EspDir 'EFI\OSYX\CMDLINE.TXT'
    if ($Cmdline) {
        [IO.File]::WriteAllText($cmdFile, $Cmdline)
    } elseif (Test-Path $cmdFile) {
        Remove-Item $cmdFile
    }
    Write-Host "==> ESP ready in $EspDir"
    Get-ChildItem $EspDir -Recurse -File | ForEach-Object {
        Write-Host ("    {0,-28} {1,10} bytes" -f $_.FullName.Substring($EspDir.Length + 1), $_.Length)
    }
} finally {
    Pop-Location
}
