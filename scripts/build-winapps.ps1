# Builds the OSyx Windows applications (compatibility/win32/apps/src) with
# the MSYS2 clang + lld toolchain and no C runtime into build/winapps. The
# same .exe files run on Windows and on OSyx (under /bin/winrun);
# build.ps1 installs them as /opt/apps. Rebuilds only what changed.
param([switch]$Force)
. (Join-Path $PSScriptRoot 'common.ps1')
$clang = Find-Tool 'clang' @('C:\msys64\ucrt64\bin\clang.exe')
$out = Join-Path $BuildDir 'winapps'
if (-not $clang) {
    Write-Warning 'clang not found (MSYS2 ucrt64): Windows applications not built'
    return
}
$src = Join-Path $Root 'compatibility\win32\apps\src'
New-Item -ItemType Directory -Force $out | Out-Null
$common = @('--target=x86_64-w64-windows-gnu', '-fuse-ld=lld', '-nostdlib', '-ffreestanding', '-O2', '-Wall', '-Werror',
            '-Wl,--dynamicbase', '-Wl,--high-entropy-va', '-Wl,--nxcompat', '-Wl,--no-insert-timestamp', '-Wl,--build-id=none',
            '-Wl,--subsystem,windows', '-Wl,-e,start')
$apps = [ordered]@{
    'textedit.exe' = @{ Src = 'textedit.c'; Libs = @('-luser32', '-lgdi32', '-lkernel32') }
    'diskutil.exe' = @{ Src = 'diskutil.c'; Libs = @('-luser32', '-lgdi32', '-lkernel32') }
    'devmgr.exe'   = @{ Src = 'devmgr.c'; Libs = @('-lsetupapi', '-luser32', '-lgdi32', '-lkernel32') }
    'netcenter.exe' = @{ Src = 'netcenter.c'; Libs = @('-liphlpapi', '-lws2_32', '-luser32', '-lgdi32', '-lkernel32') }
}
foreach ($name in $apps.Keys) {
    $a = $apps[$name]
    $file = Join-Path $src $a.Src
    if (-not (Test-Path $file)) { continue }
    $target = Join-Path $out $name
    $stale = $Force -or -not (Test-Path $target) -or ((Get-Item $file).LastWriteTime -gt (Get-Item $target).LastWriteTime) -or ((Get-Item "$src\app.h").LastWriteTime -gt (Get-Item $target).LastWriteTime)
    if ($stale) {
        Invoke-Checked $name { & $clang @common $file @($a.Libs) -o $target }
    }
}
Get-ChildItem $out -File -Filter *.exe | ForEach-Object { Write-Host ("    {0,-14} {1,7} bytes" -f $_.Name, $_.Length) }
