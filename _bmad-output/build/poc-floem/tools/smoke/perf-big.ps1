<#
.SYNOPSIS
  Latency / memory benchmark on a generated large file (default 100 MB), measured on screen.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\perf-big.ps1 -Exe C:\path\brouillon.exe [-SizeMb 100] [-Runs 5] [-Root D:\scratch\perf] [-Keep]
  Same safety contract as run.ps1: keys only while our window is the foreground one, TEMPORARY
  --settings-dir, BROUILLON_NO_UPDATE=1, the process is always killed. The generated file lives in
  -Root and is deleted at the end (unless -Keep).
  Latency = time from SendInput to the next change of a hash of the status-bar strip (the capture
  itself costs ~17 ms; this is an upper bound). Output: one line per metric (median, min, p95, n),
  and working-set samples.
#>
param([string]$Exe, [int]$SizeMb = 100, [int]$Runs = 5, [string]$Root = 'D:\travail\_perfE', [switch]$Keep, [switch]$Typing, [switch]$Scroll, [switch]$Small)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
if (-not $Typing -and -not $Scroll) { $Typing = $true; $Scroll = $true }
New-Item -ItemType Directory -Force $Root | Out-Null

if ($Small) { $SizeMb = 0 }
$big = Join-Path $Root "big$SizeMb.txt"
if (-not (Test-Path $big)) {
    $line = ('abcdefghij klmnopqrst uvwxyz 0123456789 ' * 3).Substring(0, 98) + "`n"
    $target = [long]$SizeMb * 1MB; if ($Small) { $target = 4000 }
    if ($Small) { [IO.File]::WriteAllText($big, ($line * 50)) } else {
    $fs = [IO.File]::Create($big)
    $bytes = [Text.Encoding]::ASCII.GetBytes($line)
    $chunk = New-Object byte[] ($bytes.Length * 10000)
    for ($i = 0; $i -lt 10000; $i++) { [Array]::Copy($bytes, 0, $chunk, $i * $bytes.Length, $bytes.Length) }
    $written = 0L
    while ($written -lt $target) { $fs.Write($chunk, 0, $chunk.Length); $written += $chunk.Length }
    $fs.Close() }
}

function Stat($name, $vals) {
    $v = @($vals | Sort-Object)
    if ($v.Count -eq 0) { Write-Host ("{0,-34} no sample" -f $name); return }
    $med = $v[[int][math]::Floor(($v.Count - 1) / 2)]
    $p95 = $v[[int][math]::Min($v.Count - 1, [math]::Ceiling($v.Count * 0.95) - 1)]
    Write-Host ("{0,-34} n={1,-3} min={2,-6} median={3,-6} p95={4,-6} max={5}" -f $name, $v.Count, $v[0], $med, $p95, $v[-1])
}

# Hash of a small region, polled until it differs from $before; returns ms since $sw started.
function Wait-Change($app, $before, $sw, $logical, $timeoutMs = 4000) {
    while ($sw.ElapsedMilliseconds -lt $timeoutMs) {
        $h = Get-RegionHash -App $app -Logical $logical
        if ($h -ne $before) { return $sw.ElapsedMilliseconds }
    }
    return -1
}

$typeLat = New-Object System.Collections.ArrayList
$pgLat = New-Object System.Collections.ArrayList
$memIdle = New-Object System.Collections.ArrayList
$memPeak = New-Object System.Collections.ArrayList
$openMs = New-Object System.Collections.ArrayList
for ($run = 1; $run -le $Runs; $run++) {
    Invoke-Scenario "perf-$run" "large file run $run" {
        $d = New-TestEnv $Root "env$run"
        $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($big) -Tag "perf-$run"
        Start-Sleep -Milliseconds 3000   # let the open / background work settle
        Set-AppFocus $app
        $m0 = Get-Mem $app
        [void]$memIdle.Add($m0.WorkingSetMB)
        # Status-bar strip: bottom 24 logical px of the window.
        $r = New-Object RECT
        [void][W32]::GetWindowRect($app.Hwnd, [ref]$r)
        $sc = [W32]::GetDpiForWindow($app.Hwnd) / 96.0
        $hLog = ($r.Bottom - $r.Top) / $sc; $wLog = ($r.Right - $r.Left) / $sc
        $strip = @(0.0, ($hLog - 30.0), $wLog, 24.0)
        if ($Typing) {
            for ($i = 0; $i -lt 12; $i++) {
                $script:Deadline = (Get-Date).AddSeconds(40)
                Assert-Focus $app
                $before = Get-RegionHash -App $app -Logical $strip
                $sw = [Diagnostics.Stopwatch]::StartNew()
                [void][W32]::Key([uint16][int][char]'X', $false, $false, $false, 0)
                [void][W32]::Key([uint16][int][char]'X', $true, $false, $false, 0)
                $ms = Wait-Change $app $before $sw $strip
                if ($ms -ge 0 -and $i -ge 2) { [void]$typeLat.Add($ms) }   # first two are warm-up
                Start-Sleep -Milliseconds 400
            }
            # Undo: the typed text must be removed (checked with the saved file size by the caller).
            Save-Shot $app "perf-$run-typed" 300 | Out-Null
        }
        if ($Scroll) {
            for ($i = 0; $i -lt 12; $i++) {
                $script:Deadline = (Get-Date).AddSeconds(40)
                Assert-Focus $app
                $r2 = New-Object RECT
                [void][W32]::GetWindowRect($app.Hwnd, [ref]$r2)
                $gut = @(185.0, 140.0, 55.0, 450.0)   # line-number gutter (right of the 180 px sidebar)
                $before = Get-RegionHash -App $app -Logical $gut
                $sw = [Diagnostics.Stopwatch]::StartNew()
                Send-Vk ([uint16]0x22) $false; Send-Vk ([uint16]0x22) $true
                $ms = Wait-Change $app $before $sw $gut
                if ($ms -ge 0 -and $i -ge 2) { [void]$pgLat.Add($ms) }
                Start-Sleep -Milliseconds 400
            }
            Save-Shot $app "perf-$run-scrolled" 300 | Out-Null
        }
        $m1 = Get-Mem $app
        [void]$memPeak.Add($m1.PeakMB)
        $err = Read-Stderr $app
        $perf = ($err -split "`n" | Where-Object { $_ -match 'cold start' } | Select-Object -Last 1) -join "`n"
        Set-Content -Path (Join-Path $Root "perf-$run.stderr.txt") -Value $err
        "ws_idle=$($m0.WorkingSetMB) MB, ws_now=$($m1.WorkingSetMB) MB, peak=$($m1.PeakMB) MB`n$perf"
    }
}
Write-Host '---- summary ----'
if ($Typing) { Stat 'keystroke latency ms (status bar)' $typeLat }
if ($Scroll) { Stat 'PgDn latency ms (gutter)' $pgLat }
Stat 'working set 3 s after open, MB' $memIdle
Stat 'peak working set, MB' $memPeak
if (-not $Keep) { Remove-Item -Recurse -Force $Root -ErrorAction SilentlyContinue }
