# Graphical session integration test.
#
# Boots OSyx into the desktop (init.args=desktop) in QEMU with an xHCI
# controller, a USB keyboard and a USB mouse and no PS/2 controller, then
# drives the session only through QMP input injection into those USB
# devices. Every check reads evidence from the serial log written by the
# kernel, PRISM, the shell and the applications (not from screenshots, which
# are kept in build/test/desktop-*.png for people to look at).
param(
    [switch]$SkipBuild,
    [int]$TimeoutSec = 120,
    # Run on a VirtIO-GPU as the only display device (no VGA, no usable GOP
    # framebuffer): everything the desktop shows goes through the
    # virtio-gpu driver and PRISM's damage flushes.
    [switch]$Gpu,
    # Regression for rebuilding while an interactive VM is still running.
    [switch]$RebuildWhileRunning
)
. (Join-Path $PSScriptRoot 'common.ps1')
$testDir = Join-Path $BuildDir 'test'
New-Item -ItemType Directory -Force $testDir | Out-Null
$results = New-Object System.Collections.Generic.List[object]
$prefix = if ($Gpu) { 'desktop-gpu' } else { 'desktop' }
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

# ---- USB mouse --------------------------------------------------------------
function MovePtr([int]$Dx, [int]$Dy) {
    # Relative HID reports carry at most 127 per axis: send small steps.
    while ($Dx -ne 0 -or $Dy -ne 0) {
        $sx = [math]::Max(-100, [math]::Min(100, $Dx)); $sy = [math]::Max(-100, [math]::Min(100, $Dy))
        Qmp "{`"execute`":`"input-send-event`",`"arguments`":{`"events`":[{`"type`":`"rel`",`"data`":{`"axis`":`"x`",`"value`":$sx}},{`"type`":`"rel`",`"data`":{`"axis`":`"y`",`"value`":$sy}}]}}"
        $Dx -= $sx; $Dy -= $sy
        Start-Sleep -Milliseconds 15
    }
    Start-Sleep -Milliseconds 150
}
$script:px = 0; $script:py = 0
function Home { MovePtr -4000 -4000; MovePtr -1000 -1000; $script:px = 0; $script:py = 0 }
function MoveTo([int]$X, [int]$Y) { MovePtr ($X - $script:px) ($Y - $script:py); $script:px = $X; $script:py = $Y }
function Button([string]$B, [bool]$Down) {
    $d = if ($Down) { 'true' } else { 'false' }
    Qmp "{`"execute`":`"input-send-event`",`"arguments`":{`"events`":[{`"type`":`"btn`",`"data`":{`"down`":$d,`"button`":`"$B`"}}]}}"
}
function Click([int]$X, [int]$Y, [string]$B = 'left') { MoveTo $X $Y; Button $B $true; Start-Sleep -Milliseconds 60; Button $B $false; Start-Sleep -Milliseconds 250 }
function DoubleClick([int]$X, [int]$Y) { MoveTo $X $Y; Button 'left' $true; Button 'left' $false; Button 'left' $true; Button 'left' $false; Start-Sleep -Milliseconds 300 }
function Drag([int]$X1, [int]$Y1, [int]$X2, [int]$Y2) {
    MoveTo $X1 $Y1; Button 'left' $true; Start-Sleep -Milliseconds 100
    MoveTo $X2 $Y2; Start-Sleep -Milliseconds 100; Button 'left' $false; Start-Sleep -Milliseconds 400
}

# ---- USB keyboard -------------------------------------------------------------
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

# Content rectangle of a window from PRISM's "mapped at" report.
function Window-Rect([string]$TitlePattern) {
    $m = Wait-Log "window (\d+) `"$TitlePattern[^`"]*`" mapped at \((-?\d+), (-?\d+)\) (\d+)x(\d+)" 20
    if (-not $m) { return $null }
    return [pscustomobject]@{ Id = [int]$m.Groups[1].Value; X = [int]$m.Groups[2].Value; Y = [int]$m.Groups[3].Value; W = [int]$m.Groups[4].Value; H = [int]$m.Groups[5].Value }
}

Push-Location $Root
try {
    if (-not $SkipBuild) {
        Write-Host '==> building the desktop image (init.args=desktop)'
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'init.args=desktop' | Out-Null
    }
    if (Test-Path $log) { Remove-Item $log }
    $script:port = Get-Random -Minimum 20000 -Maximum 40000
    $diskFile = 'build/osyx.img'
    if ($RebuildWhileRunning) {
        $sessionDisk = Join-Path $BuildDir 'desktop-isolation.img'
        Copy-Item -LiteralPath $DiskImage -Destination $sessionDisk -Force
        $diskFile = 'build/desktop-isolation.img'
    }
    $qargs = if ($Gpu) { Get-QemuArgs -SerialLog $log -Headless -QmpPort $script:port -MemoryMiB 1024 -Gpu -GpuSize 1920x1080 -DiskFile $diskFile } else { Get-QemuArgs -SerialLog $log -Headless -QmpPort $script:port -MemoryMiB 1024 -DiskFile $diskFile }
    $script:qemu = Start-Qemu $qargs
    $ready = Wait-Log 'desktop: shell ready on a (\d+)x(\d+) screen' $TimeoutSec
    if (-not $ready) { Record 'desktop session starts' $false 'no "desktop: shell ready" in the log'; throw 'session did not start' }
    $sw = [int]$ready.Groups[1].Value; $sh = [int]$ready.Groups[2].Value
    Start-Sleep -Seconds 2
    if ($RebuildWhileRunning) {
        $activeVm = $script:qemu
        & (Join-Path $PSScriptRoot 'build.ps1') -Cmdline 'init.args=desktop' | Out-Null
        $script:qemu = $activeVm
        Record 'build replaces base image while the running VM keeps its own disk' (Test-Path -LiteralPath $sessionDisk) ''
    }
    $t = Log-Text

    Record 'xHCI: USB keyboard and mouse enumerated (no PS/2)' ($t -match 'xhci0: running \(MSI-X interrupts\)' -and $t -match '"QEMU USB Keyboard" \(keyboard\) attached' -and $t -match '"QEMU USB Mouse" \(mouse\) attached') ''
    Record 'PRISM in ring 3 from ext4 owns the display' ($t -match 'process 2 \(prism\): /system/bin/prism spawned by 1' -and $t -match 'display: process 2 owns the display' -and $t -match 'prism: compositor running') ([regex]::Match($t, 'prism: display \S+').Value)
    Record 'desktop shell is a separate process and PRISM client' ($t -match '\(desktop\): /system/bin/desktop spawned by 2' -and $t -match 'prism: client 1 connected \(process 3' -and $t -match 'window 1 "" created \(Desktop' -and $t -match 'window 2 "" created \(Panel') "${sw}x$sh"
    Shot 'start'
    if ($Gpu) {
        $g = [regex]::Match($t, 'display: virtio-gpu at \S+: .*')
        Record 'VirtIO-GPU initialised: scanout, 2D resource, backing, EDID' ($g.Success -and $g.Value -match 'resource \d+' -and $t -notmatch 'virtio-gpu.*failed') $g.Value
        Record 'VirtIO-GPU cursor queue: resource, update, move, hide' ($t -match 'display: cursor check ok') ([regex]::Match($t, 'display: cursor check \S+').Value)
        Record 'PRISM presents through VirtIO-GPU (not GOP)' ($t -match 'prism: display 1920x1080 .*backend virtio-gpu' -and $t -match 'display: status screen active on virtio-gpu') ([regex]::Match($t, 'prism: display .*').Value)
    }

    # Pointer: USB mouse motion moves the cursor; a right click on the
    # desktop opens the context menu exactly where the pointer is.
    Home
    Click 600 400 'right'
    $m = Wait-Log 'desktop: Desktop menu opened at \(600, 400\)'
    Record 'mouse motion and buttons reach PRISM and the shell' ($null -ne $m -and (Log-Text) -match 'prism: button 2 pressed at \(600, 400\)') 'right click at (600, 400)'
    Shot 'menu'
    # "Open Files" is the second menu item (6 px padding, 32 px rows).
    Click 640 (400 + 6 + 32 + 16)
    $files = Window-Rect 'Files'
    $listed = Wait-Log 'files: listed /home/user \((\d+) entries\)'
    Record 'context menu launches Files; it lists the real ext4 home' ($null -ne $files -and $null -ne $listed) $(if ($listed) { $listed.Value } else { '' })

    # Keyboard: Super opens the launcher, typed text filters, Enter starts.
    Keys @('meta_l')
    $l = Wait-Log 'desktop: launcher opened'
    TypeText 'term'
    Shot 'launcher'
    Keys @('ret')
    $term = Window-Rect 'AXIS Terminal'
    Record 'launcher (Super key, search, Enter) starts AXIS Terminal' ($null -ne $l -and $null -ne $term -and $null -ne (Wait-Log 'desktop: launched AXIS Terminal')) ''
    $shell = Wait-Log 'terminal: AXIS shell started as process (\d+)'
    Start-Sleep -Seconds 1

    # Typed commands reach the focused terminal and really run.
    TypeText "ls -l /home/user`n"
    TypeText "print(`"Hello from AXIS`", 6 * 7)`n"
    TypeText "worker exit 3`n"
    TypeText "touch /tmp/typed-in-axis`n"
    TypeText "ls /tmp`n"
    $ran = Wait-Log '\(worker\): /bin/worker spawned by (\d+), entry \S+, args \["worker", "exit", "3"\]'
    $axisPid = if ($shell) { $shell.Groups[1].Value } else { '?' }
    $t = Log-Text
    Record 'keyboard input reaches the focused window' ($t -match 'terminal: command "ls -l /home/user"' -and $t -match 'terminal: command "print\(\\"Hello from AXIS\\", 6 \* 7\)"') ''
    Record 'terminal executes real commands (program spawned by AXIS)' ($null -ne $ran -and $ran.Groups[1].Value -eq $axisPid) $(if ($ran) { "worker spawned by AXIS process $axisPid" } else { '' })
    Shot 'terminal'

    # Window management with the mouse on the terminal window.
    if ($term) {
        Drag ($term.X + 80) ($term.Y - 17) ($term.X + 80 + 160) ($term.Y - 17 + 90)
        $mv = Wait-Log ("window {0} `"AXIS Terminal`" moved to \({1}, {2}\)" -f $term.Id, ($term.X + 160), ($term.Y + 90))
        Record 'window moved by dragging its title bar' ($null -ne $mv) $(if ($mv) { $mv.Value } else { '' })
        $x = $term.X + 160; $y = $term.Y + 90
        Drag ($x + $term.W + 2) ($y + $term.H + 2) ($x + $term.W + 2 + 120) ($y + $term.H + 2 + 70)
        $rs = Wait-Log ("window {0} `"AXIS Terminal`" resized to {1}x{2}" -f $term.Id, ($term.W + 120), ($term.H + 70))
        Record 'window resized by dragging its corner' ($null -ne $rs) $(if ($rs) { $rs.Value } else { '' })
        DoubleClick ($x + 80) ($y - 17)
        $mx = Wait-Log ("window {0} `"AXIS Terminal`" maximized" -f $term.Id)
        DoubleClick 400 17
        $um = Wait-Log ("window {0} `"AXIS Terminal`" unmaximized" -f $term.Id)
        Record 'maximize and restore by double-clicking the title' ($null -ne $mx -and $null -ne $um) ''
    }

    # Focus follows clicks: back to Files, then into a real ext4 folder.
    if ($files) {
        # An empty spot of its side bar (the terminal may cover the rest).
        Click ($files.X + 20) ($files.Y + $files.H - 100)
        $fm = Wait-Log ('focus -> window {0} "Files' -f $files.Id)
        Record 'clicking a window focuses and raises it' ($null -ne $fm) ''
        # Rows: 52 px tool bar, 28 px header, 30 px rows; folders first.
        DoubleClick ($files.X + 300) ($files.Y + 52 + 28 + 15)
        $doc = Wait-Log 'files: listed /home/user/Documents \((\d+) entries\)'
        Keys @('ret')
        $open = Wait-Log 'files: opened /home/user/Documents/Welcome.txt \((\d+) bytes, text view\)'
        Shot 'files-preview'
        Keys @('esc')
        Keys @('ctrl', 'n')
        Start-Sleep -Milliseconds 300
        TypeText 'Projekty'
        Keys @('ret')
        $made = Wait-Log 'files: Created folder /home/user/Documents/Projekty'
        Record 'File Manager: navigate, open a file, create a folder on ext4' ($null -ne $doc -and $null -ne $open -and $null -ne $made) $(if ($open) { $open.Value } else { '' })
        Shot 'files'
    }

    # A third application, from the launcher.
    Keys @('meta_l'); Wait-Log 'desktop: launcher opened' | Out-Null
    Start-Sleep -Milliseconds 300
    TypeText 'system'; Keys @('ret')
    $sys = Window-Rect 'System Information'
    $info = Wait-Log 'sysinfo: OSyx .* processes, \d+ mounts'
    Record 'multiple application windows (System Information as the third)' ($null -ne $sys -and $null -ne $info) $(if ($info) { $info.Value } else { '' })
    Start-Sleep -Seconds 2
    Shot 'three-windows'

    # Task bar: clicking the active window's button minimizes it.
    if ($sys) {
        $t = Log-Text
        $tb = Wait-Log 'desktop: task bar: (\w+) window' 1
        # Buttons start at x = 64, equal widths up to 220 px, in creation order.
        $n = 3
        $each = [math]::Min(220, [math]::Max(40, [math]::Floor((($sw - 8 - 40 - 12 - 132 - 12) - 64) / $n)))
        Click (64 + 2 * ($each + 4) + 30) ($sh - 26)
        $mn = Wait-Log ("window {0} `"System Information`" minimized" -f $sys.Id)
        Click (64 + 2 * ($each + 4) + 30) ($sh - 26)
        $rs2 = Wait-Log ("window {0} `"System Information`" restored" -f $sys.Id)
        Record 'task bar switches windows (minimize / restore)' ($null -ne $mn -and $null -ne $rs2) ''
        # Close with the title bar's close button (24 px, 8 px from the right).
        Click ($sys.X + $sys.W - 8 - 12) ($sys.Y - 17)
        $cl = Wait-Log ("window {0} `"System Information`" closed" -f $sys.Id)
        $ex = Wait-Log 'desktop: System Information \(process \d+\) exited'
        Record 'window closed by its close button; the program exits' ($null -ne $cl -and $null -ne $ex) ''
    }

    # A client that dies without closing its window: PRISM cleans up and lives on.
    $fp = Wait-Log 'desktop: launched Files \(/system/bin/files\) as process (\d+)' 1
    if ($fp -and $term) {
        # Activate the terminal with its task bar button (now the second of two).
        Click (64 + 224 + 30) ($sh - 26)
        Wait-Log ('focus -> window {0} "AXIS Terminal"' -f $term.Id) | Out-Null
        TypeText ("kill {0}`n" -f $fp.Groups[1].Value)
        $gone = Wait-Log ('prism: client \d+ \(process {0}\) disconnected; released 1 window' -f $fp.Groups[1].Value)
        Start-Sleep -Seconds 1
        TypeText "print(`"PRISM still here`")`n"
        $alive = Wait-Log 'terminal: command "print\(\\"PRISM still here\\"\)"'
        Record 'a killed client is cleaned up and PRISM keeps running' ($null -ne $gone -and $null -ne $alive) $(if ($gone) { $gone.Value } else { '' })
    }

    # New native tools and the same PE32+ applications also built for Windows.
    foreach ($app in @(
        @{ Search = 'task'; Title = 'Task Manager'; Shot = 'taskmgr' },
        @{ Search = 'settings'; Title = 'Settings'; Shot = 'settings' },
        @{ Search = 'text editor'; Title = 'OSyx Text Editor'; Shot = 'textedit' },
        @{ Search = 'disk utility'; Title = 'OSyx Disk Utility'; Shot = 'diskutil' },
        @{ Search = 'device manager'; Title = 'OSyx Device Manager'; Shot = 'devmgr' },
        @{ Search = 'network center'; Title = 'OSyx Network Center'; Shot = 'netcenter' }
    )) {
        Keys @('meta_l'); Start-Sleep -Milliseconds 300
        TypeText $app.Search; Keys @('ret')
        $window = Window-Rect ('[^"\r\n]*' + [regex]::Escape($app.Title))
        Record ($app.Title + ' launches through desktop') ($null -ne $window) ''
        if ($window) {
            Start-Sleep -Seconds 2
            Shot $app.Shot
            if ($app.Shot -eq 'taskmgr') {
                Click ($window.X + 80) ($window.Y + 120)
                Start-Sleep -Seconds 2
                Shot 'taskmgr-performance'
            }
            if ($app.Shot -eq 'settings') {
                Click ($window.X + 100) ($window.Y + 120)
                Shot 'settings-appearance'
                Click ($window.X + 100) ($window.Y + 280)
                Shot 'settings-network'
            }
            if ($app.Shot -eq 'textedit') {
                TypeText 'OSyx editor save regression'
                Keys @('ctrl', 's'); Start-Sleep -Milliseconds 200
                TypeText 'C:/home/user/Documents/editor-test.txt'; Keys @('ret')
                Start-Sleep -Milliseconds 500
                $saved = Wait-Log 'window \d+ title "C:/home/user/Documents/editor-test.txt - OSyx Text Editor"' 5
                Record 'Text Editor saves a file through Win32 on ext4' ($null -ne $saved) ''
                TypeText 'x'
                Click ($window.X + $window.W - 20) ($window.Y - 17)
                $dialog = Window-Rect 'OSyx Text Editor'
                Keys @('esc')
                Start-Sleep -Milliseconds 300
                $originalClosed = (Log-Text) -match ('window {0} .* closed' -f $window.Id)
                Record 'Text Editor cancel preserves an unsaved document' ($null -ne $dialog -and -not $originalClosed) ''
                Keys @('ctrl', 's')
            }
            Click ($window.X + $window.W - 20) ($window.Y - 17)
            $closed = Wait-Log ('window {0} .* closed' -f $window.Id) 10
            Record ($app.Title + ' closes cleanly') ($null -ne $closed) ''
        }
    }

    # USB hot-unplug of the keyboard while the session runs.
    Qmp '{"execute":"device_del","arguments":{"id":"kbd"}}'
    $un = Wait-Log 'prism: input device \d+ removed \(class 1\)'
    Record 'USB keyboard unplug reaches PRISM as a device removal' ($null -ne $un -and (Log-Text) -match 'usb 1-\d \(QEMU USB Keyboard\): disconnected') ''
    Shot 'final'

    # Measured composition performance (PRISM reports every 30 s).
    $st = Wait-Log 'prism: (\d+) frames, (\d+) Mpx composed, (\d+) commits, (\d+) input events, \d+ clients; frame time avg ([\d.]+) ms' 40
    Record 'PRISM frame statistics measured' ($null -ne $st) $(if ($st) { $st.Value } else { '' })
    if ($Gpu) {
        $fl = [regex]::Match((Log-Text), 'prism: virtio-gpu: (\d+) damage flushes, (\d+) Mpx transferred, avg (\d+) us per flush.*')
        Record 'damage-region flushes measured (VirtIO-GPU)' ($fl.Success -and [int]$fl.Groups[1].Value -gt 0) $fl.Value
    }
    $t = Log-Text
    Record 'no kernel panic, no CPU exception in PRISM or the shell' ($t -notmatch 'KERNEL PANIC' -and $t -notmatch '\((prism|desktop)\): .*exception') ''
} finally {
    if ($script:qemu -is [System.Diagnostics.Process] -and -not $script:qemu.HasExited) { $script:qemu.Kill(); $script:qemu.WaitForExit() }
    Pop-Location
}
$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object Result -eq 'FAIL').Count
Write-Host ("desktop tests: {0} passed, {1} failed" -f ($results.Count - $failed), $failed)
exit $failed
