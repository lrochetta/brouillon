<#
.SYNOPSIS
  Find in Files must find a hit inside a big (> 10 MB) opened file instead of "0 hit(s) in 0 file(s)".
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\find-in-files-big.ps1 -Exe C:\path\brouillon.exe [-BigMB 100] [-Tag after-]
  Same safety contract as run.ps1 (keys only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Screenshots in tools\smoke\out\.
#>
param([string]$Exe, [int]$BigMB = 100, [string]$DataRoot = 'D:\travail\_perfD\fif', [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)

Invoke-Scenario 'fif-big' "Find in Files on a $BigMB MB file" {
    $d = New-TestEnv $DataRoot 'big'
    $big = "$d\files\big.txt"
    $sw = New-Object IO.StreamWriter($big, $false, $u, 1MB)
    $lineText = ' the quick brown fox jumps over the lazy dog, lorem ipsum dolor sit amet consectetur adipiscing elit'
    $n = [int]($BigMB * 1MB / 100)
    for ($i = 0; $i -lt $n; $i++) { $sw.Write(('{0:D7}' -f $i)); $sw.Write($lineText); $sw.Write("`n") }
    $sw.Write("ZZUNIQUEZZ last line`n"); $sw.Close()
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($big) -Tag 'fif-big'
    Start-Sleep -Seconds 10
    $script:Deadline = (Get-Date).AddSeconds(200)
    Set-AppFocus $app
    Send-Chord $app 'ctrl+f' 800
    Send-Text $app 'ZZUNIQUEZZ' 40
    Start-Sleep -Seconds 2
    Send-Chord $app 'esc' 300
    Set-AppFocus $app
    $t0 = Get-Date
    Send-Chord $app 'ctrl+shift+f' 100
    Start-Sleep -Seconds 6
    Save-Shot $app "${Tag}fif-big-1-results" | Out-Null
    "Find in Files issued; screenshot taken $([int]((Get-Date) - $t0).TotalSeconds) s later"
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
