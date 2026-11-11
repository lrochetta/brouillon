<#
.SYNOPSIS
  Find bar cost: CPU of the process with the bar open (empty / with a query) and search on a big file.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\find-perf.ps1 -Exe C:\path\brouillon.exe [-BigMB 100] [-Tag before-]
  Same safety contract as run.ps1 (keys only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). The big file is generated
  under -DataRoot (default D:\travail\_perfD) and removed at the end.
  Reported: CPU as % of ONE core over a window, UI latency (SendMessageTimeout WM_NULL round trip,
  max over the window) while a search runs, and screenshots in tools\smoke\out\.
#>
param([string]$Exe, [int]$BigMB = 100, [string]$DataRoot = 'D:\travail\_perfD\run', [string]$Tag = '', [int]$Reps = 3, [switch]$SkipBig)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class Ping {
  [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr SendMessageTimeout(IntPtr h, uint msg, IntPtr w, IntPtr l, uint flags, uint timeout, out IntPtr result);
  // Round trip (ms) of a WM_NULL to the window's thread; 5000 = it did not answer.
  public static double RoundTrip(IntPtr h) {
    var sw = System.Diagnostics.Stopwatch.StartNew(); IntPtr r;
    SendMessageTimeout(h, 0, IntPtr.Zero, IntPtr.Zero, 2, 5000, out r);
    return sw.Elapsed.TotalMilliseconds;
  }
}
"@
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }

# CPU of the process over $Sec seconds, in % of one core.
function Measure-Cpu($app, [double]$Sec) {
    $app.Proc.Refresh(); $c0 = $app.Proc.TotalProcessorTime.TotalMilliseconds; $t0 = Get-Date
    Start-Sleep -Milliseconds ([int]($Sec * 1000))
    $app.Proc.Refresh(); $c1 = $app.Proc.TotalProcessorTime.TotalMilliseconds
    [math]::Round(100.0 * ($c1 - $c0) / ((Get-Date) - $t0).TotalMilliseconds, 1)
}
# Max UI round trip (ms) polled every ~20 ms for $Sec seconds; also the count of probes.
function Measure-Ui($app, [double]$Sec) {
    $max = 0.0; $n = 0; $t0 = Get-Date
    while (((Get-Date) - $t0).TotalSeconds -lt $Sec) {
        $v = [Ping]::RoundTrip($app.Hwnd); if ($v -gt $max) { $max = $v }; $n++
        Start-Sleep -Milliseconds 20
    }
    [pscustomobject]@{ MaxMs = [math]::Round($max, 0); Probes = $n }
}

Invoke-Scenario 'cpu' 'CPU with the Find bar open (small file)' {
    $d = New-TestEnv $DataRoot 'small'
    $lines = (1..200 | ForEach-Object { "line $_ foo bar baz Foo" }) -join "`n"
    [IO.File]::WriteAllText("$d\files\small.txt", $lines + "`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\small.txt") -Tag 'fp-small'
    Start-Sleep -Seconds 9                                  # first-seconds working-set / startup transient
    $script:Deadline = (Get-Date).AddSeconds(240)
    $res = @()
    $idle = @(1..$Reps | ForEach-Object { Measure-Cpu $app 3 })
    Set-AppFocus $app
    Send-Chord $app 'ctrl+f' 600
    Start-Sleep -Seconds 2
    $empty = @(1..$Reps | ForEach-Object { Measure-Cpu $app 3 })
    Shot $app 'fp-small-1-bar-empty'
    Send-Text $app 'foo'
    Start-Sleep -Seconds 2
    $q = @(1..$Reps | ForEach-Object { Measure-Cpu $app 3 })
    Shot $app 'fp-small-2-query'
    # per-thread CPU over 3 s with the query present
    $app.Proc.Refresh(); $a = @{}; foreach ($t in $app.Proc.Threads) { $a[$t.Id] = $t.TotalProcessorTime.TotalMilliseconds }
    Start-Sleep -Seconds 3
    $app.Proc.Refresh(); $top = @()
    foreach ($t in $app.Proc.Threads) { $dl = $t.TotalProcessorTime.TotalMilliseconds - $(if ($a.ContainsKey($t.Id)) { $a[$t.Id] } else { 0 }); if ($dl -gt 30) { $top += "tid $($t.Id): $([math]::Round($dl / 30, 1))%" } }
    Send-Chord $app 'esc' 500
    Start-Sleep -Seconds 2
    $closed = @(1..$Reps | ForEach-Object { Measure-Cpu $app 3 })
    "CPU % of one core, n=$Reps each: idle [$($idle -join ' ')]; bar open empty [$($empty -join ' ')]; bar open with query [$($q -join ' ')]; bar closed again [$($closed -join ' ')]; busy threads with query: $($top -join ', ')"
}

if (-not $SkipBig) {
    Invoke-Scenario 'big' "Search on a $BigMB MB file" {
        $d = New-TestEnv $DataRoot 'big'
        $big = "$d\files\big.txt"
        $sw = New-Object IO.StreamWriter($big, $false, $u, 1MB)
        $lineText = ' the quick brown fox jumps over the lazy dog, lorem ipsum dolor sit amet consectetur adipiscing elit'
        $n = [int]($BigMB * 1MB / 100)
        for ($i = 0; $i -lt $n; $i++) { $sw.Write(('{0:D7}' -f $i)); $sw.Write($lineText); $sw.Write("`n") }
        $sw.Write("ZZUNIQUEZZ last line`n"); $sw.Close()
        $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($big) -Tag 'fp-big'
        Start-Sleep -Seconds 10
        $script:Deadline = (Get-Date).AddSeconds(400)
        Shot $app 'fp-big-0-open'
        $row = @()
        Set-AppFocus $app
        Send-Chord $app 'ctrl+f' 800
        # common query typed char by char: UI latency while the search runs
        $t0 = Get-Date
        Send-Text $app 'lazy dog' 40
        $typeMs = [int]((Get-Date) - $t0).TotalMilliseconds
        $ui1 = Measure-Ui $app 6
        Shot $app 'fp-big-1-common-query'
        $cpu1 = Measure-Cpu $app 3
        # replace by a rare query: last line of the file
        Send-Chord $app 'ctrl+a' 100
        Send-Text $app 'ZZUNIQUEZZ' 40
        $ui2 = Measure-Ui $app 6
        Shot $app 'fp-big-2-rare-query'
        $cpu2 = Measure-Cpu $app 3
        $t1 = Get-Date
        Send-Chord $app 'enter' 100
        $ui3 = Measure-Ui $app 4
        Shot $app 'fp-big-3-next'
        # Find in Files over the folder holding the big file (the bar keeps the query)
        Send-Chord $app 'esc' 300
        Set-AppFocus $app
        Send-Chord $app 'ctrl+shift+f' 100
        $ui4 = Measure-Ui $app 10
        Shot $app 'fp-big-4-find-in-files'
        $cpu4 = Measure-Cpu $app 3
        "typing 'lazy dog' took ${typeMs} ms; UI max round trip over 6 s after it: $($ui1.MaxMs) ms ($($ui1.Probes) probes); CPU then $cpu1 %; rare query UI max $($ui2.MaxMs) ms, CPU $cpu2 %; Enter (next) UI max $($ui3.MaxMs) ms; Find in Files UI max $($ui4.MaxMs) ms ($($ui4.Probes) probes), CPU after $cpu4 %"
    }
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
