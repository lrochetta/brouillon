<#
.SYNOPSIS
  Sidebar show/hide and tab bar top/bottom: click the two menu-row icons (test-input backend), screenshot each state,
  read settings.toml back, then relaunch with the same settings dir and check the choices are restored.
  $env:BROUILLON_SMOKE_TI = '1'; powershell -NoProfile -File tools\smoke\sidebar-tabs.ps1 -Exe <dev exe built with --features test-input>
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-sidebar-tabs'), [string]$Tag = '', [double]$IconY = 62, [double]$SidebarX = 20, [double]$TabsX = 47)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot $(if ($env:BROUILLON_SMOKE_TI -eq '1') { 'testinput.ps1' } else { 'lib.ps1' }))
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Settings-Text($d) { $f = "$d\settings\settings.toml"; if (Test-Path $f) { [IO.File]::ReadAllText($f) } else { '' } }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }

Invoke-Scenario 'sidebar-tabs' 'Sidebar and tab bar position: toggle, persist, restore' {
    $d = New-TestEnv $DataRoot 'st'
    Write-Utf8 "$d\files\a.txt" "alpha`nbeta`n"
    Write-Utf8 "$d\files\b.txt" "second`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt", "$d\files\b.txt") -Tag 'st'
    Shot $app 'st-1-default'
    Send-Click $app $SidebarX $IconY
    Start-Sleep -Milliseconds 500
    Shot $app 'st-2-sidebar-hidden'
    if ((Settings-Text $d) -notmatch 'sidebar\.visible = false') { throw "no sidebar.visible = false:`n$(Settings-Text $d)" }
    Send-Click $app $TabsX $IconY
    Start-Sleep -Milliseconds 500
    Shot $app 'st-3-tabs-bottom-sidebar-hidden'
    if ((Settings-Text $d) -notmatch 'tabs\.position = "bottom"') { throw "no tabs.position = bottom:`n$(Settings-Text $d)" }
    Send-Chord $app 'ctrl+b' 500
    Shot $app 'st-4-ctrl-b-sidebar-shown-tabs-bottom'
    if ((Settings-Text $d) -notmatch 'sidebar\.visible = true') { throw 'ctrl+b did not show the sidebar' }
    Send-Chord $app 'ctrl+b' 500
    Send-Text $app 'typing still works'
    Shot $app 'st-5-hidden-and-typing'
    [void](Invoke-TI $app @{ op = 'quit' })
    Start-Sleep -Milliseconds 800
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt", "$d\files\b.txt") -Tag 'st2'
    Shot $app 'st-6-relaunch-restored'
    Send-Click $app $TabsX $IconY
    Start-Sleep -Milliseconds 500
    Send-Click $app $SidebarX $IconY
    Start-Sleep -Milliseconds 500
    Shot $app 'st-7-back-to-top-sidebar-shown'
    $t = Settings-Text $d
    if ($t -notmatch 'sidebar\.visible = true' -or $t -notmatch 'tabs\.position = "top"') { throw "final settings wrong:`n$t" }
    [void](Invoke-TI $app @{ op = 'quit' })
    "toggled by click and Ctrl+B, written to settings.toml, restored after relaunch"
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
