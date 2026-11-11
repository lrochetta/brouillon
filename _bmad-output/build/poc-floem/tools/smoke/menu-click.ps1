<#
.SYNOPSIS
  ONE click must open each top menu / run each View entry, first time, however the click arrives.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\menu-click.ps1 -Exe <dev exe built with --features test-input> [-Tag x-]
  Test-input backend only (no real mouse, never blocks the owner). A "real" click here = move, a frame, press,
  a gap, release (the `down`/`up` ops), the way a hand does it; a "fast" click = the `click` op (move+press+release
  in one turn). Open/closed is read from a hash of the popup area of a screenshot. Coordinates are LOGICAL px
  from the window's outer top-left (menu titles span y=54..80, centre 62; popup rows from y=98, 24 px apart; the outer frame is ~32 px taller than the client).
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-menuclick'), [string]$Tag = '', [string[]]$Modes = @('real', 'real-nohover', 'fast', 'real-bottom-edge', 'real-top-edge'))
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null

$T = [ordered]@{ File = 90; Edit = 143; Search = 203; View = 263; Tools = 319; Help = 376 }   # title centres with the sidebar/tab-bar icons in front
$Y = 62
$POPUP = @(0, 70, 460, 420)   # logical x, y, w, h of the popup area

function Real-Click($app, [double]$x, [double]$y, [int]$GapMs = 70, [switch]$NoHover) {
    $c = ConvertTo-ClientXY $app $x $y
    if (-not $NoHover) { [void](Invoke-TI $app @{ op = 'move'; x = $c[0]; y = $c[1] }); Start-Sleep -Milliseconds 90 }
    [void](Invoke-TI $app @{ op = 'down'; x = $c[0]; y = $c[1] }); Start-Sleep -Milliseconds $GapMs
    [void](Invoke-TI $app @{ op = 'up'; x = $c[0]; y = $c[1] }); Start-Sleep -Milliseconds 220
}
function Fast-Move-Click($app, [double]$x, [double]$y) {
    # pointer sweeps in from the editor in 3 quick moves (no wait), then clicks at once
    foreach ($f in 0.4, 0.8) {
        $c = ConvertTo-ClientXY $app ($x + 120 * (1 - $f)) ($y + 90 * (1 - $f))
        [void](Invoke-TI $app @{ op = 'move'; x = $c[0]; y = $c[1] })
    }
    Send-Click $app $x $y
    Start-Sleep -Milliseconds 150
}
function Area($app) { $script:Deadline = (Get-Date).AddSeconds(40); Get-RegionHash $app -Logical $POPUP }
function Editor($app) { $script:Deadline = (Get-Date).AddSeconds(40); Get-RegionHash $app -Logical @(700, 200, 200, 200) }
# clicks needed (max 4) until the popup area equals $want (the reference picture of that menu).
function Count-Clicks($app, [scriptblock]$Do, [string]$want) {
    for ($n = 1; $n -le 4; $n++) { & $Do; if ((Area $app) -eq $want) { return $n } }
    return 99
}
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }

foreach ($mode in $Modes) { Invoke-Scenario "mc-$mode" "one click opens every menu and runs View entries ($mode)" {
    $d = New-TestEnv $DataRoot 'mc'
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`nbeta`n", (New-Object Text.UTF8Encoding($false)))
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'mc'
    Start-Sleep -Milliseconds 800
    $res = @()
    $refs = @{}
    foreach ($m in $T.Keys) {   # reference picture of each open menu: one patient click, then close
        Real-Click $app $T[$m] $Y; $refs[$m] = Area $app; Send-Click $app 600 600; Start-Sleep -Milliseconds 300
    }
    if (@($refs.Values | Select-Object -Unique).Count -ne 6) { throw 'reference pictures are not distinct' }
    foreach ($mode in @($mode)) {
        $click = switch ($mode) {
            'real' { { param($x) Real-Click $app $x $Y } }
            'real-nohover' { { param($x) Real-Click $app $x $Y -NoHover } }
            'fast' { { param($x) Fast-Move-Click $app $x $Y } }
            'real-bottom-edge' { { param($x) Real-Click $app $x ($Y + 17) } }
            'real-top-edge' { { param($x) Real-Click $app $x ($Y - 6) } }
        }
        foreach ($m in $T.Keys) {   # a) each title from closed
            $x = $T[$m]; $n = Count-Clicks $app { & $click $x } $refs[$m]
            $res += "$mode open-from-closed $m = $n click(s)"; Shot $app "mc-$mode-$($m.ToLower())"
            Send-Click $app 600 600; Start-Sleep -Milliseconds 300
        }
        & $click $T.File   # b) switching: File open, then each other title
        foreach ($m in 'Edit', 'Search', 'View', 'Tools', 'Help', 'File') {
            $x = $T[$m]; $n = Count-Clicks $app { & $click $x } $refs[$m]
            $res += "$mode switch -> $m = $n click(s)"
        }
        Send-Click $app 600 600; Start-Sleep -Milliseconds 300
        # c) View > theme rows: Light, Dark, Light, Dark (2 = Dark, 3 = Light); the editor colour must change after ONE click
        foreach ($row in 3, 2, 3, 2) {
            $before = Editor $app
            & $click $T.View
            $yy = 98 + 24 * $row; $n = 0
            for ($k = 1; $k -le 3; $k++) {
                if ($mode -eq 'fast') { Send-Click $app 270 $yy } else { Real-Click $app 270 $yy }
                if ((Editor $app) -ne $before) { $n = $k; break }
            }
            if ($n -eq 0) { $n = 99 }
            $res += "$mode View row $row = $n click(s)"; Shot $app "mc-$mode-view-row$row"
            Send-Click $app 600 600; Start-Sleep -Milliseconds 300
        }
    }
    $bad = @($res | Where-Object { $_ -notmatch '= 1 click' })
    $res | ForEach-Object { Write-Host "  $_" }
    if ($bad.Count) { throw ("more than one click needed: " + ($bad -join '; ')) }
    "all $($res.Count) cases opened with one click"
} }
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
if (@($script:Results | Where-Object { $_.Status -ne 'OK' }).Count) { exit 1 }
