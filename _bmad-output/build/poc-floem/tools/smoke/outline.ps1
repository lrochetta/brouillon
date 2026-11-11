<#
.SYNOPSIS
  Outline picker (Go to Function, Ctrl+F11 in the Classic keymap), Toggle Line Numbers (Ctrl+Shift+L) and
  code folding with the mouse (gutter marker) and the keyboard (ol-*.png).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\outline.ps1 -Exe C:\path\brouillon.exe
  Same safety contract as run.ps1 (keys only while our window is foreground; TEMPORARY --settings-dir; process killed).
  Geometry (logical px from the window's outer top-left): -TextX0/-TextY0 = top-left of line 1 col 1, -MarkX = fold marker column.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-ol'), [string]$Tag = '', [double]$TextX0 = 253, [double]$TextY0 = 142, [double]$CellW = 7.2, [double]$LineH = 18, [double]$MarkX = 231)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot $(if ($env:BROUILLON_SMOKE_TI -eq '1') { 'testinput.ps1' } else { 'lib.ps1' }))
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)
$script:VK['['] = 0xDB; $script:VK[']'] = 0xDD   # VK_OEM_4 / VK_OEM_6 (US layout)
function Pos($line, $col) { @(($TextX0 + ($col - 1) * $CellW + 1), ($TextY0 + ($line - 1) * $LineH + 8)) }
$src = "import os`n`ndef alpha():`n    return 1`n`nclass Beta:`n    def gamma(self):`n        return 2`n`ndef delta():`n    return 3`n"

Invoke-Scenario 'ol' 'Go to Function picker, line numbers toggle' {
    $d = New-TestEnv $DataRoot 'o'
    [IO.File]::WriteAllText("$d\settings\settings.toml", "[keymap]`nclassic = true`n", $u)
    $f = "$d\files\sample.py"
    [IO.File]::WriteAllText($f, $src, $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'ol'
    Save-Shot $app "${Tag}ol-0-start" | Out-Null
    Send-Chord $app 'ctrl+f11' 600
    Save-Shot $app "${Tag}ol-1-picker" | Out-Null
    Send-Text $app 'gam'
    Save-Shot $app "${Tag}ol-2-filtered" | Out-Null
    Send-Chord $app 'enter' 500
    Save-Shot $app "${Tag}ol-3-jumped" | Out-Null
    # Ctrl+Shift+L: numbers off, then on again
    Send-Chord $app 'ctrl+shift+l' 500
    Save-Shot $app "${Tag}ol-4-numbers-off" | Out-Null
    $st = Get-Content -Raw "$d\settings\settings.toml"
    if ($st -notmatch 'line_numbers\s*=\s*false') { throw "setting not saved: $st" }
    Send-Chord $app 'ctrl+shift+l' 500
    Save-Shot $app "${Tag}ol-5-numbers-on" | Out-Null
    $st = Get-Content -Raw "$d\settings\settings.toml"
    if ($st -notmatch 'line_numbers\s*=\s*true') { throw "setting not saved back: $st" }
    # nothing leaked into the document
    Send-Chord $app 'ctrl+s' 400
    if ([IO.File]::ReadAllText($f) -cne $src) { throw 'LEAK: document changed' }
    'ok'
}

Invoke-Scenario 'fold' 'Folding: gutter click, caret/click mapping, fold all / unfold all' {
    $d = New-TestEnv $DataRoot 'f'
    $f = "$d\files\sample.py"
    [IO.File]::WriteAllText($f, $src, $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'fold'
    Save-Shot $app "${Tag}fold-0-start" | Out-Null
    $p = Pos 3 1
    Send-Click $app $MarkX $p[1]            # fold `def alpha():` (hides line 4)
    Save-Shot $app "${Tag}fold-1-alpha-folded" | Out-Null
    # After the fold the visible source lines are 1,2,3,6,7,8,... : the 5th visible row shows source line 7 `def gamma(self):`.
    $p = Pos 5 1
    Send-Click $app $p[0] $p[1]
    Send-Chord $app 'end' 200
    Send-Text $app 'X'
    Save-Shot $app "${Tag}fold-2-click-after-fold" | Out-Null
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne $src.Replace("def gamma(self):", "def gamma(self):X")) { throw "click after a fold edited the wrong line: $($t -replace "`n",'|')" }
    Send-Chord $app 'ctrl+shift+[' 400      # fold all
    Save-Shot $app "${Tag}fold-3-fold-all" | Out-Null
    Send-Chord $app 'ctrl+shift+]' 400      # unfold all
    Save-Shot $app "${Tag}fold-4-unfold-all" | Out-Null
    'ok'
}
Invoke-Scenario 'fold2' 'Fold regions follow edits (new block gets a marker and folds)' {
    $d = New-TestEnv $DataRoot 'f2'
    $f = "$d\files\sample.py"
    [IO.File]::WriteAllText($f, $src, $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'fold2'
    $p = Pos 1 1; Send-Click $app $p[0] $p[1]
    Send-Chord $app 'end' 200
    Send-Chord $app 'enter' 200
    Send-Text $app 'def zeta():'
    Send-Chord $app 'enter' 200
    Send-Text $app 'return 0'
    Save-Shot $app "${Tag}fold2-1-new-block" | Out-Null
    $p = Pos 2 1
    Send-Click $app $MarkX $p[1]            # fold the new `def zeta():`
    Save-Shot $app "${Tag}fold2-2-new-block-folded" | Out-Null
    'ok'
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize | Out-String | Write-Host
