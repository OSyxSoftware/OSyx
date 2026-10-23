# Fetch an official, pinned Windows x64 release without executing its installer.
# Vendor files stay under ignored build/zen; no binaries enter the repository.
param([string]$Version = '1.23.4b')
. (Join-Path $PSScriptRoot 'common.ps1')
if ($Version -notmatch '^[0-9][0-9A-Za-z.\-]+$') { throw 'Invalid release tag' }
$base = Join-Path $BuildDir 'zen'
New-Item -ItemType Directory -Force $base | Out-Null
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/zen-browser/desktop/releases/tags/$Version" -Headers @{ 'User-Agent' = 'OSyx-compatibility-research' }
$asset = @($release.assets | Where-Object name -EQ 'zen.installer.exe')
if ($asset.Count -ne 1 -or $asset[0].digest -notmatch '^sha256:([a-fA-F0-9]{64})$') { throw 'Official x64 asset or SHA-256 digest missing' }
$asset = $asset[0]
$sha = $asset.digest.Substring(7).ToLowerInvariant()
$expectedUrl = "https://github.com/zen-browser/desktop/releases/download/$Version/zen.installer.exe"
if ($asset.browser_download_url -ne $expectedUrl) { throw 'Unexpected asset URL' }
$installer = Join-Path $base "zen-$Version-x64.exe"
if (-not (Test-Path -LiteralPath $installer)) {
    & curl.exe --fail --location --retry 2 --output $installer $expectedUrl
    if ($LASTEXITCODE -ne 0) { throw 'Zen download failed' }
}
if ((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant() -ne $sha) { throw 'Zen SHA-256 mismatch' }
$package = Join-Path $base $Version
$extracted = Join-Path $package '.extracted.sha256'
if (-not (Test-Path -LiteralPath $extracted) -or ([IO.File]::ReadAllText($extracted)).Trim() -ne $sha) {
    New-Item -ItemType Directory -Force $package | Out-Null
    $entries = & tar.exe -tf $installer
    if ($LASTEXITCODE -ne 0) { throw 'Cannot list Zen archive (Windows tar with 7z support required)' }
    foreach ($entry in $entries) {
        if ($entry -match '(^[/\\]|(^|[/\\])\.\.([/\\]|$)|:)' ) { throw "Unsafe archive path: $entry" }
    }
    & tar.exe -xf $installer -C $package
    if ($LASTEXITCODE -ne 0) { throw 'Zen extraction failed' }
    [IO.File]::WriteAllText($extracted, $sha)
}
$exe = Join-Path $package 'core/zen.exe'
if (-not (Test-Path -LiteralPath $exe)) { throw 'Archive has no core/zen.exe' }
$manifest = [ordered]@{
    version = $Version; source = $release.html_url; published_at = $release.published_at
    asset = $expectedUrl; sha256 = $sha; size = $asset.size
    zen_sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
    extraction = 'Windows tar; installer never executed'
}
$manifest | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $base "provenance-$Version.json")
Write-Host "Verified official Zen $Version x64: $sha"
Write-Host "Zen directory: $(Join-Path $package 'core')"
