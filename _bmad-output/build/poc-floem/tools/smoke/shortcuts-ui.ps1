<#
.SYNOPSIS
  Keyboard Shortcuts dialog (File > Keyboard Shortcuts...): record a shortcut by pressing keys, see a
  conflict warning, replace it, and check the new shortcut works and keymap.toml was written.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\shortcuts-ui.ps1 -Exe C:\path\brouillon.exe [-Tag x-] [-Mode dark|light]
  Same safety contract as run.ps1 (input only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-shortcuts-ui'), [string]$Tag = '', [string]$Mode = 'dark')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Keymap-Text($d) { $f = "$d\settings\keymap.toml"; if (Test-Path $f) { [IO.File]::ReadAllText($f) } else { '' } }

# LOGICAL px from the window's outer top-left (1100x720 logical window).
$FILE_MENU = @(27, 62)

Invoke-Scenario 'shortcuts' 'Shortcuts: record, conflict, replace, use' {
    $d = New-TestEnv $DataRoot 'shortcuts'
    Write-Utf8 "$d\settings\settings.toml" "[theme]`nmode = `"$Mode`"`n"
    Write-Utf8 "$d\files\a.txt" "alpha`nbeta`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'shortcuts'
    Send-Click $app $FILE_MENU[0] $FILE_MENU[1]
    Shot $app 'key-0-file-menu'
    Send-Click $app 89 218
    Start-Sleep -Milliseconds 800
    Shot $app 'key-1-open'
    Send-Text $app 'duplicate line'
    Shot $app 'key-2-search'
    # Row 1 = "Duplicate Line or Selection" (no default chord). Record Ctrl+J; a bare 'x' must be refused, not typed.
    $SET1 = @(792, 207); $SET2 = @(792, 236)
    Send-Click $app $SET1[0] $SET1[1]
    Shot $app 'key-3-recording'
    Send-Text $app 'x'
    Send-Chord $app 'ctrl+j' 500
    Shot $app 'key-4-bound'
    $k = Keymap-Text $d
    if ($k -notmatch 'chord = "ctrl\+j"' -or $k -notmatch 'command = "duplicate_line"') { throw "keymap.toml lacks ctrl+j -> duplicate_line:`n$k" }
    # Close the dialog and use the shortcut in the document, then save with the untouched Ctrl+S.
    Send-Chord $app 'esc' 400
    Send-Chord $app 'ctrl+j' 400
    Shot $app 'key-5-used'
    Send-Chord $app 'ctrl+s' 700
    $doc = [IO.File]::ReadAllText("$d\files\a.txt")
    if ($doc -ne "alpha`nalpha`nbeta`n") { throw "Ctrl+J did not duplicate the line (file: $($doc -replace "`n",'|'))" }
    # Conflict: give Ctrl+J to "Delete Duplicate Lines" (row 2) while Duplicate Line holds it.
    Send-Click $app 27 62
    Shot $app 'key-5b-menu-again'
    Send-Click $app 89 218
    Start-Sleep -Milliseconds 600
    Shot $app 'key-5c-reopened'
    Send-Text $app 'duplicate line'
    Send-Click $app $SET2[0] $SET2[1]
    Send-Chord $app 'ctrl+j' 500
    Shot $app 'key-6-conflict'
    # The dialog warned (key-6). Nothing is changed until "Replace": the file still maps Ctrl+J to duplicate_line.
    $k = Keymap-Text $d
    if ($k -notmatch 'duplicate_line' -or $k -match 'delete_duplicate_lines') { throw "keymap.toml changed before the user confirmed:`n$k" }
    Send-Click $app 961 155
    Start-Sleep -Milliseconds 500
    Shot $app 'key-7-replaced'
    $k = Keymap-Text $d
    if ($k -notmatch 'command = "delete_duplicate_lines"' -or $k -match 'command = "duplicate_line"') { throw "Replace did not move Ctrl+J:`n$k" }
    Send-Chord $app 'esc' 400
    # The document holds alpha / alpha / beta: Ctrl+J now DELETES the duplicate line instead of duplicating it.
    Send-Chord $app 'ctrl+j' 400
    Send-Chord $app 'ctrl+s' 700
    $doc = [IO.File]::ReadAllText("$d\files\a.txt")
    if ($doc -ne "alpha`nbeta`n") { throw "Ctrl+J did not delete the duplicate line after Replace (file: $($doc -replace "`n",'|'))" }
    Shot $app 'key-8-after-replace'
    'Ctrl+J recorded, conflict warned, replaced; keymap.toml and the document read back from disk'
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
