# Builds the PE32+ test fixtures of compatibility/pe-parser from source with
# the MSYS2 clang + lld toolchain (no C runtime). The resulting binaries are
# committed so the tests do not need a C toolchain; rerun only when the
# sources change. The fixtures are never executed.
. (Join-Path $PSScriptRoot 'common.ps1')
$clang = Find-Tool 'clang' @('C:\msys64\ucrt64\bin\clang.exe')
if (-not $clang) { throw 'clang not found (MSYS2 ucrt64 package mingw-w64-ucrt-x86_64-clang)' }
$dir = Join-Path $Root 'compatibility\pe-parser\tests\fixtures'
$src = Join-Path $dir 'src'
$common = @('--target=x86_64-w64-windows-gnu', '-fuse-ld=lld', '-nostdlib', '-O2', '-Wl,--dynamicbase', '-Wl,--high-entropy-va', '-Wl,--nxcompat',
            '-Wl,--no-insert-timestamp', '-Wl,--build-id=none')

Write-Host '==> hello.exe'
Invoke-Checked 'hello.exe' {
    & $clang @common '-Wl,--subsystem,console' '-Wl,-e,start' "$src\hello.c" -lkernel32 -o "$dir\hello.exe"
}
Write-Host '==> sample.dll'
Invoke-Checked 'sample.dll' {
    & $clang @common -shared '-Wl,-e,DllMainCRTStartup' "$src\sample_dll.c" "$src\sample_dll.def" -lkernel32 -o "$dir\sample.dll"
}
Remove-Item "$dir\*.a", "$dir\*.lib" -ErrorAction SilentlyContinue
Get-ChildItem $dir -File | ForEach-Object { Write-Host ("    {0,-12} {1,7} bytes" -f $_.Name, $_.Length) }
