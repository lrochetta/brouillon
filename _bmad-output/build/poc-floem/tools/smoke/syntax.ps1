<#
.SYNOPSIS
  Syntax colouring on screen: opens each sample file of -SampleDir in its own window and saves syn-<name>.png.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\syntax.ps1 -Exe C:\path\brouillon.exe -SampleDir C:\samples [-Only s.c,s.sql]
  Same safety contract as run.ps1 (no keys are sent at all; TEMPORARY --settings-dir; process killed).
#>
param([string]$Exe, [string]$SampleDir, [string[]]$Only = @(), [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-syn'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
$Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot $(if ($env:BROUILLON_SMOKE_TI -eq '1') { 'testinput.ps1' } else { 'lib.ps1' }))
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$files = Get-ChildItem $SampleDir -File | Where-Object { $Only.Count -eq 0 -or $Only -contains $_.Name }
foreach ($f in $files) {
    Invoke-Scenario "syn-$($f.Name)" "colouring of $($f.Name)" {
        $d = New-TestEnv $DataRoot 'x'
        Copy-Item $f.FullName "$d\files\$($f.Name)"
        $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\$($f.Name)") -Tag 'syn'
        Save-Shot $app "${Tag}syn-$($f.Name)" 700 | Out-Null
        'shot'
    }
}
