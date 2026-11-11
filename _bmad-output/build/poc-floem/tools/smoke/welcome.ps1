<#
.SYNOPSIS
  Start-up document scenarios: Welcome on the first launch, session restored without help,
  release notes once after an update, Help menu, startup.document = docs / none.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\welcome.ps1 -Exe C:\path\brouillon.exe [-Only w1,w2] [-Tag x-]
  Same safety contract as run.ps1 (keys/clicks only while our window is the foreground one; always a
  TEMPORARY --settings-dir; the process is always killed). Captures land in tools\smoke\out\.
#>
param([string]$Exe, [string[]]$Only, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-welcome'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
if ($Only) { $Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ }) }
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Want([string]$id) { (-not $Only) -or ($Only -contains $id) }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Get-LastRun([string]$d) { $f = "$d\settings\last-run.toml"; if (Test-Path $f) { (Get-Content -Raw $f).Trim() -replace "`r?`n", '; ' } else { '(none)' } }
function Get-Journal([string]$d) { @(Get-ChildItem "$d\settings\journal" -File -ErrorAction SilentlyContinue).Count }
# `brouillon --version` -> "Brouillon 0.1.0-alpha.13 (build 12, abc1234, 2026-...)"
function Get-ExeVersion { $o = (& $Exe --version) -join ' '; $m = [regex]::Match($o, 'Brouillon (\S+) \(build (\d+)'); @{ Version = $m.Groups[1].Value; Build = $m.Groups[2].Value } }
function Set-LastRun([string]$d, [string]$version, [string]$build) { Write-Utf8 "$d\settings\last-run.toml" "version = `"$version`"`nbuild = `"$build`"`n" }
function Open-ViaPalette($app, [string]$query) {
    Send-Chord $app 'ctrl+k'
    Start-Sleep -Milliseconds 400
    Send-Text $app $query
    Start-Sleep -Milliseconds 300
    Send-Chord $app 'enter'
    Start-Sleep -Milliseconds 700
}

function Send-Wheel($app, [int]$Notches, [double]$LX = 600, [double]$LY = 400) {
    # Wheel over the page: LOGICAL coordinates from the window's top-left; negative = scroll down.
    Assert-Focus $app
    $r = New-Object RECT
    $hr = [W32]::DwmGetWindowAttribute($app.Hwnd, 9, [ref]$r, 16)
    if ($hr -ne 0) { [void][W32]::GetWindowRect($app.Hwnd, [ref]$r) }
    $scale = [W32]::GetDpiForWindow($app.Hwnd) / 96.0
    [void][W32]::SetCursorPos([int]($r.Left + $LX * $scale), [int]($r.Top + $LY * $scale))
    Start-Sleep -Milliseconds 80
    [W32]::mouse_event(0x0001, 2, 2, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40   # a real move event so the view under the pointer is hovered
    $neg2 = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]-2), 0)
    [W32]::mouse_event(0x0001, $neg2, $neg2, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 80
    $delta = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int](-120 * [math]::Sign($Notches))), 0)
    for ($i = 0; $i -lt [math]::Abs($Notches); $i++) {
        Assert-Focus $app
        [W32]::mouse_event(0x0800, 0, 0, $delta, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 30
    }
    Start-Sleep -Milliseconds 300
}

# ---- w1: very first launch -> Welcome with the Markdown preview, nothing written that could duplicate it
if (Want 'w1') { Invoke-Scenario 'w1' 'first launch: Welcome opens with the preview' {
    $d = New-TestEnv $DataRoot 'w1'
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w1'
    Shot $app 'w1-first-launch'
    Send-Chord $app 'down'; Send-Text $app 'abc xyz'   # typing on the read-only page must do nothing
    Shot $app 'w1-after-typing'
    $r = Stop-AppGraceful $app
    $err = Read-Stderr $app
    "close=$r; last-run=[$(Get-LastRun $d)]; session.toml exists=$(Test-Path "$d\settings\session.toml"); journal files=$(Get-Journal $d); stderr: $(($err -split "`n" | Where-Object { $_ -match '\[startup\] opened' }) -join ' | ')"
} }

# ---- w2: close with a real tab next to Welcome, relaunch -> session restored, no help, no duplicate
if (Want 'w2') { Invoke-Scenario 'w2' 'relaunch: session restored, no help page added' {
    $d = New-TestEnv $DataRoot 'w2'
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w2a'
    Send-Chord $app 'ctrl+n'
    Send-Text $app 'my own notes'
    Shot $app 'w2-before-close'
    Start-Sleep -Milliseconds 800
    $r = Stop-AppGraceful $app
    $sess = Get-SessionText $d
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w2b'
    Shot $app2 'w2-relaunch'
    $err = Read-Stderr $app2
    "close=$r; tabs in session=$(([regex]::Matches($sess, '\[\[tabs\]\]')).Count); session mentions Welcome=$($sess -match 'Welcome'); stderr: $(($err -split "`n" | Where-Object { $_ -match 'restored|\[startup\] opened' }) -join ' | ')"
} }

# ---- w3: simulate an update -> Release notes open once, as an extra tab; the next start does not repeat it
if (Want 'w3') { Invoke-Scenario 'w3' 'update: release notes once' {
    $d = New-TestEnv $DataRoot 'w3'
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'w3a'
    $before = Get-LastRun $d
    $r1 = Stop-AppGraceful $app
    $v = Get-ExeVersion
    Set-LastRun $d $v.Version '1'                       # "the previous run was build 1"
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w3b'
    Shot $app2 'w3-after-update'
    $err2 = Read-Stderr $app2
    $after = Get-LastRun $d
    $r2 = Stop-AppGraceful $app2
    $app3 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w3c'
    Shot $app3 'w3-next-start'
    $err3 = Read-Stderr $app3
    "first run last-run=[$before]; after update start last-run=[$after]; update start: $(($err2 -split "`n" | Where-Object { $_ -match '\[startup\] opened' }) -join ' | '); next start: [$(($err3 -split "`n" | Where-Object { $_ -match '\[startup\] opened' }) -join ' | ')]"
} }

# ---- w4: Help > Documentation and Help > Release notes (palette), then reuse of the open tab
if (Want 'w4') { Invoke-Scenario 'w4' 'Help: Documentation and Release notes reopen at any time' {
    $d = New-TestEnv $DataRoot 'w4'
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'w4'
    Shot $app 'w4-plain-file'
    Open-ViaPalette $app 'Help: Documentation'
    Shot $app 'w4-docs'
    Open-ViaPalette $app 'Help: Release'
    Shot $app 'w4-release-notes'
    Open-ViaPalette $app 'Help: Documentation'
    Shot $app 'w4-docs-again'
    Stop-AppGraceful $app | Out-Null
    $sess = Get-SessionText $d
    "session after closing: tabs=$(([regex]::Matches($sess, '\[\[tabs\]\]')).Count) (only a.txt expected); journal files=$(Get-Journal $d)"
} }

# ---- w5: startup.document = docs
if (Want 'w5') { Invoke-Scenario 'w5' 'startup.document = docs' {
    $d = New-TestEnv $DataRoot 'w5'
    $v = Get-ExeVersion
    Set-LastRun $d $v.Version $v.Build
    Write-Utf8 "$d\settings\settings.toml" "[startup]`ndocument = `"docs`"`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w5'
    Shot $app 'w5-docs'
    $err = Read-Stderr $app
    "stderr: $(($err -split "`n" | Where-Object { $_ -match '\[startup\] opened|\[settings\]' }) -join ' | ')"
} }

# ---- w6: startup.document = none
if (Want 'w6') { Invoke-Scenario 'w6' 'startup.document = none' {
    $d = New-TestEnv $DataRoot 'w6'
    $v = Get-ExeVersion
    Set-LastRun $d $v.Version $v.Build
    Write-Utf8 "$d\settings\settings.toml" "[startup]`ndocument = `"none`"`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w6'
    Shot $app 'w6-none'
    $err = Read-Stderr $app
    "stderr: $(($err -split "`n" | Where-Object { $_ -match '\[startup\]|\[settings\]' }) -join ' | ')"
} }

# ---- w7: a file name that does not exist opens as a new empty file (it used to write a 100,000-line sample there)
if (Want 'w7') { Invoke-Scenario 'w7' 'missing file name: new empty file, nothing written' {
    $d = New-TestEnv $DataRoot 'w7'
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\not-yet.txt") -Tag 'w7'
    Shot $app 'w7-missing-file'
    Kill-App $app
    "file exists on disk after start=$(Test-Path "$d\files\not-yet.txt"); sample dir created=$(Test-Path "$d\files\sample")"
} }

# ---- w8: read the whole Documentation page (wheel scroll, one capture per screen)
if (Want 'w8') { Invoke-Scenario 'w8' 'Documentation: scroll through the page' {
    $d = New-TestEnv $DataRoot 'w8'
    $v = Get-ExeVersion
    Set-LastRun $d $v.Version $v.Build
    Write-Utf8 "$d\settings\settings.toml" "[startup]`ndocument = `"docs`"`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'w8'
    # The synthetic mouse wheel does not scroll the preview, so drag the scroll bar thumb (right edge) instead.
    $y = 168
    for ($i = 1; $i -le 11; $i++) {
        Shot $app ("w8-docs-{0:00}" -f $i) 300
        if ($i -lt 11) { Send-Drag $app 1093 $y 1093 ($y + 50) 6; $y += 50 }
    }
    'captures w8-docs-01..11'
} }

# ---- w10: limits of the Markdown preview on an ordinary .md file (inline styles, table, repeated lines)
if (Want 'w10') { Invoke-Scenario 'w10' 'Markdown preview limits' {
    $d = New-TestEnv $DataRoot 'w10'
    Write-Utf8 "$d\files\t.md" ("# Title`n`nA sentence with **bold words** and `code` and a [link](https://example.org) inside it, then plain text again.`n`nsame line`nsame line`nsame line`n`n| a | b |`n|---|---|`n| 1 | 2 |`n`n1. first`n2. second`n   - nested`n`nsnake_case_name and 2*3*4`n")
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\t.md") -Tag 'w10'
    Send-Chord $app 'ctrl+shift+v'
    Start-Sleep -Milliseconds 700
    Shot $app 'w10-md-limits'
    'see w10-md-limits'
} }

# ---- w9: Help menu with the mouse, then each row
if (Want 'w9') { Invoke-Scenario 'w9' 'Help menu rows' {
    $d = New-TestEnv $DataRoot 'w9'
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'w9'
    Send-Click $app 313 62
    Start-Sleep -Milliseconds 500
    Shot $app 'w9-help-menu'
    Send-Click $app 330 122                      # Documentation
    Start-Sleep -Milliseconds 600
    Shot $app 'w9-docs'
    Send-Click $app 313 62; Start-Sleep -Milliseconds 400
    Send-Click $app 330 146                      # Release Notes
    Start-Sleep -Milliseconds 600
    Shot $app 'w9-notes'
    Send-Click $app 313 62; Start-Sleep -Milliseconds 400
    Send-Click $app 310 98                       # Welcome
    Start-Sleep -Milliseconds 600
    Shot $app 'w9-welcome'
    Send-Click $app 313 62; Start-Sleep -Milliseconds 400
    Send-Click $app 330 122                      # Documentation again: the open tab is reused
    Start-Sleep -Milliseconds 600
    Shot $app 'w9-docs-reused'
    'see w9-*'
} }

$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
