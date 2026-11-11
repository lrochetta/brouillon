<#
.SYNOPSIS
  Preferences dialog (Ctrl+,): change settings from the UI and read settings.toml back from disk.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\settings-ui.ps1 -Exe C:\path\brouillon.exe [-Tag x-]
  Same safety contract as run.ps1 (input only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-settings-ui'), [string]$Tag = '', [string]$Mode = 'dark')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot $(if ($env:BROUILLON_SMOKE_TI -eq '1') { 'testinput.ps1' } else { 'lib.ps1' }))
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Settings-Text($d) { $f = "$d\settings\settings.toml"; if (Test-Path $f) { [IO.File]::ReadAllText($f) } else { '' } }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }

# LOGICAL px from the window's outer top-left; filled in after the first look at the dialog.
$P = @{
    ThemeLight = @(0, 0); UpdatesCheck = @(0, 0)
}

Invoke-Scenario 'settings' 'Preferences: open, change, persist' {
    $d = New-TestEnv $DataRoot 'settings'
    Write-Utf8 "$d\settings\settings.toml" "# my comment`n[theme]`nmode = `"$Mode`"`n`n[plugin_x]`nkeep = 1`n"
    Write-Utf8 "$d\files\a.txt" "alpha`nbeta`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'settings'
    Shot $app 'set-0-start'
    Send-Chord $app 'ctrl+comma' 600
    Shot $app 'set-1-open'
    # Typing goes to the search field, never to the document.
    Send-Text $app 'render'
    Shot $app 'set-2-search'
    # Search shows the one matching setting; the document is untouched.
    # Clear the field, then switch the theme mode to the OTHER one (click), reading the file back.
    for ($i = 0; $i -lt 6; $i++) { Send-Chord $app 'back' 30 }
    $other = if ($Mode -eq 'dark') { 'Light' } else { 'Dark' }
    $x = if ($Mode -eq 'dark') { 660 } else { 603 }   # "Light" / "Dark" button of the Theme mode row (logical px)
    Send-Click $app $x 187
    Start-Sleep -Milliseconds 700
    Shot $app 'set-3-mode-changed'
    $t = Settings-Text $d
    $want = $other.ToLower()
    if ($t -notmatch "mode = `"$want`"") { throw "settings.toml has no mode = $want :`n$t" }
    if ($t -notmatch '# my comment' -or $t -notmatch '\[plugin_x\]\s+keep = 1') { throw "comment or unknown section lost:`n$t" }
    # Updates: filter, then click the 'Check for updates' toggle off.
    Send-Click $app 600 122   # back into the search field (a click on a button moved the focus away)
    Send-Text $app 'updates'
    Shot $app 'set-4-updates'
    Send-Click $app 600 187
    Start-Sleep -Milliseconds 600
    Shot $app 'set-5-updates-off'
    $t = Settings-Text $d
    if ($t -notmatch 'check = false') { throw "settings.toml has no check = false:`n$t" }
    Send-Chord $app 'esc' 400
    Shot $app 'set-6-closed'
    # The document did not receive any of the typed text.
    $doc = [IO.File]::ReadAllText("$d\files\a.txt")
    if ($doc -ne "alpha`nbeta`n") { throw 'document changed' }
    "mode -> $want and updates.check=false read back from disk; comments kept"
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
