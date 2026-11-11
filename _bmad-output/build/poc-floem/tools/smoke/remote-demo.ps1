<#
.SYNOPSIS
  Remote explorer against the BUILT-IN FAKE SERVER only (no network, no real host):
  palette 'Remote: Connect to Demo Server' (the sidebar Browser section switches its source to the demo
  site; it is shown by default below Open Files), loading then loaded rows, open a
  remote file (tab chip, status-bar chip), edit + Ctrl+S (Saving then Synced), a forced conflict and
  its dialog (Cancel focused, Esc closes), 'Manage Sites...', the 3-field connect modal, Disconnect.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\remote-demo.ps1 -Exe C:\path\brouillon.exe [-Tag x-]

  Same safety contract as run.ps1: keys/clicks only while our window is the foreground one (lib.ps1
  Assert-Focus), TEMPORARY --settings-dir (sites.toml, journal and backups all live in it),
  BROUILLON_NO_UPDATE=1, the process is always killed, 60 s budget per scenario. Captures: tools\smoke\out\.
  Every host typed here is fake (host `demo`, or fake-host.invalid with BROUILLON_REMOTE_FAKE=1): the
  editor never opens a socket in this script.

  HOW STEPS ARE CHECKED WITHOUT OCR
    BROUILLON_REMOTE_DEBUG=1 makes the editor print one `[remote-dbg] ...` line per state change on stderr
    (`tree strip=[Local:Connected,demo:Connected*] active=... loading=N errors=N entries=[...]` = the browser's
    sources and the active one's rows, `browser source=... root=... back=... forward=...` after each folder
    move, selection, chip text per document, which dialogs are open, status messages, resolve choices).
    The sources are no longer a visible strip (the browser has a source menu), but the strip list is still
    in the log: Local is always first, the active source is starred. The script waits for those lines (Wait-Dbg) and fails with the last
    lines when one does not show up. Pixel checks (Get-RegionHash before/after) back them up where a
    repaint must happen. Env hooks used (all dev-only, inert unless set):
      BROUILLON_REMOTE_FAKE=1                 any host name reaches the fake server
      BROUILLON_REMOTE_FAKE_LATENCY_MS=n      every fake call sleeps n ms (loading / saving become visible)
      BROUILLON_REMOTE_FAKE_SAFE=1            the fake behaves like the SFTP helper: Ctrl+S runs the verified saver
      BROUILLON_REMOTE_FAKE_TRIGGER_DIR=dir   a file dir\conflict-once.txt ("/remote/path`nnew text") changes
                                              that server file once, the next time the saver looks at it

  NOT COVERED: the vault-less password prompt. It is only raised by the SFTP helper (PromptBroker) and the
  fake server never asks, so it is not reachable without a real SSH server. Reported as NOT_TESTED below.
  SOFT checks (printed as WARN, never fail the run): clicks whose coordinates are estimated from the modal
  layout (Password button, Connect button), and the Refresh loading row. Calibrate $P if they warn.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-remote-demo'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$script:Warn = New-Object System.Collections.ArrayList

# LOGICAL px from the window's outer top-left (see SYNOPSIS). TitleBar = window frame above the client area
# (32, same value the other calibrated scripts imply). ModalTop = padding_top of the modal backdrop
# (ui_kit.rs `modal`: 44, measured from the client top, independent of the chrome rows).
# Layout below the title bar since f694fd3 (the toolbar row is gone): header 24 + 1 border, menu bar 28,
# tab bar 28 -> the body (sidebar + editor) starts at TitleBar + 81 = 113. The sidebar (browser_view.rs) holds
# OPEN FILES then BROWSER; the script never clicks in it any more: the browser gets the keyboard through the
# palette command 'Browser: Focus File List'.
$P = @{ TitleBar = 32; ModalTop = 44; BodyTop = 113; RowsTop = 196; RowH = 20; SideX = 100 }

# Our window must be the foreground one before any key/click. Only brings OUR window forward (no keystroke
# is sent to anyone else); a no-op when it already is. Called before every input burst.
function Refocus($app) {
    if ((Get-ForegroundPid) -ne $app.Proc.Id) { Set-AppFocus $app }
}

# Launch like every other script (Launch-Brouillon waits for the window, settles 1.5 s, takes the foreground),
# then wait a little more: with BROUILLON_REMOTE_DEBUG/FAKE set the first frames are busier, and a window
# that appears after we measured can steal the foreground (rd-a died on FOCUS_LOST right after launch).
function Launch-Remote([string]$Dir, [string]$Name) {
    $app = Launch-Brouillon -Exe $Exe -Dir $Dir -Files @("$Dir\files\local.txt") -Tag $Name
    Start-Sleep -Milliseconds 1000
    Refocus $app
    $app
}

function Shot($app, $name, [int]$settle = 500) { Save-Shot $app "$Tag$name" $settle | Out-Null }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Warn([string]$m) { [void]$script:Warn.Add($m); Write-Host "  WARN: $m" }

function Set-RemoteEnv {
    param([string]$Latency, [switch]$Safe, [string]$TriggerDir = '')
    $env:BROUILLON_REMOTE_DEBUG = '1'
    $env:BROUILLON_REMOTE_FAKE = '1'
    $env:BROUILLON_REMOTE_FAKE_LATENCY_MS = $Latency
    if ($Safe) { $env:BROUILLON_REMOTE_FAKE_SAFE = '1' } else { Remove-Item Env:BROUILLON_REMOTE_FAKE_SAFE -ErrorAction SilentlyContinue }
    if ($TriggerDir) { $env:BROUILLON_REMOTE_FAKE_TRIGGER_DIR = $TriggerDir } else { Remove-Item Env:BROUILLON_REMOTE_FAKE_TRIGGER_DIR -ErrorAction SilentlyContinue }
}

# ---------------------------------------------------------------- state dump (stderr [remote-dbg] lines)

function Get-Dbg($app) {
    $t = Read-Stderr $app
    @($t -split "`r?`n" | Where-Object { $_.StartsWith('[remote-dbg] ') })
}

# First line at index >= $After matching $Pattern. Throws a clear ASSERT message with the last lines.
function Wait-Dbg {
    param($App, [string]$Pattern, [int]$After = 0, [int]$TimeoutMs = 8000, [string]$What = '')
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        Test-Deadline
        $l = @(Get-Dbg $App)   # @(): 0 or 1 line unrolls to $null / a string, which has no .Count under strict mode
        for ($i = $After; $i -lt $l.Count; $i++) {
            if ($l[$i] -match $Pattern) { return [pscustomobject]@{ Index = $i; Line = $l[$i] } }
        }
        if ($App.Proc.HasExited) { throw "PROCESS_EXITED while waiting for: $What" }
        if ($sw.ElapsedMilliseconds -gt $TimeoutMs) {
            $tail = (@($l | Select-Object -Last 6)) -join ' || '
            throw "ASSERT FAILED: $What. No [remote-dbg] line matching /$Pattern/ within $TimeoutMs ms. Last lines: $tail"
        }
        Start-Sleep -Milliseconds 50
    }
}

function Dbg-Count($app) { @(Get-Dbg $app).Count }

function Dbg-Last($app, [string]$Pattern) {
    $m = @(Get-Dbg $app | Where-Object { $_ -match $Pattern })
    if ($m.Count -eq 0) { $null } else { $m[$m.Count - 1] }
}

# ---------------------------------------------------------------- geometry + pixels

function Get-WinLogical($app) {
    $r = New-Object RECT
    if ([W32]::DwmGetWindowAttribute($app.Hwnd, 9, [ref]$r, 16) -ne 0) { [void][W32]::GetWindowRect($app.Hwnd, [ref]$r) }
    $s = [W32]::GetDpiForWindow($app.Hwnd) / 96.0
    [pscustomobject]@{ W = [math]::Floor(($r.Right - $r.Left) / $s); H = [math]::Floor(($r.Bottom - $r.Top) / $s) }
}

function Hash-Top($app, $w) { Get-RegionHash $app -Logical @(0, 0, $w.W, 118) }                                  # header / menu / tab bar (ends at 113)
function Hash-Side($app, $w) { Get-RegionHash $app -Logical @(0, 60, 240, [math]::Min(300, $w.H - 120)) }        # left sidebar
function Hash-Center($app, $w) { Get-RegionHash $app -Logical @(($w.W / 2 - 250), 60, 500, 380) }                # where dialogs appear
function Hash-Status($app) { Get-RegionHash $app }                                                              # status bar strip

# ---------------------------------------------------------------- UI steps

# Command palette: Ctrl+K, type the command title, Enter. The effect is proven by the caller from the state dump.
function Run-Palette($app, [string]$Query) {
    Refocus $app
    Send-Chord $app 'ctrl+k' 500
    Send-Text $app $Query
    Start-Sleep -Milliseconds 250
    Send-Chord $app 'enter' 250
}

# The browser pane takes the keyboard through the palette ('Browser: Focus File List': shows the browser if
# hidden, then `kbd-owner=browser` in the state dump). No pixel is clicked. `kbd-owner=` is only logged on
# change, so first read the last one: it may already say browser.
function Focus-Pane($app) {
    $last = Dbg-Last $app 'kbd-owner='
    if ($null -ne $last -and $last -match 'kbd-owner=browser') { return }
    $n = Dbg-Count $app
    Run-Palette $app 'Browser: Focus File List'
    [void](Wait-Dbg $app 'kbd-owner=browser' $n 4000 "palette ran 'Browser: Focus File List' (the browser has the keyboard)")
}

function Select-Row($app, [string]$Path) {
    for ($i = 0; $i -lt 16; $i++) {
        $t = Dbg-Last $app '\] tree '
        if ($null -ne $t -and $t -match ' selected=(\S*)' -and $Matches[1] -eq $Path) { return }
        $n = Dbg-Count $app
        Refocus $app
        Send-Chord $app 'down' 200
        Start-Sleep -Milliseconds 100
    }
    throw "ASSERT FAILED: Down x16 never selected $Path (last tree line: $(Dbg-Last $app '\] tree '))"
}

# Click into the editor (takes the keyboard from the browser rows / filter / path), prove it from the state dump.
function Click-Editor($app, $w) {
    Refocus $app
    Send-Click $app ($w.W * 0.65) ($w.H * 0.45)
    Start-Sleep -Milliseconds 400
    $o = Dbg-Last $app 'kbd-owner='
    if ($null -ne $o -and $o -notmatch 'kbd-owner=editor') { throw "ASSERT FAILED: after a click in the editor the keyboard owner must be the editor. Last: $o" }
}

function Open-DemoSession {
    param($App, $W)
    Run-Palette $App 'Remote: Connect to Demo Server'
    [void](Wait-Dbg $App '\] connect name=demo host=demo' 0 6000 "palette ran 'Remote: Connect to Demo Server'")
}

# ================================================================= A: demo, tree, open, edit, save (plain upload)

Invoke-Scenario 'rd-a' 'Demo server: connect, strip + tree, open, edit, Ctrl+S Saving->Synced, refresh' {
    $d = New-TestEnv $DataRoot 'a'
    Write-Utf8 "$d\files\local.txt" "local file`n"
    Set-RemoteEnv -Latency '700'
    $app = Launch-Remote $d 'rd-a'
    $w = Get-WinLogical $app
    $side0 = Hash-Side $app $w; $top0 = Hash-Top $app $w
    Shot $app 'rd-a-0-start'

    # 1. palette -> Connect to Demo Server; the strip appears, rows load (700 ms per fake call, so 'Loading' is visible)
    Open-DemoSession $app $w
    $c = Wait-Dbg $app 'tree strip=\[[^\]]*demo:Connect(ing|ed)\*[^\]]*\]' 0 6000 'the browser has switched its source to the demo site (starred in the strip list)'
    $l = Wait-Dbg $app 'tree strip=\[[^\]]*demo:Connected\*[^\]]*\].*loading=[1-9]' 0 6000 'a Loading row is shown while the root folder is listed'
    Shot $app 'rd-a-1-loading' 0
    $loaded = Wait-Dbg $app 'tree strip=\[[^\]]*demo:Connected\*[^\]]*\].* loading=0 errors=0 entries=\[[^\]]*/var/www/index\.php' $l.Index 8000 'the loaded rows show /var/www/index.php'
    Shot $app 'rd-a-2-loaded'
    if ((Hash-Side $app $w) -eq $side0) { throw 'ASSERT FAILED: the sidebar looks identical before and after connecting (strip + remote tree not painted)' }

    # 1b. the browser's own navigation lines (soft: Go Up then Alt+Left must log the folder moves)
    Focus-Pane $app
    $n = Dbg-Count $app
    Run-Palette $app 'Browser: Go Up'
    try {
        $up = Wait-Dbg $app 'browser source=\S+ root=/var back=True' $n 4000 'Browser: Go Up logged the parent folder with back available'
        Shot $app 'rd-a-2b-up'
        Focus-Pane $app
        $n = Dbg-Count $app
        Send-Chord $app 'alt+left' 300
        [void](Wait-Dbg $app 'browser source=\S+ root=/var/www back=False forward=True' $n 4000 'Alt+Left went back to /var/www')
        [void](Wait-Dbg $app 'tree strip=\[[^\]]*demo:Connected\*[^\]]*\].* loading=0 errors=0 entries=\[[^\]]*/var/www/index\.php' $n 8000 'the folder is listed again')
    } catch { Warn "rd-a browser navigation: $($_.Exception.Message)" }

    # 2. open /var/www/index.php with the keyboard (Browser: Focus File List gives the pane the keys)
    Focus-Pane $app
    Select-Row $app '/var/www/index.php'
    $n = Dbg-Count $app
    Send-Chord $app 'enter' 200
    [void](Wait-Dbg $app 'file-loaded /var/www/index\.php on demo' $n 8000 'Enter on the file opened it')
    [void](Wait-Dbg $app 'chip /var/www/index\.php \| demo  Synced' $n 3000 'the status chip of the new tab says Synced')
    Shot $app 'rd-a-3-opened'
    if ((Hash-Top $app $w) -eq $top0) { throw 'ASSERT FAILED: the tab bar did not change after opening the remote file (no new tab painted)' }

    # 3. edit: the status bar repaints (modified / caret), then Ctrl+S: Saving... then Synced
    Click-Editor $app $w
    $h0 = Hash-Status $app
    Send-Chord $app 'ctrl+home' 150
    Send-Text $app 'EDIT-'
    Shot $app 'rd-a-4-edited'
    if ((Hash-Status $app) -eq $h0) { throw 'ASSERT FAILED: typing in the remote tab did not change the status bar' }
    $n = Dbg-Count $app
    Send-Chord $app 'ctrl+s' 0
    $sv = Wait-Dbg $app 'chip /var/www/index\.php \| demo  Saving' $n 4000 'Ctrl+S puts the chip in Saving...'
    $hSaving = Hash-Status $app
    Shot $app 'rd-a-5-saving' 0
    $sy = Wait-Dbg $app 'chip /var/www/index\.php \| demo  Synced' $sv.Index 8000 'the chip comes back to Synced after the upload'
    [void](Wait-Dbg $app 'say Saved /var/www/index\.php to demo' $sv.Index 3000 'the status line confirms the save')
    Start-Sleep -Milliseconds 400
    if ((Hash-Status $app) -eq $hSaving) { throw 'ASSERT FAILED: the status bar looks the same in Saving and in Synced (chip not repainted)' }
    Shot $app 'rd-a-6-synced'

    # 4. Remote: Refresh Folder reloads rows (soft check: needs a visible Loading row)
    $n = Dbg-Count $app
    Run-Palette $app 'Remote: Refresh Folder'
    try { [void](Wait-Dbg $app 'tree strip=\[[^\]]*demo:Connected\*[^\]]*\].*loading=[1-9]' $n 4000 'refresh shows Loading rows'); Shot $app 'rd-a-7-refreshing' 0 }
    catch { Warn "rd-a refresh: no Loading row seen after 'Remote: Refresh Folder' (it may refresh silently or not at all for a selected file)" }
    'palette connect, strip, Loading then loaded rows, open (tab+status chip Synced), edit, Ctrl+S Saving->Synced, refresh'
}

# ================================================================= B: forced conflict through the verified saver

Invoke-Scenario 'rd-b' 'Conflict: server file changed behind our back, dialog, Cancel focused, Esc closes' {
    $d = New-TestEnv $DataRoot 'b'
    $trig = Join-Path $d 'trigger'
    New-Item -ItemType Directory -Force $trig | Out-Null
    Write-Utf8 "$d\files\local.txt" "local file`n"
    Set-RemoteEnv -Latency '250' -Safe -TriggerDir $trig
    $app = Launch-Remote $d 'rd-b'
    $w = Get-WinLogical $app
    Open-DemoSession $app $w
    [void](Wait-Dbg $app 'tree strip=\[[^\]]*demo:Connected\*[^\]]*\].* loading=0 errors=0 entries=\[[^\]]*/var/www/index\.php' 0 8000 'demo tree loaded')
    Focus-Pane $app
    Select-Row $app '/var/www/index.php'
    $n = Dbg-Count $app
    Send-Chord $app 'enter' 200
    [void](Wait-Dbg $app 'file-loaded /var/www/index\.php on demo' $n 8000 'remote file opened')
    [void](Wait-Dbg $app 'save-tracking /var/www/index\.php safe=True tracked=True' $n 3000 'the file is on the safe-save path (BROUILLON_REMOTE_FAKE_SAFE reached the app)')
    Click-Editor $app $w

    # 1. a normal verified save first (proves the safe path works before we break it)
    Send-Chord $app 'ctrl+home' 150
    Send-Text $app 'A-'
    $n = Dbg-Count $app
    Send-Chord $app 'ctrl+s' 0
    $sv = Wait-Dbg $app 'save-submit /var/www/index\.php Started' $n 4000 'Ctrl+S reached the verified-save engine (Started)'
    if ($sv.Line -notmatch 'bytes=(\d+)') { throw "ASSERT FAILED: no byte count in $($sv.Line)" }
    $bytes1 = [int]$Matches[1]
    $sv = Wait-Dbg $app 'chip /var/www/index\.php \| demo  Saving' $n 4000 'verified save: Saving...'
    [void](Wait-Dbg $app 'chip /var/www/index\.php \| demo  Synced' $sv.Index 12000 'verified save: Synced')
    Shot $app 'rd-b-1-saved-ok'
    if (@(Get-Dbg $app | Where-Object { $_ -match 'dialogs .*conflict=True' }).Count -gt 0) { throw 'ASSERT FAILED: a conflict dialog opened on a normal save' }

    # 2. change the server file behind our back (one-shot hook), then save again: Conflict + dialog
    Click-Editor $app $w   # the browser pane may hold the keyboard: type into the editor
    Send-Text $app 'B-'
    Write-Utf8 "$trig\conflict-once.txt" "/var/www/index.php`nCHANGED BY SOMEONE ELSE`n"
    $before = Hash-Center $app $w
    $n = Dbg-Count $app
    Send-Chord $app 'ctrl+s' 0
    $sv2 = Wait-Dbg $app 'save-submit /var/www/index\.php Started' $n 4000 'the second Ctrl+S reached the verified-save engine (if this fails the keystroke never became a save)'
    # The typed `B-` went into the document, not into the browser filter: the saved size grew by exactly 2 bytes.
    if ($sv2.Line -notmatch 'bytes=(\d+)' -or [int]$Matches[1] -ne ($bytes1 + 2)) { throw "ASSERT FAILED: typing B- must add 2 bytes to the saved document (first save bytes=$bytes1, now: $($sv2.Line)); the keys went elsewhere" }
    [void](Wait-Dbg $app 'chip /var/www/index\.php \| demo  Conflict' $n 12000 'the chip goes to Conflict')
    $dlg = Wait-Dbg $app 'dialogs .*conflict=True' $n 4000 'the conflict dialog opens'
    if (Test-Path "$trig\conflict-once.txt") { throw 'ASSERT FAILED: the fake server never consumed the trigger file (hook not reached)' }
    [void](Wait-Dbg $app 'say (The file changed on the server|The server has a newer version)' $n 3000 'the status line explains the conflict')
    Shot $app 'rd-b-2-conflict-dialog'
    if ((Hash-Center $app $w) -eq $before) { throw 'ASSERT FAILED: the screen did not change when the conflict dialog opened' }

    # 3. Cancel has the keyboard focus: Enter alone picks it
    $n = Dbg-Count $app
    Send-Chord $app 'enter' 300
    [void](Wait-Dbg $app 'resolve-conflict Cancel' $n 3000 'Enter on the freshly opened dialog = Cancel (the safe button is focused)')
    [void](Wait-Dbg $app 'dialogs .*conflict=False' $n 3000 'the dialog closed after Cancel')
    Shot $app 'rd-b-3-after-cancel'
    $lastChip = Dbg-Last $app 'chip /var/www/index\.php'
    if ($lastChip -notmatch 'Conflict') { throw "ASSERT FAILED: after Cancel the chip must stay Conflict (nothing was overwritten). Last chip line: $lastChip" }

    # 4. save again: the server still differs from what we opened, so the dialog returns; Esc closes it without choosing
    Click-Editor $app $w
    $n = Dbg-Count $app
    Send-Chord $app 'ctrl+s' 0
    [void](Wait-Dbg $app 'dialogs .*conflict=True' $n 12000 'a second save asks again (nothing was silently overwritten)')
    Shot $app 'rd-b-4-conflict-again'
    $resolvesBefore = @(Get-Dbg $app | Where-Object { $_ -match 'resolve-conflict' }).Count
    $n = Dbg-Count $app
    Send-Chord $app 'esc' 300
    [void](Wait-Dbg $app 'dialogs .*conflict=False' $n 3000 'Escape closed the conflict dialog (if this fails: the focused Cancel button swallows Escape, so the window cannot be closed from the keyboard)')
    Start-Sleep -Milliseconds 300
    $resolvesAfter = @(Get-Dbg $app | Where-Object { $_ -match 'resolve-conflict' }).Count
    if ($resolvesAfter -ne $resolvesBefore) { throw 'ASSERT FAILED: Escape triggered a resolve action (it must only close)' }
    Shot $app 'rd-b-5-esc-closed'
    'normal verified save Saving->Synced; forced conflict -> Conflict chip + dialog; Enter = Cancel (chip stays Conflict); second dialog closed by Esc without any resolve'
}

# ================================================================= C: Manage Sites, connect modal (3 fields), Disconnect

Invoke-Scenario 'rd-c' 'Manage Sites, connect modal (address, folder, sign-in), Disconnect' {
    $d = New-TestEnv $DataRoot 'c'
    Write-Utf8 "$d\files\local.txt" "local file`n"
    Set-RemoteEnv -Latency '150'
    $app = Launch-Remote $d 'rd-c'
    $w = Get-WinLogical $app
    $closed = Hash-Center $app $w
    Shot $app 'rd-c-0-start'

    # 1. Manage Sites (empty list), closed with Esc
    $n = Dbg-Count $app
    Run-Palette $app 'Remote: Manage Sites...'
    [void](Wait-Dbg $app 'dialogs connect=False sites=True' $n 4000 "palette ran 'Remote: Manage Sites...'")
    Shot $app 'rd-c-1-sites-empty'
    $sitesEmpty = Hash-Center $app $w
    if ($sitesEmpty -eq $closed) { throw 'ASSERT FAILED: the Sites modal is not painted' }
    $n = Dbg-Count $app
    Send-Chord $app 'esc' 300
    [void](Wait-Dbg $app 'dialogs connect=False sites=False' $n 3000 'Esc closes the Sites modal')

    # 2. connect modal: address, Tab to Folder, Shift+Tab back, Enter (fake address, never a real one)
    $n = Dbg-Count $app
    Run-Palette $app 'Remote: Connect...'
    [void](Wait-Dbg $app 'dialogs connect=True' $n 4000 "palette ran 'Remote: Connect...' (the modal opened)")
    Send-Text $app 'smoke@fake-host.invalid'
    Send-Chord $app 'tab' 150
    Send-Text $app '/var/www'
    Shot $app 'rd-c-2-connect-filled'
    Send-Chord $app 'shift+tab' 150
    $n = Dbg-Count $app
    Send-Chord $app 'enter' 200
    $f = Wait-Dbg $app 'connect-form address="[^"]*" folder="[^"]*"' $n 4000 'Enter in the Address field submitted the form'
    if ($f.Line -notmatch 'address="smoke@fake-host\.invalid" folder="/var/www"') {
        throw "ASSERT FAILED: the three-field form did not carry address + folder (Tab from Address must reach Folder, Shift+Tab must return). Got: $($f.Line)"
    }
    [void](Wait-Dbg $app 'connect name=\S+ host=fake-host\.invalid root=/var/www' $f.Index 4000 'the profile reached the explorer with the typed host and folder')
    [void](Wait-Dbg $app 'tree strip=\[[^\]]*:Connected\*[^\]]*\].* loading=0 errors=0' $f.Index 8000 'the fake site connected and listed its folder')
    Shot $app 'rd-c-3-connected'
    $sitesToml = Join-Path $d 'settings\sites.toml'
    if (-not (Test-Path $sitesToml) -or (Get-Content -Raw $sitesToml) -notmatch 'fake-host\.invalid') { throw 'ASSERT FAILED: "Remember this site" did not write the profile to the temporary settings dir (sites.toml)' }
    $connectedSide = Hash-Side $app $w

    # 3. sign-in choice (soft: estimated click coordinates). Opens the form again, clicks Password then Connect;
    #    submit_form logs auth=2 even when the address is empty, so no connection is attempted.
    $n = Dbg-Count $app
    Run-Palette $app 'Remote: Connect...'
    [void](Wait-Dbg $app 'dialogs connect=True' $n 4000 'connect modal opened a second time')
    Refocus $app
    $cx = $w.W / 2; $cardTop = $P.TitleBar + $P.ModalTop
    try {
        Send-Click $app ($cx + 12) ($cardTop + 120)          # " Password " button
        Shot $app 'rd-c-4-password-selected'
        $n = Dbg-Count $app
        Send-Click $app ($cx + 168) ($cardTop + 253)         # " Connect " button
        $a = Wait-Dbg $app 'connect-form address="" .* auth=2' $n 3000 'Password sign-in choice reached the form'
    } catch {
        Warn "rd-c sign-in: Password/Connect clicks did not register (estimated coordinates; calibrate `$P TitleBar/ModalTop). $($_.Exception.Message)"
    }
    Send-Chord $app 'esc' 300

    # 4. Manage Sites now lists the remembered site
    $n = Dbg-Count $app
    Run-Palette $app 'Remote: Manage Sites...'
    [void](Wait-Dbg $app 'dialogs connect=False sites=True' $n 4000 'Manage Sites opened again')
    Shot $app 'rd-c-5-sites-with-site'
    if ((Hash-Center $app $w) -eq $sitesEmpty) { throw 'ASSERT FAILED: the Sites modal looks the same with and without a saved site' }
    Send-Chord $app 'esc' 300

    # 5. Disconnect: the strip goes away, the local tree is back
    $n = Dbg-Count $app
    Run-Palette $app 'Remote: Disconnect'
    [void](Wait-Dbg $app 'close-site ' $n 4000 "palette ran 'Remote: Disconnect'")
    [void](Wait-Dbg $app 'tree strip=\[(?![^\]]*demo)[^\]]*\] ' $n 4000 'the demo site is gone from the browser sources (Local only)')
    Shot $app 'rd-c-6-disconnected'
    if ((Hash-Side $app $w) -eq $connectedSide) { throw 'ASSERT FAILED: the sidebar did not change after Disconnect' }
    'Sites modal (empty then with the remembered site, Esc closes), connect form carried address + folder via Tab/Shift+Tab, sites.toml written in the temp dir, Disconnect'
}

Write-Host '[rd-pw] NOT_TESTED    vault-less password prompt: only the SFTP helper raises it (PromptBroker); the fake server never asks, so it is unreachable without a real SSH server'
if ($script:Warn.Count -gt 0) { Write-Host ("`n{0} warning(s):" -f $script:Warn.Count); $script:Warn | ForEach-Object { Write-Host "  - $_" } }
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
foreach ($v in 'BROUILLON_REMOTE_DEBUG', 'BROUILLON_REMOTE_FAKE', 'BROUILLON_REMOTE_FAKE_LATENCY_MS', 'BROUILLON_REMOTE_FAKE_SAFE', 'BROUILLON_REMOTE_FAKE_TRIGGER_DIR') { Remove-Item "Env:$v" -ErrorAction SilentlyContinue }
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
if (@($script:Results | Where-Object { $_.Status -ne 'OK' }).Count -gt 0) { exit 1 }
