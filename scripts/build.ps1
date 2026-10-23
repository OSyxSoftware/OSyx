# Builds BOOTX64.EFI and the kernel, validates the kernel image with the boot
# loader's own ELF rules, assembles the EFI system partition in build/esp and
# packs it into the GPT disk image build/osyx.img.
param(
    [string]$Cmdline = '',
    [switch]$DebugBuild,
    # Leave \EFI\OSYX\BIN out of the ESP (kernel boots without user programs).
    [switch]$NoInit,
    # Keep the root partition of the current build/osyx.img (e.g. to boot
    # again after a simulated power cut) instead of building a fresh one.
    [switch]$KeepRoot
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
    $cmdFile = Join-Path $EspDir 'EFI\OSYX\CMDLINE.TXT'
    if ($Cmdline) {
        [IO.File]::WriteAllText($cmdFile, $Cmdline)
    } elseif (Test-Path $cmdFile) {
        Remove-Item $cmdFile
    }
    Write-Host "==> ESP contents in $EspDir"
    Get-ChildItem $EspDir -Recurse -File | ForEach-Object {
        Write-Host ("    {0,-28} {1,10} bytes" -f $_.FullName.Substring($EspDir.Length + 1), $_.Length)
    }
    if ($KeepRoot) {
        Write-Host '==> keeping the root partition of the current disk image'
        Save-RootPartition
    } else {
        Write-Host '==> building ext4 root file system (mke2fs in WSL)'
        New-RootImage (Join-Path $Root "target\user\x86_64-unknown-none\$profileDir")
    }
    Write-Host '==> assembling GPT disk image'
    New-DiskImage
} finally {
    Pop-Location
}
