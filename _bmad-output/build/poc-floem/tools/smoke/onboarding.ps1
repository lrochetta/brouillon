<#
.SYNOPSIS
  First-run screens, EditPlus import dialog, About page, Try an agent, and the Tab key preference.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\onboarding.ps1 -Exe C:\path\brouillon.exe [-Mode dark|light] [-Only f1,i1,a1,g1,t1] [-Tag x-]
  Same safety contract as run.ps1 (input only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). The EditPlus folder is a
  SYNTHETIC one under the temp dir, found through a temporary APPDATA: no real user data is read.
  Captures land in tools\smoke\out\.
#>
param([string]$Exe, [string[]]$Only, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-onboarding'), [string]$Tag = '', [string]$Mode = 'dark')
$ErrorActionPreference = 'Stop'
if ($Only) { $Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ }) }
$env:BROUILLON_KEEP_FIRSTRUN = '1'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Want([string]$id) { (-not $Only) -or ($Only -contains $id) }
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Settings-Text($d) { $f = "$d\settings\settings.toml"; if (Test-Path $f) { [IO.File]::ReadAllText($f) } else { '' } }
function Open-ViaPalette($app, [string]$query) {
    Send-Chord $app 'ctrl+k'
    Start-Sleep -Milliseconds 400
    Send-Text $app $query
    Start-Sleep -Milliseconds 300
    Send-Chord $app 'enter'
    Start-Sleep -Milliseconds 900
}
function New-FakeEditPlus([string]$d) {
    # <d>\appdata\EditPlus : a synthetic per-user folder (UTF-16 INI with the real layout, never real data)
    $ep = "$d\appdata\EditPlus"
    New-Item -ItemType Directory -Force $ep | Out-Null
    $ini = "[Options]`r`nPlacement=$([char]1)$([char]2)`r`nTabSize=2`r`nUse spaces for tab=1`r`nIndent size=2`r`nAuto indent=1`r`nWord wrap=1`r`nDefault encoding=windows-1252`r`n[Font]`r`nName=Consolas`r`nSize=-13`r`n[Keyboard]`r`nSave As=Ctrl+Alt+S`r`nDuplicate Line=Ctrl+Shift+D`r`n[User Tools]`r`nMenu1=Build`r`nCommand1=make`r`n"
    [IO.File]::WriteAllBytes("$ep\editplus_u.ini", [Text.Encoding]::Unicode.GetPreamble() + [Text.Encoding]::Unicode.GetBytes($ini))
    $rs = "[Restore]`r`nCount=1`r`n[Restore\1]`r`nFile=C:\sample\notes.txt`r`nLine=4`r`n"
    [IO.File]::WriteAllBytes("$ep\restore_u.ini", [Text.Encoding]::Unicode.GetBytes($rs))   # no BOM
    Write-Utf8 "$ep\snippets.json" '{"html":{"snippets":{"a":"b"},"abbreviations":{"x":"y"}}}'
    Write-Utf8 "$ep\snip.ctl" "#TITLE=Snip`r`n#T=bold`r`n<b>^!</b>`r`n#`r`n"
    $ep
}

Invoke-Scenario 'f1' 'First run: 3 screens by keyboard, answers saved, shown once' {
    if (-not (Want 'f1')) { throw 'NOT_TESTABLE: skipped' }
    $d = New-TestEnv $DataRoot 'firstrun'
    Write-Utf8 "$d\settings\settings.toml" "[theme]`nmode = `"$Mode`"`n"
    Write-Utf8 "$d\files\a.txt" "alpha`nbeta`n"
    $env:APPDATA = "$d\appdata"; New-Item -ItemType Directory -Force "$d\appdata" | Out-Null   # nothing to detect on screen 2
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'firstrun'
    Shot $app 'fr-1-keyboard-and-theme'
    Send-Chord $app 'c' 200                         # Classic (letters: digits need Shift on AZERTY)
    Send-Chord $app ($(if ($Mode -eq 'dark') { 'l' } else { 'd' })) 500   # the other theme, previewed live
    Shot $app 'fr-1b-other-theme'
    Send-Chord $app ($(if ($Mode -eq 'dark') { 'd' } else { 'l' })) 500   # back to the requested mode for the proof shots
    Send-Chord $app 'enter' 500
    Shot $app 'fr-2-import'
    Send-Chord $app 'enter' 500
    Shot $app 'fr-3-privacy'
    Send-Chord $app 'u' 300                         # update check is ON by default: U turns it off
    Shot $app 'fr-3b-updates-off'
    Send-Chord $app 'enter' 700                     # Finish
    Shot $app 'fr-4-finished'
    $t = Settings-Text $d
    foreach ($want in @('done = true', 'classic = true', 'check = false')) { if ($t -notmatch [regex]::Escape($want)) { throw "settings.toml lacks '$want':`n$t" } }
    if ([IO.File]::ReadAllText("$d\files\a.txt") -ne "alpha`nbeta`n") { throw 'document changed' }
    Kill-App $app
    # Second launch: never again.
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'firstrun2'
    Shot $app2 'fr-5-second-launch'
    Open-ViaPalette $app2 'First-Run'
    Shot $app2 'fr-6-reopened-from-help'
    Send-Chord $app2 'esc' 500
    Shot $app2 'fr-7-skipped'
    "answers saved (classic, updates.check turned off with U, done); second launch shows no screens; Help reopens them; Esc skips"
}

Invoke-Scenario 'i1' 'Import EditPlus settings: detect, dry run, apply, live reload' {
    if (-not (Want 'i1')) { throw 'NOT_TESTABLE: skipped' }
    $d = New-TestEnv $DataRoot 'import'
    Write-Utf8 "$d\settings\settings.toml" "[firstrun]`ndone = true`n[theme]`nmode = `"$Mode`"`n"
    Write-Utf8 "$d\files\a.txt" "alpha`nbeta`n"
    $ep = New-FakeEditPlus $d
    $env:APPDATA = "$d\appdata"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'import'
    Open-ViaPalette $app 'Import EditPlus Settings'
    Shot $app 'imp-1-dry-run'
    # the dry run wrote nothing
    if (Test-Path "$d\settings\keymap.toml") { throw 'the dry run wrote keymap.toml' }
    Send-Chord $app 'ctrl+enter' 900
    Shot $app 'imp-2-applied'
    $t = Settings-Text $d
    foreach ($want in @('tab_size = 2', 'classic = true', 'default_encoding = "windows-1252"')) { if ($t -notmatch [regex]::Escape($want)) { throw "settings.toml lacks '$want':`n$t" } }
    if (-not (Test-Path "$d\settings\keymap.toml")) { throw 'keymap.toml not written' }
    if (-not (Test-Path "$d\settings\emmet\snippets.json")) { throw 'emmet data not kept' }
    Send-Chord $app 'esc' 500
    Shot $app 'imp-3-closed'
    "dry run listed items and wrote nothing; Ctrl+Enter applied; settings.toml/keymap.toml read back"
}

Invoke-Scenario 'a1' 'About page' {
    if (-not (Want 'a1')) { throw 'NOT_TESTABLE: skipped' }
    $d = New-TestEnv $DataRoot 'about'
    Write-Utf8 "$d\settings\settings.toml" "[firstrun]`ndone = true`n[theme]`nmode = `"$Mode`"`n"
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'about'
    Open-ViaPalette $app 'About Brouillon'
    Shot $app 'about-1'
    Send-Chord $app 'esc' 400
    Shot $app 'about-2-closed'
    'About page opened from the palette and closed with Esc'
}

Invoke-Scenario 'g1' 'Try an agent opens Connect with the Claude Code line highlighted' {
    if (-not (Want 'g1')) { throw 'NOT_TESTABLE: skipped' }
    $d = New-TestEnv $DataRoot 'agent'
    Write-Utf8 "$d\settings\settings.toml" "[firstrun]`ndone = true`n[theme]`nmode = `"$Mode`"`n"
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'agent'
    Open-ViaPalette $app 'Try an Agent'
    Shot $app 'agent-1-connect'
    Send-Chord $app 'esc' 400
    # From the first-run screen 3
    $d2 = New-TestEnv $DataRoot 'agent2'
    Write-Utf8 "$d2\settings\settings.toml" "[theme]`nmode = `"$Mode`"`n"
    Write-Utf8 "$d2\files\a.txt" "alpha`n"
    Kill-App $app
    $env:APPDATA = "$d2\appdata"; New-Item -ItemType Directory -Force "$d2\appdata" | Out-Null
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d2 -Files @("$d2\files\a.txt") -Tag 'agent2'
    Send-Chord $app2 'enter' 400; Send-Chord $app2 'enter' 400
    Shot $app2 'agent-2-screen3'
    Send-Chord $app2 'a' 900
    Shot $app2 'agent-3-from-firstrun'
    $t = Settings-Text $d2
    if ($t -notmatch 'done = true') { throw "first run not marked done:`n$t" }
    if ($t -match 'check = false') { throw "update check turned off without the user choosing it:`n$t" }
    'Connect tab opened from the palette and from first-run screen 3; updates.check stayed on (default)'
}

Invoke-Scenario 't1' 'Tab key follows editor.insert_spaces / tab_size' {
    if (-not (Want 't1')) { throw 'NOT_TESTABLE: skipped' }
    $d = New-TestEnv $DataRoot 'tab'
    Write-Utf8 "$d\settings\settings.toml" "[firstrun]`ndone = true`n[editor]`ntab_size = 3`ninsert_spaces = true`n"
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'tab'
    Send-Chord $app 'tab' 150
    Send-Text $app 'x'
    Send-Chord $app 'ctrl+s' 600
    Shot $app 'tab-1'
    $t = [IO.File]::ReadAllText("$d\files\a.txt")
    if ($t -ne "   xalpha`n") { throw "unexpected file content: $($t -replace ' ', '.')" }
    'Tab inserted 3 spaces (tab_size = 3)'
}

$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
