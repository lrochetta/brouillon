<#
.SYNOPSIS
  Data-integrity smoke scenarios (stabilisation V0): crayon-mode close, crash recovery, session round trip.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\v0a.ps1 -Exe C:\path\brouillon.exe -Only m,k,d -Tag before-
  Same safety contract as run.ps1 (keys/clicks only while our window is the foreground one).
#>
param([string]$Exe, [string[]]$Only, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-v0a'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
if ($Only) { $Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ }) }
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Want([string]$id) { (-not $Only) -or ($Only -contains $id) }
function Wait-Until([scriptblock]$Cond, [int]$Ms = 5000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $Ms) { if (& $Cond) { return $true }; Start-Sleep -Milliseconds 100 }
    $false
}
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }

# ---- m: crayon mode, draw strokes on a scratch tab, close the tab
if (Want 'm') { Invoke-Scenario 'm' 'crayon strokes: closing the tab must not lose them silently' {
    $d = New-TestEnv $DataRoot 'm'
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'm1'
    Send-Chord $app 'ctrl+n'
    Send-Click $app 617 91
    Start-Sleep -Milliseconds 600
    Shot $app 'm1-crayon-on'
    Send-Drag $app 400 300 800 450
    Send-Drag $app 450 500 750 350
    Shot $app 'm2-drawn'
    Send-Chord $app 'ctrl+w'
    Start-Sleep -Milliseconds 1000
    Shot $app 'm3-after-ctrl-w'
    $fg = Get-ForegroundPid
    $sess = Get-SessionText $d
    $detail = "foreground pid after Ctrl+W = $fg (app $($app.Proc.Id)); tabs in session: $(([regex]::Matches($sess, '\[\[tabs\]\]')).Count)"
    try { Send-Chord $app 'esc' } catch {}
    $detail
} }

# ---- m3: draw, close the tab, answer Yes (Oui), pick a file name: text + drawing reach the disk and reload
if (Want 'm3') { Invoke-Scenario 'm3' 'crayon strokes: Save before close writes <file> and <file>.sketch.json' {
    $d = New-TestEnv $DataRoot 'm3'
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'o1'
    Send-Chord $app 'ctrl+n'
    Send-Click $app 617 91
    Start-Sleep -Milliseconds 500
    Send-Drag $app 400 300 800 450
    Send-Drag $app 450 500 750 350
    Send-Chord $app 'ctrl+w'
    Start-Sleep -Milliseconds 900
    Shot $app 'o1-prompt'
    Send-Chord $app 'enter'            # default button = Yes / Oui
    Start-Sleep -Milliseconds 1200
    Shot $app 'o2-save-dialog'
    Send-Text $app "$d\files\drawn.txt"
    Send-Chord $app 'enter'
    $ok = Wait-Until { Test-Path "$d\files\drawn.txt.sketch.json" } 6000
    Start-Sleep -Milliseconds 600
    Shot $app 'o3-after-save'
    $json = if ($ok) { Get-Content -Raw "$d\files\drawn.txt.sketch.json" } else { '' }
    $strokes = ([regex]::Matches($json, '"tool"')).Count
    Kill-App $app
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\drawn.txt") -Extra @('--new-window', '--no-session') -Tag 'o2'
    Send-Click $app2 617 91
    Start-Sleep -Milliseconds 600
    Shot $app2 'o4-reopened-drawn'
    "text file exists=$(Test-Path "$d\files\drawn.txt"); sidecar exists=$ok; strokes in sidecar=$strokes; see o1..o4"
} }

# ---- m2: strokes survive a graceful close + relaunch
if (Want 'm2') { Invoke-Scenario 'm2' 'crayon strokes persisted across restart' {
    $d = New-TestEnv $DataRoot 'm2'
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'n1'
    Send-Chord $app 'ctrl+n'
    Send-Click $app 617 91
    Start-Sleep -Milliseconds 500
    Send-Drag $app 400 300 800 450
    Send-Drag $app 450 500 750 350
    Shot $app 'n1-drawn'
    Start-Sleep -Milliseconds 800
    $r = Stop-AppGraceful $app
    $sess = Get-SessionText $d
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'n2'
    Send-Click $app2 617 91
    Start-Sleep -Milliseconds 600
    Shot $app2 'n2-reopened-crayon'
    "close=$r; session mentions sketch=$($sess -match 'sketch'); see n2-reopened-crayon.png"
} }

# ---- k: kill -9 then relaunch
if (Want 'k') { Invoke-Scenario 'k' 'hard kill then relaunch: recovery marks modified' {
    $d = New-TestEnv $DataRoot 'k'
    Write-Utf8 "$d\files\k.txt" "original`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\k.txt") -Tag 'k1'
    Send-Text $app 'UNSAVED_K'
    Shot $app 'k1-typed-unsaved'
    $jdir = "$d\settings"
    $got = Wait-Until { @(Get-ChildItem $jdir -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.FullName -match 'recover' }).Count -gt 0 } 45000
    Kill-App $app
    Start-Sleep -Milliseconds 500
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'k2'
    Shot $app2 'k2-relaunch'
    $err2 = Read-Stderr $app2
    $disk = (Get-Content -Raw "$d\files\k.txt")
    "journal=$got; recovered=$([regex]::Match($err2, '(\d+) document\(s\) recovered').Groups[1].Value); disk='$($disk.Trim())'; see k2-relaunch.png"
} }

# ---- d: close then reopen
if (Want 'd') { Invoke-Scenario 'd' 'session: titles, no duplicates, active tab, cursor, dirty text' {
    $d = New-TestEnv $DataRoot 'd'
    Write-Utf8 "$d\files\one.txt" (((1..120 | ForEach-Object { "line $_ of file one" }) -join "`n") + "`n")
    Write-Utf8 "$d\files\two.txt" (((1..120 | ForEach-Object { "second $_" }) -join "`n") + "`n")
    Write-Utf8 "$d\files\three.txt" "three`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\one.txt", "$d\files\two.txt", "$d\files\three.txt") -Tag 'd1'
    Send-Chord $app 'ctrl+g'; Send-Text $app "40`n"
    Send-Text $app 'MARK'
    Shot $app 'd2-line40-dirty'
    Send-Chord $app 'ctrl+n'; Send-Text $app 'scratch-text-survives'
    Shot $app 'd3-scratch'
    Send-Chord $app 'ctrl+pgup'
    Shot $app 'd4-before-close'
    $r = Stop-AppGraceful $app
    $sess = Get-SessionText $d
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'd2'
    Shot $app2 'd5-reopened'
    $err2 = Read-Stderr $app2
    $titles = ([regex]::Matches($sess, 'title = [''"]([^''"]*)[''"]') | ForEach-Object { $_.Groups[1].Value }) -join ','
    "close=$r; session scratch titles=[$titles]; tabs in session=$(([regex]::Matches($sess, '\[\[tabs\]\]')).Count); stderr: $(($err2 -split "`n" | Where-Object { $_ -match 'restored|recovered' }) -join ' | ')"
} }

$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue

