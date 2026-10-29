# Builds a signed OSyx release: the live USB image and ISO for new
# installations, and the system update installed systems fetch
# (/bin/osyx-update, shared/update): build/release/manifest.txt,
# manifest.sig and payload.bin.
#
#   scripts/release.ps1 -Version 0.2.0
#
# The version must be newer than the one installed systems run. The update
# is signed with the release key (Ed25519). Its secret half lives outside
# the repository (default %USERPROFILE%\.osyx\release-signing.key); its
# public half, shared/update/release.pub, is built into every system.
# Nothing is published: the GitHub CLI command to do so is printed.
param(
    [Parameter(Mandatory)][ValidatePattern('^\d+(\.\d+){1,3}$')][string]$Version,
    [string]$Key = (Join-Path $env:USERPROFILE '.osyx\release-signing.key')
)
. (Join-Path $PSScriptRoot 'common.ps1')
if (-not (Test-Path -LiteralPath $Key)) {
    throw "No release key at $Key (make one with: cargo run -p osyx-update --example release -- keygen $Key shared/update/release.pub)"
}
$versionFile = Join-Path $Root 'VERSION'
$previous = (Get-Content -LiteralPath $versionFile -ErrorAction SilentlyContinue | Select-Object -First 1)
Write-Host "==> OSyx $Version (previous: $previous)"
[IO.File]::WriteAllText($versionFile, "$Version`n")
$out = Join-Path $BuildDir 'release'
if (Test-Path -LiteralPath $out) { Remove-Item -LiteralPath $out -Recurse -Force }
New-Item -ItemType Directory -Force $out | Out-Null
$tar = Join-Path $out 'rootfs.tar'
& (Join-Path $PSScriptRoot 'build.ps1') -Live -ReleaseTar $tar
if ($LASTEXITCODE -ne 0) { throw 'live build failed' }
Push-Location $Root
try {
    Invoke-Checked 'signed update' { cargo run -q -p osyx-update --example release -- build $tar $Version $Key $out }
} finally { Pop-Location }
Remove-Item -LiteralPath $tar
Copy-Item (Join-Path $BuildDir 'osyx-live-usb.img') (Join-Path $out "osyx-$Version-live-usb.img")
Copy-Item (Join-Path $BuildDir 'osyx-live.iso') (Join-Path $out "osyx-$Version-live.iso")
Write-Host "==> release files in $out"
Get-ChildItem $out | ForEach-Object { Write-Host ("    {0,-32} {1,12} bytes" -f $_.Name, $_.Length) }
Write-Host ''
Write-Host 'Publish it yourself (the repository must be public for systems to download it):'
Write-Host "  git tag v$Version; git push origin v$Version"
Write-Host "  gh release create v$Version build/release/* --title `"OSyx $Version`" --notes `"OSyx $Version`""
