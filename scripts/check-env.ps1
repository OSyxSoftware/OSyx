# Reports the tools OSyx needs and whether they are usable. Exit code 1 if a
# required tool is missing. Makes no changes to the system.
. (Join-Path $PSScriptRoot 'common.ps1')

$failed = $false
function Report([string]$Name, [bool]$Ok, [string]$Detail, [bool]$Required = $true) {
    $tag = if ($Ok) { ' ok ' } elseif ($Required) { 'FAIL' } else { 'warn' }
    Write-Host ("[{0}] {1,-22} {2}" -f $tag, $Name, $Detail)
    if (-not $Ok -and $Required) { $script:failed = $true }
}

function Or($a, $b) { if ($a) { $a } else { $b } }

function Version([string]$Exe, [string[]]$VArgs) {
    if (-not $Exe) { return $null }
    try { return ((& $Exe @VArgs 2>&1) | Select-Object -First 1).ToString().Trim() } catch { return $null }
}

$rustc = Find-Tool 'rustc' @()
Report 'rustc' ([bool]$rustc) (Or (Version $rustc @('--version')) 'not found')
$cargo = Find-Tool 'cargo' @()
Report 'cargo' ([bool]$cargo) (Or (Version $cargo @('--version')) 'not found')
$targets = if (Find-Tool 'rustup' @()) { rustup target list --installed } else { @() }
foreach ($t in 'x86_64-unknown-uefi', 'x86_64-unknown-none') {
    Report "target $t" ($targets -contains $t) $(if ($targets -contains $t) { 'installed' } else { "missing: rustup target add $t" })
}
Report 'qemu-system-x86_64' ([bool]$Script:Qemu) (Or (Version $Script:Qemu @('--version')) 'not found: pacman -S mingw-w64-ucrt-x86_64-qemu')
Report 'OVMF firmware' ([bool]$Script:Ovmf) $(if ($Script:Ovmf) { $Script:Ovmf.Code } else { 'edk2-x86_64-code.fd not found (ships with QEMU)' })
$py = Find-Tool 'python' @()
Report 'python (assets)' ([bool]$py) (Or (Version $py @('--version')) 'not found; only needed to regenerate boot assets') $false
$clang = Find-Tool 'clang++' @('C:\msys64\ucrt64\bin\clang++.exe')
Report 'clang++ (C++23)' ([bool]$clang) (Or (Version $clang @('--version')) 'not found; needed for future C++ components') $false
$nasm = Find-Tool 'nasm' @('C:\msys64\ucrt64\bin\nasm.exe')
Report 'nasm' ([bool]$nasm) (Or (Version $nasm @('-v')) 'not found; not used yet (Rust global_asm)') $false
$git = Find-Tool 'git' @()
Report 'git' ([bool]$git) (Or (Version $git @('--version')) 'not found') $false

if ($failed) { Write-Host 'Environment check FAILED'; exit 1 }
Write-Host 'Environment check passed'
