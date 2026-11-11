<#
.SYNOPSIS
  Keystroke-to-presented-frame latency on a big file, no real input (test-input channel, dev feature only).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\typing-latency.ps1 -Exe <exe built with --features test-input> [-SizeMb 100] [-Chars 200] [-GapMs 60] [-SecondTab] [-Gpu]
  Types -Chars characters at the start, the middle and the end of the file (one character per request, -GapMs apart)
  and prints p50/p95/p99/max in ms of: injection -> end of the first frame painted afterwards (renderer finish call).
  Also prints private bytes / working set read through the channel.
#>
param([string]$Exe, [int]$SizeMb = 100, [int]$Chars = 200, [int]$GapMs = 60, [switch]$SecondTab, [switch]$Gpu, [string]$Root = 'D:\travail\_latroot')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $Root | Out-Null
$big = Join-Path $Root "f$SizeMb.txt"; $small = Join-Path $Root 'second.txt'
if (-not (Test-Path $big)) {
    $line = ('abcdefghij klmnopqrst uvwxyz 0123456789 ' * 3).Substring(0, 98) + "`n"
    [IO.File]::WriteAllText($big, ($line * [int]($SizeMb * 1MB / 99)))
}
[IO.File]::WriteAllText($small, ("second file`n" * 200))
$lines = [int]($SizeMb * 1MB / 99)
function Pct($a, [double]$p) { $s = @($a | Sort-Object); if ($s.Count -eq 0) { return [double]::NaN }; $s[[math]::Min($s.Count - 1, [int][math]::Ceiling($p * $s.Count) - 1)] }
Invoke-Scenario 'typing-latency' 'typing latency' {
    $d = New-TestEnv $Root 'env-lat'
    $files = @($big); if ($SecondTab) { $files += $small }
    $app = if ($Gpu) { Launch-Brouillon -Exe $Exe -Dir $d -Files $files -Tag 'lat' -Gpu } else { Launch-Brouillon -Exe $Exe -Dir $d -Files $files -Tag 'lat' }
    $script:Deadline = (Get-Date).AddSeconds(900)
    Start-Sleep -Milliseconds 2500
    $s = Get-State $app 5
    Write-Host "open: tabs=$($s.state.tab_count) active=$($s.state.tab_title) private=$([math]::Round($s.state.private_bytes/1MB,1)) ws=$([math]::Round($s.state.working_set/1MB,1)) MB"
    if ($SecondTab -and $s.state.tab_title -notlike 'f*') { Send-Chord $app 'ctrl+tab' 300 }
    $text = ('the quick brown fox ' * 20).Substring(0, $Chars)
    foreach ($pos in 'start', 'middle', 'end') {
        switch ($pos) {
            'start'  { Send-Chord $app 'ctrl+home' 400 }
            'middle' { Send-Chord $app 'ctrl+g' 300; Send-Text $app ([string][int]($lines / 2)) ; Send-Chord $app 'enter' 600 }
            'end'    { Send-Chord $app 'ctrl+end' 600 }
        }
        $r = Invoke-TI $app @{ op = 'latency'; text = $text; gap_ms = $GapMs } 120000
        $ms = @($r.ms)
        $st = Get-State $app 5
        Write-Host ("       handled p50={0:N2} p95={1:N2} | wait(handled->paint start) p50={2:N2} p95={3:N2} | paint+present p50={4:N2} p95={5:N2}" -f (Pct @($r.handled) .5), (Pct @($r.handled) .95), (Pct @($r.wait) .5), (Pct @($r.wait) .95), (Pct @($r.paint) .5), (Pct @($r.paint) .95))
        Write-Host ("{0,-6} n={1} missed={2} p50={3:N2} p95={4:N2} p99={5:N2} max={6:N2} mean={7:N2} ms | caret={8} len={9} private={10:N1} ws={11:N1}" -f $pos, $ms.Count, $r.missed, (Pct $ms .5), (Pct $ms .95), (Pct $ms .99), ($ms | Measure-Object -Maximum).Maximum, ($ms | Measure-Object -Average).Average, $st.state.caret, $st.state.text_len, ($st.state.private_bytes/1MB), ($st.state.working_set/1MB))
    }
    try { Stop-App $app } catch { }
}
