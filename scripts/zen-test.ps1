# A real boot attempt of the unmodified Windows browser. Exit 2 means
# BLOCKED/UNVERIFIED, never success. Functional success needs a later test
# of real PRISM windows, tabs, input, page rendering and verified HTTPS.
# -SkipFetch reuses an already verified build/zen/<version>/core tree, for
# reruns when the release API cannot be reached.
param([string]$Version = '1.23.4b', [switch]$SkipBuild, [switch]$SkipFetch, [int]$TimeoutSec = 90,
      [ValidateRange(1024, 16384)][int]$MemoryMiB = 6144)
. (Join-Path $PSScriptRoot 'common.ps1')
$evidenceDir = Join-Path $BuildDir 'zen'
New-Item -ItemType Directory -Force $evidenceDir | Out-Null
if (-not $SkipBuild) {
    if (-not $SkipFetch) { & (Join-Path $PSScriptRoot 'fetch-zen.ps1') -Version $Version }
    & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'init.args=zen-probe test.exit' -RootMiB 1024 -ZenDirectory (Join-Path $evidenceDir "$Version/core")
}
$log = Join-Path $evidenceDir 'qemu.serial.log'
if (Test-Path -LiteralPath $log) { Remove-Item -LiteralPath $log }
$probeImage = Join-Path $evidenceDir 'qemu-probe.img'
Copy-Item -LiteralPath $DiskImage -Destination $probeImage -Force
$qmp = Get-Random -Minimum 20000 -Maximum 40000
$vm = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -MemoryMiB $MemoryMiB -QmpPort $qmp -DiskFile 'build/zen/qemu-probe.img')
try {
    # A first look while the browser should be up, and one at the end.
    $midSec = [math]::Min(45, [int]($TimeoutSec / 2))
    $finished = $vm.WaitForExit($midSec * 1000)
    if (-not $finished) {
        Save-Screenshot $qmp (Join-Path $evidenceDir 'qemu-mid.png') | Out-Null
        $finished = $vm.WaitForExit(($TimeoutSec - $midSec) * 1000)
    }
    if (-not $finished) { Save-Screenshot $qmp (Join-Path $evidenceDir 'qemu.png') | Out-Null }
} finally {
    if (-not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
}
$text = [IO.File]::ReadAllText($log)
$errorLine = [regex]::Matches($text, '\[winrun:\d+\] winrun: (?:delay load failed:|unhandled Windows exception)[^\r\n]+') | Select-Object -Last 1
$fatalLine = [regex]::Matches($text, 'process \d+(?:: out of memory on first access| terminated:)[^\r\n]+') | Select-Object -Last 1
$result = [ordered]@{
    state = 'blocked'; version = $Version; vm_exit = $vm.ExitCode; memory_mib = $MemoryMiB
    browser_result = ([regex]::Match($text, 'ZEN_PROBE_RESULT [^\r\n]+')).Value
    loader_error = $(if ($errorLine) { $errorLine.Value } else { $null })
    process_fault = $(if ($fatalLine) { $fatalLine.Value } else { $null })
    timed_out = (-not $finished)
    functional_tests = 'not reached; null means untested'
    real_browser_window = $null; page_rendering = $null; tabs = $null
    keyboard_mouse = $null; https_verified = $null
    kernel_panic = ($text -match 'KERNEL PANIC'); evidence = $log
}
if (-not $result.browser_result -and -not $result.loader_error) { $result.state = 'unverified' }
$result | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $evidenceDir 'qemu-result.json')
$result | ConvertTo-Json | Write-Host
if ($text -match 'WIN_EXPORT ') {
    Push-Location $Root
    try {
        Invoke-Checked 'Zen import audit' {
            cargo run -q -p osyx-pe-loader --example dependency-audit -- (Join-Path $evidenceDir "$Version/core") $log (Join-Path $evidenceDir 'dependencies.json')
        } | Write-Host
    } finally { Pop-Location }
}
Write-Host 'ZEN STATUS: BLOCKED/UNVERIFIED. No functional browser success is claimed.'
exit 2
