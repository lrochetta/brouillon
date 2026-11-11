<#
.SYNOPSIS
  Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+PgDn / Ctrl+PgUp switch tabs (default keymap, Classic overlay OFF).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\tabs-ctrltab.ps1 -Exe C:\path\brouillon.exe [-Tag x-]
  Same safety contract as run.ps1 (keys only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
  The active tab is proven by the status-bar / title strip changing (region hash) and read on the captures.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-tabs'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }

Invoke-Scenario 'tab' 'Ctrl+Tab cycles tabs' {
    $d = New-TestEnv $DataRoot 'tab'
    $u = New-Object Text.UTF8Encoding($false)
    [IO.File]::WriteAllText("$d\files\one.txt", "ONE-FILE`n", $u)
    [IO.File]::WriteAllText("$d\files\two.txt", "TWO-FILE`n", $u)
    [IO.File]::WriteAllText("$d\files\three.txt", "THREE-FILE`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\one.txt", "$d\files\two.txt", "$d\files\three.txt") -Tag 'tab'
    # The editor body (logical px) changes with the active document's text.
    $body = @(60.0, 130.0, 600.0, 120.0)
    $h0 = Get-RegionHash $app -Logical $body
    Shot $app 'tab-0-start'
    $seen = @{}
    $seq = New-Object System.Collections.ArrayList
    foreach ($step in 'ctrl+tab', 'ctrl+tab', 'ctrl+tab', 'ctrl+shift+tab', 'ctrl+pgdn', 'ctrl+pgup') {
        Send-Chord $app $step 200
        $h = Get-RegionHash $app -Logical $body
        [void]$seq.Add("$step=" + $h.Substring(0, 5))
        Shot $app ('tab-' + ($step -replace '\+', '-') + '-' + $seq.Count)
    }
    ($seq -join '  ') + "  (start=" + $h0.Substring(0, 5) + ")"
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
