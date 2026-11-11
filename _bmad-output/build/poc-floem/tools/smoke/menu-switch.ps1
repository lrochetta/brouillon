<#
.SYNOPSIS
  Top-menu switching: with a menu open, ONE click on another menu title must open that menu.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\menu-switch.ps1 -Exe C:\path\brouillon.exe [-Tag before-]
  Same safety contract as run.ps1 (clicks/keys only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
  Coordinates are LOGICAL px from the window's outer top-left (menu titles at y=62).
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-menuswitch'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }

# Title centres (logical px), measured on a capture of the menu bar.
$T = [ordered]@{ File = 30; Edit = 84; Search = 138; View = 188; Tools = 238; Help = 313 }
$Y = 62

Invoke-Scenario 'ms' 'menu switching with one click' {
    $d = New-TestEnv $DataRoot 'ms'
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`nbeta`n", (New-Object Text.UTF8Encoding($false)))
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'ms'
    Shot $app 'ms-00-closed'
    # a) File
    Send-Click $app $T.File $Y; Shot $app 'ms-a-file'
    # b) File -> Edit directly (one click)
    Send-Click $app $T.Edit $Y; Shot $app 'ms-b-edit'
    # c) Edit -> Search -> View -> Tools -> Help, one click each
    foreach ($m in 'Search', 'View', 'Tools', 'Help') {
        Send-Click $app $T[$m] $Y; Shot $app ("ms-c-" + $m.ToLower())
    }
    # d) click the title of the open menu (Help) -> closes
    Send-Click $app $T.Help $Y; Shot $app 'ms-d-help-toggled-closed'
    # e) open File, click in the edit area -> closes
    Send-Click $app $T.File $Y; Shot $app 'ms-e-open'
    Send-Click $app 600 400; Shot $app 'ms-e-after-edit-click'
    # f) open Edit, Escape -> closes
    Send-Click $app $T.Edit $Y; Shot $app 'ms-f-open'
    Send-Chord $app 'esc'; Shot $app 'ms-f-after-esc'
    # g) Help > Show Shortcuts executes and closes the menu
    Send-Click $app $T.Help $Y; Shot $app 'ms-g-help-open'
    Send-Click $app 330 194                       # "Show Shortcuts" row (y 339 px / 1.75 DPI)
    Start-Sleep -Milliseconds 600
    Send-Click $app 600 600                       # park the pointer in the edit area (no hover on a title)
    Shot $app 'ms-g-after-show-shortcuts'
    'captures ms-*'
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
