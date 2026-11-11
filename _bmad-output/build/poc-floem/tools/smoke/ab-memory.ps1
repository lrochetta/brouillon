<#
.SYNOPSIS
  A/B memory comparison of two builds (e.g. default mimalloc vs --no-default-features system heap, or
  idle_trim on vs off) using the existing on-screen probes. Drives the GUI through leak.ps1 (same safety
  contract: focus guard, temporary settings dir, process always killed).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\ab-memory.ps1 -ExeA A.exe -ExeB B.exe [-Seconds 90] [-IdleSeconds 60]
  Per exe it prints: leak.ps1 mode=idle (startup + idle: private/working set), mode=search and mode=typing
  (transient buffers: find, edits). Compare "private" first (real footprint), then ws (what Task Manager shows).
  First-frame time: run each exe once with BROUILLON_TRACE_STARTUP=1 and read "[metrics] cold start: N ms" on stderr.
#>
param([Parameter(Mandatory)][string]$ExeA, [Parameter(Mandatory)][string]$ExeB, [int]$Seconds = 90, [int]$SizeMb = 20)
$ErrorActionPreference = 'Stop'
foreach ($exe in @($ExeA, $ExeB)) {
    Write-Host "=== $exe ===" -ForegroundColor Cyan
    foreach ($mode in 'idle', 'search', 'typing') {
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'leak.ps1') -Exe $exe -Mode $mode -Seconds $Seconds -SizeMb $SizeMb
    }
}
