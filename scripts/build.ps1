# Builds BOOTX64.EFI and the kernel, validates the kernel image with the boot
# loader's own ELF rules and assembles the EFI system partition in build/esp.
param(
    [string]$Cmdline = '',
    [switch]$DebugBuild,
    # Leave \EFI\OSYX\INIT.ELF out of the ESP (kernel boots without a user program).
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
        Invoke-Checked 'user build' { cargo build -p osyx-hello --target x86_64-unknown-none --target-dir target/user @profileArgs }
    } finally { $env:RUSTFLAGS = $savedFlags }
    $init = Join-Path $Root "target\user\x86_64-unknown-none\$profileDir\hello"
    Invoke-Checked 'init validation' { cargo run -q -p osyx-elf --example inspect -- --user $init }

    $efi = Join-Path $Root "target\x86_64-unknown-uefi\$profileDir\osyx-uefi-loader.efi"
    $kernel = Join-Path $Root "target\x86_64-unknown-none\$profileDir\osyx-kernel"
    Write-Host '==> validating kernel image'
    Invoke-Checked 'kernel validation' { cargo run -q -p osyx-elf --example inspect -- $kernel }

    New-Item -ItemType Directory -Force (Join-Path $EspDir 'EFI\BOOT'), (Join-Path $EspDir 'EFI\OSYX') | Out-Null
    Copy-Item $efi (Join-Path $EspDir 'EFI\BOOT\BOOTX64.EFI') -Force
    Copy-Item $kernel (Join-Path $EspDir 'EFI\OSYX\KERNEL.ELF') -Force
    $initDest = Join-Path $EspDir 'EFI\OSYX\INIT.ELF'
    if ($NoInit) {
        if (Test-Path $initDest) { Remove-Item $initDest }
    } else {
        Copy-Item $init $initDest -Force
    }
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
