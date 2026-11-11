<#
.SYNOPSIS
  Column (rectangular) selection by Alt+drag, then the hex view (Ctrl+Shift+H).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\colhex.ps1 -Exe C:\path\brouillon.exe [-Tag x-]
  Same safety contract as run.ps1 (input only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-colhex'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }

Invoke-Scenario 'col' 'Alt+drag column selection' {
    $d = New-TestEnv $DataRoot 'col'
    [IO.File]::WriteAllText("$d\files\grid.txt", "abcdefghij`nklmnopqrst`nuvwxyz0123`n456789ABCD`n", (New-Object Text.UTF8Encoding($false)))
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\grid.txt") -Tag 'col'
    Shot $app 'col-0-start'
    Send-Vk (Resolve-Vk 'alt') $false; Start-Sleep -Milliseconds 80
    try { Send-Drag $app 262 143 305 197 } finally { Send-Vk (Resolve-Vk 'alt') $true }
    Shot $app 'col-1-after-alt-drag'
    'captures col-*'
}
Invoke-Scenario 'hex' 'hex view toggle' {
    $d = New-TestEnv $DataRoot 'hex'
    [IO.File]::WriteAllText("$d\files\h.txt", "Hello hex view`nsecond line`n", (New-Object Text.UTF8Encoding($false)))
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\h.txt") -Tag 'hex'
    Send-Chord $app 'ctrl+shift+h' 400
    Shot $app 'hex-1-ctrl-shift-h'
    Send-Chord $app 'ctrl+shift+h' 400
    Shot $app 'hex-2-toggled-back'
    'captures hex-*'
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
