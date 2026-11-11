<#
.SYNOPSIS
  Release gate: fails when an exe contains any marker of the DEV-ONLY test-input channel.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\check-no-test-input.ps1 -Exe C:\path\brouillon.exe
#>
param([Parameter(Mandatory)][string]$Exe)
$bytes = [IO.File]::ReadAllBytes((Resolve-Path $Exe).Path)
$latin = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
$markers = @('BROUILLON_TEST_INPUT', 'brouillon-test-input', '[test-input]', 'GUI thread did not answer')
$hits = @($markers | Where-Object { $latin.Contains($_) })
if ($hits.Count -gt 0) { Write-Host "FAIL: test-input markers present in $Exe : $($hits -join ', ')"; exit 1 }
Write-Host "OK: no test-input marker in $Exe ($([math]::Round($bytes.Length / 1MB, 1)) MB checked)"
