<#
.SYNOPSIS
  Memory on a big file without real input: the leak.ps1 'mixed' mode driven through the test-input channel.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\mem-mixed.ps1 -Exe <test-input exe> [-Cycles 40] [-SizeMb 100] [-DhatOut file.json]
  Prints private bytes / working set after open, after the cycles, and after the idle trim (35 s quiet).
#>
param([string]$Exe, [int]$Cycles = 40, [int]$SizeMb = 100, [string]$DhatOut = '', [switch]$NoSecond, [switch]$Gpu, [string]$Resize = '', [string]$Root = 'D:\travail\_memroot')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
if ($DhatOut) { $env:BROUILLON_DHAT_OUT = $DhatOut; $env:BROUILLON_DHAT_TRIGGER = ($DhatOut + '.trigger'); Remove-Item $DhatOut, $env:BROUILLON_DHAT_TRIGGER -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force $Root | Out-Null
$big = Join-Path $Root "f$SizeMb.txt"; $small = Join-Path $Root 'second.txt'
if (-not (Test-Path $big)) { $line = ('abcdefghij klmnopqrst uvwxyz 0123456789 ' * 3).Substring(0, 98) + "`n"; [IO.File]::WriteAllText($big, ($line * [int]($SizeMb * 1MB / 99))) }
[IO.File]::WriteAllText($small, ("second file`n" * 200))
function Mem($app, $label) { $s = Get-State $app 1; Write-Host ("{0,-14} private={1,6:N1} ws={2,6:N1} MB tab={3}" -f $label, ($s.state.private_bytes/1MB), ($s.state.working_set/1MB), $s.state.tab_title) }
Invoke-Scenario 'mem-mixed' 'memory mixed' {
    $d = New-TestEnv $Root 'env-mem'
    $files = @($big); if (-not $NoSecond) { $files += $small }
    $app = if ($Gpu) { Launch-Brouillon -Exe $Exe -Dir $d -Files $files -Tag 'mem' -Gpu } else { Launch-Brouillon -Exe $Exe -Dir $d -Files $files -Tag 'mem' }
    $script:Deadline = (Get-Date).AddSeconds(1500)
    Start-Sleep -Seconds 4
    Mem $app 'open+4s'
    if ($Resize) { $wh = $Resize -split 'x'; [void](Invoke-TI $app @{ op = 'resize'; w = [double]$wh[0]; h = [double]$wh[1] }); Start-Sleep -Seconds 2; Send-Chord $app 'pgdn' 300; Mem $app "resized $Resize" }
    $n = 0
    for ($c = 0; $c -lt $Cycles * 8; $c++) {
        switch ($c % 8) {
            0 { Send-Text $app 'hello world' }
            1 { Send-Chord $app 'ctrl+z' 20 }
            2 { Send-Chord $app 'ctrl+f' 20; Send-Text $app 'klm'; Send-Chord $app 'enter' 20; Send-Chord $app 'esc' 20 }
            3 { Send-Chord $app 'ctrl+tab' 20 }
            4 { Send-Chord $app 'pgdn' 20 }
            5 { Send-Chord $app 'ctrl+s' 150 }
            6 { Send-Chord $app 'ctrl+p' 20; Send-Chord $app 'esc' 20 }
            7 { Send-Chord $app 'down' 20; Send-Chord $app 'enter' 20 }
        }
        if ($c % 80 -eq 79) { Mem $app "cycle $([int](($c+1)/8))" }
    }
    Mem $app 'after cycles'
    Start-Sleep -Seconds 40
    Mem $app 'after idle 40s'
    if ($DhatOut) { New-Item -ItemType File $env:BROUILLON_DHAT_TRIGGER | Out-Null; $w = 0; while (-not (Test-Path $DhatOut) -and $w -lt 180) { Start-Sleep -Seconds 1; $w++ }; Start-Sleep -Seconds 3 }
    try { Stop-App $app } catch { }
}
