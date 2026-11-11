<#
.SYNOPSIS
  Memory-growth probe: drives one mode for -Seconds and samples working set / private bytes every -SampleSec.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\leak.ps1 -Exe C:\path\brouillon.exe -Mode typing|search|tabs|pgdn|idle [-Seconds 120] [-SizeMb 2] [-GapMs 500]
  Output: one line per sample and the slope (MB/min) over the second half of the run.
  Same safety contract as run.ps1 (focus guard, temporary settings dir, process always killed).
#>
param([string]$Exe, [string]$Mode = 'typing', [int]$Seconds = 120, [int]$SizeMb = 2, [int]$GapMs = 500, [int]$SampleSec = 10, [string]$Ext = 'txt', [string]$DhatOut = '', [string]$Root = 'D:\travail\_leakroot')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
if ($DhatOut) { $env:BROUILLON_DHAT_OUT = $DhatOut; $env:BROUILLON_DHAT_TRIGGER = ($DhatOut + '.trigger'); Remove-Item $DhatOut, $env:BROUILLON_DHAT_TRIGGER -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force $Root | Out-Null
$big = Join-Path $Root "f$SizeMb.$Ext"; $small = Join-Path $Root 'second.txt'
$line = if ($Ext -eq 'rs') { 'fn function_name(arg: u32) -> u32 { let value = arg + 1; value * 2 } // trailing comment text' + "`n" } else { ('abcdefghij klmnopqrst uvwxyz 0123456789 ' * 3).Substring(0, 98) + "`n" }
[IO.File]::WriteAllText($big, ($line * [int]($SizeMb * 1MB / 99)))
[IO.File]::WriteAllText($small, ("second file`n" * 200))
$samples = New-Object System.Collections.ArrayList
Invoke-Scenario "leak-$Mode" "leak probe $Mode" {
    $d = New-TestEnv $Root "env-$Mode"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($big, $small) -Tag "leak-$Mode"
    Start-Sleep -Milliseconds 3000
    $t0 = Get-Date; $next = 0; $n = 0
    $script:Deadline = (Get-Date).AddSeconds($Seconds + 120)
    while (((Get-Date) - $t0).TotalSeconds -lt $Seconds) {
        $el = ((Get-Date) - $t0).TotalSeconds
        if ($el -ge $next) {
            $app.Proc.Refresh()
            [void]$samples.Add([pscustomobject]@{ T = [int]$el; WS = [math]::Round($app.Proc.WorkingSet64 / 1MB, 1); Priv = [math]::Round($app.Proc.PrivateMemorySize64 / 1MB, 1) })
            Write-Host ("t={0,4}s ws={1,7} priv={2,7} ops={3}" -f [int]$el, $samples[-1].WS, $samples[-1].Priv, $n)
            $next += $SampleSec
        }
        Assert-Focus $app
        switch ($Mode) {
            'typing' { Send-Text $app 'a' 0; $n++; if ($n % 25 -eq 0) { Send-Chord $app 'ctrl+z' 0 } }
            'search' { Send-Chord $app 'ctrl+f' 0; Send-Text $app 'klm' 0; Send-Vk 0x0D $false; Send-Vk 0x0D $true; Send-Vk 0x1B $false; Send-Vk 0x1B $true; $n++ }
            'findopen' { Send-Chord $app 'ctrl+f' 0; Send-Vk 0x1B $false; Send-Vk 0x1B $true; $n++ }
            'findnext' { if ($n -eq 0) { Send-Chord $app 'ctrl+f' 0; Send-Text $app 'klm' 0 }; Send-Vk 0x72 $false; Send-Vk 0x72 $true; $n++ }
            'findtype' { if ($n -eq 0) { Send-Chord $app 'ctrl+f' 0 }; Send-Text $app 'k' 0; Send-Vk 0x08 $false; Send-Vk 0x08 $true; $n++ }
            'enter'  { Send-Vk 0x0D $false; Send-Vk 0x0D $true; $n++ }
            'type3'  { Send-Text $app 'klm' 0; $n++ }
            'type3a' { Send-Text $app 'aaa' 0; $n++ }
            'tabs'   { Send-Chord $app 'ctrl+tab' 0; $n++ }
            'pgdn'   { Send-Vk 0x22 $false; Send-Vk 0x22 $true; $n++ }
            'mixed'  { switch ($n % 8) { 0 { Send-Text $app 'hello world' 0 } 1 { Send-Chord $app 'ctrl+z' 0 } 2 { Send-Chord $app 'ctrl+f' 0; Send-Text $app 'klm' 0; Send-Vk 0x0D $false; Send-Vk 0x0D $true; Send-Vk 0x1B $false; Send-Vk 0x1B $true } 3 { Send-Chord $app 'ctrl+tab' 0 } 4 { Send-Vk 0x22 $false; Send-Vk 0x22 $true } 5 { Send-Chord $app 'ctrl+s' 0 } 6 { Send-Chord $app 'ctrl+p' 0; Send-Vk 0x1B $false; Send-Vk 0x1B $true } 7 { Send-Vk 0x28 $false; Send-Vk 0x28 $true; Send-Vk 0x0D $false; Send-Vk 0x0D $true } }; $n++ }
            default  { }
        }
        Start-Sleep -Milliseconds $GapMs
    }
    if ($DhatOut) { New-Item -ItemType File $env:BROUILLON_DHAT_TRIGGER | Out-Null; $w = 0; while (-not (Test-Path $DhatOut) -and $w -lt 120) { Start-Sleep -Seconds 1; $w++ }; Start-Sleep -Seconds 3 }
    $app.Proc.Refresh()
    if ($app.Proc.HasExited) { throw 'CRASH: the process exited during the run' }
    $h = [int]($samples.Count / 2)
    $a = $samples[$h]; $b = $samples[-1]
    $slope = ($b.Priv - $a.Priv) / (([double]($b.T - $a.T)) / 60.0)
    "mode=$Mode ops=$n private $($a.Priv) -> $($b.Priv) MB, slope(2nd half)=$([math]::Round($slope,1)) MB/min; ws slope=$([math]::Round(($b.WS-$a.WS)/(([double]($b.T-$a.T))/60.0),1))"
}
Remove-Item -Recurse -Force $Root -ErrorAction SilentlyContinue
