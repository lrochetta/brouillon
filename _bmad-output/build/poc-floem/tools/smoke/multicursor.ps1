<#
.SYNOPSIS
  Multi-caret: Alt+click adds carets, typing / Backspace / Delete apply at all of them, one Ctrl+Z undoes
  the whole typed run, Esc collapses (mc-*.png). Also Ctrl+Alt+Down adds a caret below.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\multicursor.ps1 -Exe C:\path\brouillon.exe
  Same safety contract as run.ps1 (keys only while our window is foreground; TEMPORARY --settings-dir; process killed).
  The text is checked by saving (Ctrl+S) and reading the file back.
  Click geometry (logical px from the window's outer top-left): -TextX0/-TextY0 = top-left of line 1 col 1.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-mc'), [string]$Tag = '', [double]$TextX0 = 78, [double]$TextY0 = 112, [double]$CellW = 7.15, [double]$LineH = 18)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)
function Pos($line, $col) { @(($TextX0 + ($col - 1) * $CellW + 1), ($TextY0 + ($line - 1) * $LineH + 8)) }
function AltClick($app, $line, $col) {
    $p = Pos $line $col
    Send-Vk 0xA4 $false; Start-Sleep -Milliseconds 60
    Send-Click $app $p[0] $p[1]
    Send-Vk 0xA4 $true; Start-Sleep -Milliseconds 120
}
Invoke-Scenario 'mc' 'Multi-caret typing and undo' {
    $d = New-TestEnv $DataRoot 'm'
    $f = "$d\files\main.txt"
    [IO.File]::WriteAllText($f, "alpha`nbeta`ngamma`ndelta`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'mc'
    Save-Shot $app "${Tag}mc-0-start" | Out-Null
    $p = Pos 1 1; Send-Click $app $p[0] $p[1]
    AltClick $app 2 1
    AltClick $app 3 1
    AltClick $app 4 1
    Save-Shot $app "${Tag}mc-1-four-carets" | Out-Null
    Send-Text $app 'XY'
    Save-Shot $app "${Tag}mc-2-typed" | Out-Null
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne "XYalpha`nXYbeta`nXYgamma`nXYdelta`n") { throw "typed text wrong: $($t -replace "`n",'|')" }
    Send-Chord $app 'back' 200
    Send-Chord $app 'delete' 200
    Save-Shot $app "${Tag}mc-3-bs-del" | Out-Null
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne "Xlpha`nXeta`nXamma`nXelta`n") { throw "backspace+delete wrong: $($t -replace "`n",'|')" }
    # Every keystroke is ONE undo step covering all four carets: Delete, Backspace, then Y, then X.
    Send-Chord $app 'ctrl+z' 400
    Send-Chord $app 'ctrl+z' 400
    Send-Chord $app 'ctrl+s' 500
    Save-Shot $app "${Tag}mc-4-undo-twice" | Out-Null
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne "XYalpha`nXYbeta`nXYgamma`nXYdelta`n") { throw "after 2 undos expected the typed state back: $($t -replace "`n",'|')" }
    Send-Chord $app 'ctrl+z' 400
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne "Xalpha`nXbeta`nXgamma`nXdelta`n") { throw "one undo must revert the keystroke at ALL carets: $($t -replace "`n",'|')" }
    Send-Chord $app 'ctrl+z' 400
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne "alpha`nbeta`ngamma`ndelta`n") { throw "original not restored: $($t -replace "`n",'|')" }
    Send-Chord $app 'esc' 300
    Save-Shot $app "${Tag}mc-5-esc" | Out-Null
    'ok'
}
Invoke-Scenario 'mc2' 'Ctrl+Alt+Down adds carets, paste, Esc collapses' {
    $d = New-TestEnv $DataRoot 'm2'
    $f = "$d\files\main.txt"
    [IO.File]::WriteAllText($f, "one`ntwo`nthree`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'mc2'
    $p = Pos 1 1; Send-Click $app $p[0] $p[1]
    Send-Chord $app 'ctrl+alt+down' 300
    Send-Chord $app 'ctrl+alt+down' 300
    Save-Shot $app "${Tag}mc2-1-three-carets" | Out-Null
    Send-Chord $app 'end' 200
    Save-Shot $app "${Tag}mc2-2-end" | Out-Null
    Send-Text $app '!'
    Send-Chord $app 'esc' 300
    Send-Text $app '?'
    Save-Shot $app "${Tag}mc2-3-esc-then-q" | Out-Null
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne "one!`ntwo!`nthree!?`n") { throw "got: $($t -replace "`n",'|')" }
    'ok'
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize | Out-String | Write-Host
