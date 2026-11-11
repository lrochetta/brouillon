<#
.SYNOPSIS
  Visible text caret: start, after typing, after moving down/right (caret-*.png).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\caret.ps1 -Exe C:\path\brouillon.exe
  Same safety contract as run.ps1 (keys only while our window is foreground; TEMPORARY --settings-dir; process killed).
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-caret'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)
Invoke-Scenario 'caret' 'Text caret follows typing and movement' {
    $d = New-TestEnv $DataRoot 'c'
    [IO.File]::WriteAllText("$d\files\main.txt", "first line`nsecond line`nthird line`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\main.txt") -Tag 'caret'
    Save-Shot $app "${Tag}caret-0-start" | Out-Null
    Send-Text $app 'abc'
    Save-Shot $app "${Tag}caret-1-typed" | Out-Null
    Send-Chord $app 'down' 300
    Send-Chord $app 'down' 300
    Save-Shot $app "${Tag}caret-2-down" | Out-Null
}
