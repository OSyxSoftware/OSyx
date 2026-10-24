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
    [switch]$KeepRoot,
    [ValidateRange(128, 16384)][int]$RootMiB = 128,
    # Experimental vendor payload; does not install a menu entry or default.
    [string]$ZenDirectory = ''
)
. (Join-Path $PSScriptRoot 'common.ps1')
$Script:RootSizeMiB = $RootMiB
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
        Invoke-Checked 'user build' { cargo build -p osyx-apps -p osyx-desktop -p osyx-nettools -p osyx-win32 --target x86_64-unknown-none --target-dir target/user @profileArgs }
    } finally { $env:RUSTFLAGS = $savedFlags }
    # Program -> directory on the root file system. The graphical session
    # (PRISM, the desktop shell and its applications) lives in /system/bin.
    $installDirs = [ordered]@{
        init = 'bin'; worker = 'bin'; probe = 'usr/bin'; axis = 'bin'
        prism = 'system/bin'; desktop = 'system/bin'; terminal = 'system/bin'; files = 'system/bin'; sysinfo = 'system/bin'
        ping = 'bin'; net = 'bin'; netstat = 'bin'; dns = 'bin'; nc = 'bin'
        winrun = 'bin'
        taskmgr = 'system/bin'; settings = 'system/bin'
    }
    $userBins = @($installDirs.Keys)
    # Only these are also boot modules (the fallback root's /bin).
    $moduleBins = @('init', 'worker', 'probe')
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
    # Every file in \EFI\OSYX\BIN becomes a boot module; the kernel installs
    # them as /bin of its fallback root when no root partition is usable.
    $binDir = Join-Path $EspDir 'EFI\OSYX\BIN'
    if (Test-Path $binDir) { Remove-Item $binDir -Recurse -Force }
    $legacyInit = Join-Path $EspDir 'EFI\OSYX\INIT.ELF'
    if (Test-Path $legacyInit) { Remove-Item $legacyInit }
    if (-not $NoInit) {
        New-Item -ItemType Directory -Force $binDir | Out-Null
        foreach ($bin in $moduleBins) {
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
        # Programs for /bin and /usr/bin of the root (none with -NoInit).
        $stage = Join-Path $BuildDir 'rootfs-programs'
        if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
        foreach ($d in 'bin', 'usr\bin', 'system\bin') { New-Item -ItemType Directory -Force (Join-Path $stage $d) | Out-Null }
        # The default desktop wallpaper, converted from PNG to OSXI (cached).
        $wallSrc = Join-Path $Root 'graphics\assets\wallpaper\osyx-default.png'
        $wallOut = Join-Path $BuildDir 'wallpaper-default.osxi'
        if (-not (Test-Path $wallOut) -or (Get-Item $wallSrc).LastWriteTime -gt (Get-Item $wallOut).LastWriteTime) {
            Invoke-Checked 'wallpaper' { python (Join-Path $PSScriptRoot 'png2osxi.py') $wallSrc $wallOut }
        }
        $wallDir = Join-Path $stage 'system\share\wallpapers'
        New-Item -ItemType Directory -Force $wallDir | Out-Null
        Copy-Item $wallOut (Join-Path $wallDir 'default.osxi')
        # Fonts for Windows programs (C:\Windows\Fonts) and the registry's
        # substitutes for the usual Windows font names.
        $fontDir = Join-Path $stage 'Windows\Fonts'
        New-Item -ItemType Directory -Force $fontDir | Out-Null
        Copy-Item (Join-Path $Root 'graphics\assets\fonts\windows\*') $fontDir
        $regDir = Join-Path $stage 'etc\osyx\registry'
        New-Item -ItemType Directory -Force $regDir | Out-Null
        Invoke-Checked 'registry seed' { python (Join-Path $PSScriptRoot 'seed-registry.py') (Join-Path $regDir 'machine.reg') }
        # Windows PE32+ test programs (built with clang when available).
        if ($ZenDirectory) {
            $zenSource = (Resolve-Path -LiteralPath $ZenDirectory).Path
            if (-not (Test-Path -LiteralPath (Join-Path $zenSource 'zen.exe'))) { throw 'ZenDirectory has no zen.exe' }
            if ($RootMiB -lt 1024) { throw 'The complete Zen package requires -RootMiB 1024 or larger' }
            $zenStage = Join-Path $stage 'opt/zen'
            New-Item -ItemType Directory -Force $zenStage | Out-Null
            Get-ChildItem -LiteralPath $zenSource -Force | Copy-Item -Destination $zenStage -Recurse
        }
        & (Join-Path $PSScriptRoot 'build-winapps.ps1') | Out-Null
        $winApps = Join-Path $BuildDir 'winapps'
        if (Test-Path (Join-Path $winApps 'textedit.exe')) {
            $appDir = Join-Path $stage 'opt\apps'
            New-Item -ItemType Directory -Force $appDir | Out-Null
            Copy-Item (Join-Path $winApps '*.exe') $appDir
        }
        & (Join-Path $PSScriptRoot 'build-wintests.ps1') | Out-Null
        $winTests = Join-Path $BuildDir 'wintests'
        if (Test-Path (Join-Path $winTests 'hello.exe')) {
            $winDir = Join-Path $stage 'opt\wintests'
            New-Item -ItemType Directory -Force $winDir | Out-Null
            Copy-Item (Join-Path $winTests '*') $winDir -Include *.exe, *.dll
        }
        if (-not $NoInit) {
            foreach ($bin in $userBins) {
                Copy-Item (Join-Path $Root "target\user\x86_64-unknown-none\$profileDir\$bin") (Join-Path $stage "$($installDirs[$bin])\$bin")
            }
        }
        Write-Host '==> building ext4 root file system (mke2fs in WSL)'
        New-RootImage $stage
    }
    Write-Host '==> assembling GPT disk image'
    New-DiskImage
} finally {
    Pop-Location
}
