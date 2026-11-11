<#
.SYNOPSIS
  Cliptext: Ctrl+Alt+N inserts an entry of the default library, and the panel (Tools > Cliptext Library)
  lists the entries with a preview and inserts one by click; the text is checked in the saved file.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\cliptext-ui.ps1 -Exe C:\path\brouillon.exe [-Tag x-] [-Mode dark|light]
  Same safety contract as run.ps1 (input only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-cliptext-ui'), [string]$Tag = '', [string]$Mode = 'dark')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }

# LOGICAL px from the window's outer top-left (1100x720 logical window).
$TOOLS_MENU = @(257, 62)

Invoke-Scenario 'cliptext' 'Cliptext: chord, panel, click' {
    $d = New-TestEnv $DataRoot 'cliptext'
    Write-Utf8 "$d\settings\settings.toml" "[theme]`nmode = `"$Mode`"`n"
    Write-Utf8 "$d\files\b.txt" ""
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\b.txt") -Tag 'cliptext'
    # Ctrl+Alt+3 = third entry of the default library = the ISO date.
    Send-Chord $app 'ctrl+alt+3' 500
    Shot $app 'clip-1-chord'
    Send-Click $app $TOOLS_MENU[0] $TOOLS_MENU[1]
    Shot $app 'clip-2-tools-menu'
    Send-Click $app 283 146
    Start-Sleep -Milliseconds 700
    Shot $app 'clip-3-panel'
    # New line, then click the "Lorem ipsum" entry (7th row): it is inserted at the caret.
    Send-Chord $app 'enter' 200
    Send-Click $app 886 426
    Shot $app 'clip-4-clicked'
    Send-Chord $app 'ctrl+s' 700
    $doc = [IO.File]::ReadAllText("$d\files\b.txt")
    if ($doc -notmatch '^\d{4}-\d\d-\d\d\r?\nLorem ipsum dolor sit amet, consectetur adipiscing elit') { throw "saved file lacks the date and the lorem entry: $($doc -replace "`n",'|')" }
    # Add the whole text as a new entry (Ctrl+A, "Add selection"): the library file on disk gets it.
    Send-Chord $app 'ctrl+a' 200
    Send-Click $app 863 669
    Shot $app 'clip-5-added'
    $lib = [IO.File]::ReadAllText("$d\settings\cliptext\default.toml")
    if ($lib -notmatch 'title = "\d{4}-\d\d-\d\d"') { throw "default.toml has no entry made from the selection:`n$lib" }
    'Ctrl+Alt+3 and a panel click inserted text (saved file checked); Add selection wrote the library file'
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
