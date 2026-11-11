<#
.SYNOPSIS
  External changes to an open file: reload when untouched, bar + guarded save when edited,
  deleted file, and the ordinary save. Every outcome is checked on the DISK (bytes) and read on captures.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\external-change.ps1 -Exe C:\path\brouillon.exe [-Only a,b1] [-Tag x-]
  Same safety contract as run.ps1 (keys/clicks only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
  Scenarios: a (untouched + external change -> reloaded), b (edited + external change -> bar, Ctrl+S -> choice bar,
  disk untouched), b1 Overwrite, b2 Reload, b3 Save As, c (deleted), d (plain save).
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-extchange'), [string]$Tag = '', [string[]]$Only = @())
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
$U = New-Object Text.UTF8Encoding($false)
$Only = @($Only | ForEach-Object { $_ -split ',' })   # -File passes 'a,b' as one string
function Want($id) { $Only.Count -eq 0 -or $Only -contains $id }
function Disk([string]$p) { if (Test-Path $p) { $U.GetString([IO.File]::ReadAllBytes($p)) } else { '<missing>' } }
function Expect([string]$p, [string]$want, [string]$what) {
    $got = Disk $p
    if ($got -ne $want) { throw ("DISK_MISMATCH {0}: wanted {1} got {2}" -f $what, ($want -replace "`n", '\n'), ($got -replace "`n", '\n')) }
}
$ORIG = "line 1 alpha foo`nline 2 beta`nline 3 gamma`n"
$EXT = "line 1 EXTERNAL`nline 2 beta`nline 3 gamma`nline 4 added by another program`n"

# Button centres (logical px from the window's outer top-left); measured on captures.
$BarY = 127   # bar row: was 151 (capture y 265 px / 1.75 DPI) before f694fd3 removed the 24 px toolbar row (in-flow banner, -24); not re-measured
$BtnOverwrite = @(675, $BarY); $BtnReloadSave = @(859, $BarY); $BtnSaveAs = @(1048, $BarY)

function Open-Doc($d, $tag) {
    [IO.File]::WriteAllText("$d\files\a.txt", $ORIG, $U)
    Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag $tag
}
function Touch-External($path, $text) { Start-Sleep -Milliseconds 1200; [IO.File]::WriteAllText($path, $text, $U) }

if (Want 'a') { Invoke-Scenario 'a' 'untouched document + external change -> reloaded' {
    $d = New-TestEnv $DataRoot 'a'; $f = "$d\files\a.txt"
    $app = Open-Doc $d 'ec-a'
    Shot $app 'ec-a-0-open'
    Touch-External $f $EXT
    Start-Sleep -Milliseconds 3000
    Shot $app 'ec-a-1-reloaded'
    # Prove the buffer holds the external text: append, save, no conflict -> disk = ZZ + external.
    Send-Text $app 'ZZ '; Send-Chord $app 'ctrl+s' 600
    Shot $app 'ec-a-2-saved-after-reload'
    Expect $f ('ZZ ' + $EXT) 'a'
    'reloaded, then Ctrl+S wrote external text + ZZ without a conflict'
} }

if (Want 'b') { Invoke-Scenario 'b' 'edited + external change -> bar, Ctrl+S -> choices, disk untouched' {
    $d = New-TestEnv $DataRoot 'b'; $f = "$d\files\a.txt"
    $app = Open-Doc $d 'ec-b'
    Send-Text $app 'MINE '
    Shot $app 'ec-b-0-edited'
    Touch-External $f $EXT
    Start-Sleep -Milliseconds 3000
    Shot $app 'ec-b-1-changed-bar'
    Expect $f $EXT 'b before save'
    Send-Chord $app 'ctrl+s' 800
    Shot $app 'ec-b-2-save-refused'
    Expect $f $EXT 'b after Ctrl+S (must NOT be overwritten)'
    Start-Sleep -Milliseconds 2500
    Shot $app 'ec-b-3-still-waiting'
    Expect $f $EXT 'b after waiting'
    'external bytes intact after Ctrl+S'
} }

function Conflict-Setup($id) {
    $d = New-TestEnv $DataRoot $id; $f = "$d\files\a.txt"
    $app = Open-Doc $d "ec-$id"
    Send-Text $app 'MINE '
    Touch-External $f $EXT
    Start-Sleep -Milliseconds 2500
    Send-Chord $app 'ctrl+s' 900
    Shot $app "ec-$id-0-choices"
    Expect $f $EXT "$id after Ctrl+S"
    [pscustomobject]@{ App = $app; File = $f; Dir = $d }
}

if (Want 'b1') { Invoke-Scenario 'b1' 'choice Overwrite' {
    $s = Conflict-Setup 'b1'
    Send-Click $s.App $BtnOverwrite[0] $BtnOverwrite[1]
    Start-Sleep -Milliseconds 800; Shot $s.App 'ec-b1-1-after'
    Expect $s.File ("MINE " + $ORIG) 'b1 overwritten with my text'
    'disk = my text'
} }
if (Want 'b2') { Invoke-Scenario 'b2' 'choice Reload from disk' {
    $s = Conflict-Setup 'b2'
    Send-Click $s.App $BtnReloadSave[0] $BtnReloadSave[1]
    Start-Sleep -Milliseconds 800; Shot $s.App 'ec-b2-1-after'
    Expect $s.File $EXT 'b2 external kept'
    'disk = external; editor shows external text'
} }
if (Want 'b3') { Invoke-Scenario 'b3' 'choice Save As' {
    $s = Conflict-Setup 'b3'
    Send-Click $s.App $BtnSaveAs[0] $BtnSaveAs[1]
    Start-Sleep -Milliseconds 1500; Shot $s.App 'ec-b3-1-dialog'
    Send-Text $s.App ($s.Dir + '\files\mine-copy.txt'); Send-Chord $s.App 'enter' 1200
    Shot $s.App 'ec-b3-2-after'
    Expect $s.File $EXT 'b3 original external kept'
    Expect ($s.Dir + '\files\mine-copy.txt') ("MINE " + $ORIG) 'b3 copy has my text'
    'original = external, mine-copy.txt = my text'
} }

if (Want 'c') { Invoke-Scenario 'c' 'file deleted while open' {
    $d = New-TestEnv $DataRoot 'c'; $f = "$d\files\a.txt"
    $app = Open-Doc $d 'ec-c'
    Start-Sleep -Milliseconds 1200; Remove-Item $f
    Start-Sleep -Milliseconds 3000
    Shot $app 'ec-c-1-gone-bar'
    Send-Text $app 'KEPT '; Send-Chord $app 'ctrl+s' 800
    Shot $app 'ec-c-2-after-save'
    Expect $f ("KEPT " + $ORIG) 'c recreated by Ctrl+S'
    'bar shown; Ctrl+S recreated the file with the text still in the editor'
} }

if (Want 'd') { Invoke-Scenario 'd' 'ordinary save, no conflict' {
    $d = New-TestEnv $DataRoot 'd'; $f = "$d\files\a.txt"
    $app = Open-Doc $d 'ec-d'
    Send-Text $app 'PLAIN '; Send-Chord $app 'ctrl+s' 800
    Shot $app 'ec-d-1-saved'
    Expect $f ("PLAIN " + $ORIG) 'd'
    Send-Text $app 'AGAIN '; Send-Chord $app 'ctrl+s' 800
    Shot $app 'ec-d-2-saved-twice'
    Expect $f ("PLAIN AGAIN " + $ORIG) 'd twice'
    'two consecutive saves, no bar'
} }

$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
