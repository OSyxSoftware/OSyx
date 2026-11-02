# Targeted memory-lock tests using a temporary FAT boot, without a real disk.
param(
    [switch]$SkipBuild,
    [ValidateRange(2, 16)][int]$CpuCount = 4,
    [ValidateRange(20, 300)][int]$TimeoutSec = 90,
    # Compare an older kernel using exactly the same userspace workload.
    # The older kernel cannot run the new internal independence test.
    [string]$BaselineKernel
)
. (Join-Path $PSScriptRoot 'common.ps1')
$lock = Enter-BuildLock
$vm = $null
$previousFlags = $env:RUSTFLAGS
Push-Location $Root
try {
    if (-not $SkipBuild) {
        Invoke-Checked 'kernel build' { cargo build -p osyx-kernel --target x86_64-unknown-none --release } | Out-Null
        $env:RUSTFLAGS = '-C relocation-model=static -C code-model=small --cfg polyval_force_soft --cfg poly1305_force_soft --cfg aes_force_soft --cfg chacha20_force_soft --cfg curve25519_dalek_backend="serial"'
        Invoke-Checked 'userspace build' { cargo build -p osyx-apps --bin init --bin worker --target x86_64-unknown-none --target-dir target/user --release } | Out-Null
        $env:RUSTFLAGS = $previousFlags
    }
    $name = 'memory-lock-' + [guid]::NewGuid().ToString('N')
    $stage = Join-Path $BuildDir $name
    New-Item -ItemType Directory -Path $stage | Out-Null
    Copy-Item -LiteralPath (Join-Path $BuildDir 'esp/EFI') -Destination $stage -Recurse
    $kernel = if ($BaselineKernel) { $BaselineKernel } else { 'target/x86_64-unknown-none/release/osyx-kernel' }
    Copy-Item -LiteralPath $kernel -Destination (Join-Path $stage 'EFI/OSYX/KERNEL.ELF') -Force
    foreach ($binary in @('init', 'worker')) {
        Copy-Item -LiteralPath "target/user/x86_64-unknown-none/release/$binary" -Destination (Join-Path $stage "EFI/OSYX/BIN/$binary") -Force
    }
    Set-Content -LiteralPath (Join-Path $stage 'EFI/OSYX/CMDLINE.TXT') -Value 'test.exit boot.fast init.args=tests,memory-locks prof=4' -NoNewline
    $log = Join-Path $stage 'serial.log'
    $qemuArgs = Get-QemuArgs -SerialLog $log -Headless -NoDisk -NoNet -NoUsb -Accel tcg -CpuCount $CpuCount -MemoryMiB 1024
    $qemuArgs += @('-drive', "if=virtio,format=raw,file=fat:rw:build/$name")
    $vm = Start-Qemu $qemuArgs
    if (-not $vm.WaitForExit($TimeoutSec * 1000)) { throw "QEMU timed out; log: $log" }
    $output = [IO.File]::ReadAllText($log)
    # The FAT directory backend rejects the raw boot-sector write used by
    # block_io. Permit only that exact failure, never an arbitrary test error.
    $failures = @([regex]::Matches($output, '\[test\] ([^\r\n]+) ... FAILED: ([^\r\n]+)'))
    $expectedExit = 33
    if ($failures.Count -eq 1 -and $failures[0].Groups[1].Value -eq 'block device I/O' -and
        $failures[0].Groups[2].Value -eq 'write sector 6: Io') { $expectedExit = 35 }
    elseif ($failures.Count -ne 0) { throw "Unexpected kernel test failures; log: $log" }
    if ($vm.ExitCode -ne $expectedExit -or $output -match 'KERNEL PANIC|FRAME LEAK' -or
        (-not $BaselineKernel -and $output -notmatch '\[test\] per-process memory locks ... ok') -or
        $output -notmatch 'memory-locks: processes=2 failed=0' -or
        $output -notmatch 'prof: end') { throw "Memory-lock tests failed; log: $log" }
    $workers = [regex]::Matches($output, 'memory-locks: cpu=(\d+) rounds=(\d+) ms=(\d+)')
    if ($workers.Count -ne 2 * [math]::Min($CpuCount, 4)) { throw "Missing worker results; log: $log" }
    Write-Host "Memory-lock tests passed ($CpuCount CPUs, $($workers.Count) threads); log: $log"
    if ($BaselineKernel) { Write-Host 'Baseline comparison: new internal memory-lock test not required.' }
    foreach ($failure in $failures) { Write-Host "Known temporary-FAT failure: $($failure.Value)" }
    Write-Host 'Other kernel tests can fail on this temporary FAT boot; this is not the full system suite.'
} finally {
    if ($vm -and -not $vm.HasExited) { $vm.Kill(); $vm.WaitForExit() }
    $env:RUSTFLAGS = $previousFlags
    Pop-Location
    $lock.ReleaseMutex()
    $lock.Dispose()
}
