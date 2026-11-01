# Elevation (User Account Control) integration test.
#
# Boots the desktop session (init.args=tests,desktop), opens AXIS Terminal
# and, as the desktop user, asks for administrator rights four times: the
# native `elevate` command and ShellExecuteExW's "runas" verb
# (/opt/wintests/runas.exe), each once accepted (Enter on the prompt's
# default "Yes") and once declined (Escape). Evidence comes from the serial
# log: the desktop's elevation lines, the kernel's process spawn and exit
# lines, and the elevated programs' own output (they run as children of the
# desktop, whose output is the serial console). Screenshots of the prompts
# are kept in build/test/elevation-*.png.
param(
    [switch]$SkipBuild,
    [int]$TimeoutSec = 120
)
. (Join-Path $PSScriptRoot 'common.ps1')
$script:qemu = $null
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$results = New-Object System.Collections.Generic.List[object]
$prefix = 'elevation'
$log = Join-Path $testDir "$prefix.serial.log"

function Record([string]$Name, [bool]$Ok, [string]$Detail) {
    $results.Add([pscustomobject]@{ Test = $Name; Result = $(if ($Ok) { 'PASS' } else { 'FAIL' }); Detail = $Detail })
    Write-Host ("[{0}] {1} {2}" -f $(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail)
}

function Log-Text {
    if (-not (Test-Path $log)) { return '' }
    # QEMU keeps the log open for writing: read it with sharing allowed.
    $fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite')
    try { return ((New-Object IO.StreamReader($fs)).ReadToEnd() -replace "`r", '') } finally { $fs.Close() }
}

# Waits until `$Pattern` matches the log; returns the match (or $null).
function Wait-Log([string]$Pattern, [int]$Seconds = 15) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $m = [regex]::Match((Log-Text), $Pattern)
        if ($m.Success) { return $m }
        if ($script:qemu.HasExited) { return $null }
        Start-Sleep -Milliseconds 200
    }
    return $null
}

function Qmp([string]$Json) { Invoke-Qmp $script:port $Json | Out-Null }

# The shell's confirmation popups as PRISM maps them.
$popupPattern = 'window \d+ "popup" mapped at \((-?\d+), (-?\d+)\) (\d+)x(\d+)'
function Popup-Count { [regex]::Matches((Log-Text), $popupPattern).Count }
function Wait-Popup([int]$Before, [int]$Seconds = 20) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if ([regex]::Matches((Log-Text), $popupPattern).Count -gt $Before) { Start-Sleep -Milliseconds 500; return $true }
        if ($script:qemu.HasExited) { return $false }
        Start-Sleep -Milliseconds 200
    }
    return $false
}

$shifted = @{ '(' = '9'; ')' = '0'; '*' = '8'; '"' = 'apostrophe'; '_' = 'minus'; ':' = 'semicolon'; '>' = 'dot'; '<' = 'comma'; '+' = 'equal'; '?' = 'slash'; '!' = '1'; '$' = '4' }
$plain = @{ ' ' = 'spc'; '-' = 'minus'; '/' = 'slash'; '.' = 'dot'; ',' = 'comma'; '=' = 'equal'; ';' = 'semicolon'; "'" = 'apostrophe'; "`n" = 'ret' }
function Keys([string[]]$Codes) {
    $list = ($Codes | ForEach-Object { "{`"type`":`"qcode`",`"data`":`"$_`"}" }) -join ','
    Qmp "{`"execute`":`"send-key`",`"arguments`":{`"keys`":[$list],`"hold-time`":40}}"
    Start-Sleep -Milliseconds 70
}
function TypeText([string]$Text) {
    foreach ($c in $Text.ToCharArray()) {
        $s = [string]$c
        if ($plain.ContainsKey($s)) { Keys @($plain[$s]) }
        elseif ($shifted.ContainsKey($s)) { Keys @('shift', $shifted[$s]) }
        elseif ($s -cmatch '[A-Z]') { Keys @('shift', $s.ToLower()) }
        else { Keys @($s) }
    }
}
function Shot([string]$Name) { Save-Screenshot $script:port (Join-Path $testDir "$prefix-$Name.png") | Out-Null; Start-Sleep -Milliseconds 300 }

# Exit status of the process the kernel started with arguments matching
# `$ArgsPattern` (after `$After` such starts), or $null.
function Wait-Exit([string]$ArgsPattern, [int]$After = 0, [int]$Seconds = 60) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $t = Log-Text
        $starts = [regex]::Matches($t, "process (\d+) \([^)]*\): \S+ spawned by \d+, entry \S+, args \[$ArgsPattern\]")
        if ($starts.Count -gt $After) {
            $id = $starts[$After].Groups[1].Value
            $e = [regex]::Match($t, "process $id \([^)]*\) exited with status (-?\d+)")
            if ($e.Success) { return [int]$e.Groups[1].Value }
        }
        if ($script:qemu.HasExited) { return $null }
        Start-Sleep -Milliseconds 300
    }
    return $null
}

# Types a command, waits for the consent prompt, screenshots it and answers
# it with `$Key` (ret: Yes, esc: Cancel). Returns whether the prompt came.
function Ask([string]$Command, [string]$ProgramPattern, [string]$Key, [string]$ShotName) {
    $before = Popup-Count
    $requests = [regex]::Matches((Log-Text), "desktop: elevation requested for $ProgramPattern by uid \d+").Count
    TypeText "$Command`n"
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline -and [regex]::Matches((Log-Text), "desktop: elevation requested for $ProgramPattern by uid \d+").Count -le $requests) { Start-Sleep -Milliseconds 200 }
    if (-not (Wait-Popup $before)) { return $false }
    Shot $ShotName
    Keys @($Key)
    return $true
}

Push-Location $Root
try {
    $sessionDisk = Join-Path $testDir ('elevation-session-' + [Guid]::NewGuid().ToString('N') + '.img')
    if (-not $SkipBuild) {
        Write-Host '==> building the desktop image (init.args=tests,desktop)'
        & (Join-Path $PSScriptRoot 'build.ps1') -NoZen -Cmdline 'init.args=tests,desktop' -OutputImage $sessionDisk | Out-Null
    } else {
        $copyLock = Enter-BuildLock
        try { Copy-Item -LiteralPath $DiskImage -Destination $sessionDisk }
        finally { $copyLock.ReleaseMutex(); $copyLock.Dispose() }
    }
    if (Test-Path $log) { Remove-Item $log }
    $script:port = Get-Random -Minimum 20000 -Maximum 40000
    $script:qemu = Start-Qemu (Get-QemuArgs -SerialLog $log -Headless -QmpPort $script:port -MemoryMiB 1024 -DiskFile $sessionDisk)
    $ready = Wait-Log 'desktop: shell ready on a (\d+)x(\d+) screen' $TimeoutSec
    if (-not $ready) { Record 'desktop session starts' $false 'no "desktop: shell ready" in the log'; throw 'session did not start' }
    Record 'the elevation service is up' ((Log-Text) -notmatch 'desktop: no elevation service') ''
    Start-Sleep -Seconds 2

    Keys @('meta_l')
    $null = Wait-Log 'desktop: launcher opened'
    TypeText 'term'
    Keys @('ret')
    $shell = Wait-Log 'terminal: AXIS shell started as process (\d+)' 30
    Record 'AXIS Terminal runs as the desktop user' ($null -ne $shell) ''
    Start-Sleep -Seconds 2

    # Native: the user cannot setuid; the elevated copy can (and then runs
    # its permission checks as uid 1000).
    TypeText "worker as-user 1000`n"
    $plainStatus = Wait-Exit '"worker", "as-user", "1000"'
    Record 'without elevation, setuid is refused (worker exits 100)' ($plainStatus -eq 100) "status $plainStatus"

    $asked = Ask 'elevate worker as-user 1000' '/bin/worker' 'ret' 'native-prompt'
    Record 'elevate: the consent prompt appears' $asked ''
    $elevated = Wait-Log 'desktop: elevated /bin/worker as process (\d+)' 30
    $checks = Wait-Log 'as-user 1000: (\d+) of 12 permission checks passed' 30
    $cliStatus = Wait-Exit '"elevate", "worker", "as-user", "1000"'
    Record 'elevate: after Yes the program runs as root (setuid works)' ($null -ne $elevated -and $null -ne $checks) $(if ($checks) { $checks.Value } else { '' })
    Record 'elevate exits with the program status' ($null -ne $checks -and $cliStatus -eq (12 - [int]$checks.Groups[1].Value)) "status $cliStatus"
    Shot 'native-done'

    $asked = Ask 'elevate worker exit 7' '/bin/worker' 'esc' 'native-decline'
    $declined = Wait-Log 'desktop: elevation of /bin/worker declined' 20
    $declineStatus = Wait-Exit '"elevate", "worker", "exit", "7"'
    Record 'elevate: Cancel declines; exit 1223 (ERROR_CANCELLED), nothing runs' ($asked -and $null -ne $declined -and $declineStatus -eq 1223 -and (Log-Text) -notmatch 'args \["/bin/worker", "exit", "7"\]') "status $declineStatus"

    # Windows: ShellExecuteExW(runas) returns only after the answer.
    $asked = Ask 'winrun /opt/wintests/runas.exe yes' '/opt/wintests/runas\.exe' 'ret' 'runas-prompt'
    $yes = Wait-Exit '"winrun", "/opt/wintests/runas.exe", "yes"' 0 90
    $child = Wait-Log 'desktop: elevated /opt/wintests/runas\.exe as process (\d+)' 5
    Record 'runas: after Yes ShellExecuteExW succeeds and the child is an administrator' ($asked -and $yes -eq 0 -and $null -ne $child) "runas yes exited with $yes"

    $asked = Ask 'winrun /opt/wintests/runas.exe no' '/opt/wintests/runas\.exe' 'esc' 'runas-decline'
    $no = Wait-Exit '"winrun", "/opt/wintests/runas.exe", "no"' 0 90
    Record 'runas: after Cancel ShellExecuteExW fails with ERROR_CANCELLED' ($asked -and $no -eq 0) "runas no exited with $no"
    Shot 'end'

    $t = Log-Text
    Record 'no kernel panic, no CPU exception in PRISM or the shell' ($t -notmatch 'KERNEL PANIC' -and $t -notmatch '\((prism|desktop)\): .*exception') ''
} finally {
    if ($script:qemu -is [System.Diagnostics.Process] -and -not $script:qemu.HasExited) { $script:qemu.Kill(); $script:qemu.WaitForExit() }
    if ($sessionDisk -and (Test-Path -LiteralPath $sessionDisk)) { Remove-Item -LiteralPath $sessionDisk -ErrorAction SilentlyContinue }
    Pop-Location
}
$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object Result -eq 'FAIL').Count
Write-Host ("elevation tests: {0} passed, {1} failed" -f ($results.Count - $failed), $failed)
exit $failed
