<#
.SYNOPSIS
  Brouillon Link with the REAL Claude Code CLI. The editor starts with --mcp (session only), the
  relay command is registered with `claude mcp add` (local scope, in a scratch dir, removed
  afterwards), `claude -p` calls editor_state, the consent question is answered on screen with Enter
  (= read only), and the time from launching the editor to Claude's answer is reported.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\mcp-claude.ps1 -Exe C:\path\brouillon.exe [-Tag x-]
  Same safety contract as run.ps1 (keys only while our window is the foreground one; TEMPORARY --settings-dir).
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-mcp-claude'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
$relay = Join-Path (Split-Path $Exe) 'brouillon-mcp.exe'
if (-not (Test-Path $relay)) { throw "relay not found next to the exe: $relay" }

Invoke-Scenario 'claude' 'Real Claude Code reads editor_state through the relay' {
    $d = New-TestEnv $DataRoot 'cc'
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`nbeta`ngamma`n", (New-Object Text.UTF8Encoding($false)))
    $script:T0 = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Extra @('--new-window', '--mcp') -Tag 'claude'
    $script:Deadline = (Get-Date).AddSeconds(200)
    Shot $app 'cc-0-start'
    $link = "$d\settings\link"
    for ($i = 0; $i -lt 100 -and -not (Test-Path "$link\link.token"); $i++) { Start-Sleep -Milliseconds 100 }
    if (-not (Test-Path "$link\link.token")) { throw 'no link token: --mcp did not start the server' }
    $work = Join-Path $DataRoot 'cc-work'; New-Item -ItemType Directory -Force $work | Out-Null
    $name = 'brouillon-test'
    $claude = (Get-Command claude).Source
    Push-Location $work
    try {
        & $claude mcp add $name -- $relay --client claude-code --link-dir $link 2>&1 | Write-Host
        $prompt = 'Call the MCP tool editor_state of the server "' + $name + '" exactly once and answer with the raw JSON it returns, nothing else.'
        $answered = $false; $outText = ''; $errText = ''
        for ($attempt = 1; $attempt -le 2; $attempt++) {
            $psi = New-Object Diagnostics.ProcessStartInfo
            $psi.FileName = $claude
            $psi.Arguments = '-p "' + $prompt.Replace('"', '\"') + '" --allowedTools "mcp__' + $name + '__editor_state" --model haiku --output-format text'
            $psi.WorkingDirectory = $work
            $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
            $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
            $cc = [Diagnostics.Process]::Start($psi)
            $outTask = $cc.StandardOutput.ReadToEndAsync(); $errTask = $cc.StandardError.ReadToEndAsync()
            $sw = [Diagnostics.Stopwatch]::StartNew()
            while (-not $cc.HasExited -and $sw.Elapsed.TotalSeconds -lt 180) {
                Test-Deadline
                if (-not $answered -and (Test-Path "$link/audit.jsonl") -and ((Get-Content -Raw "$link/audit.jsonl") -match 'CONSENT_REQUIRED')) {
                    Start-Sleep -Milliseconds 800; Shot $app 'cc-1-consent'
                    Set-AppFocus $app
                    Send-Chord $app 'enter' 700; $answered = $true
                    Shot $app 'cc-2-after-enter'
                }
                Start-Sleep -Milliseconds 300
            }
            if (-not $cc.HasExited) { $cc.Kill(); throw 'claude did not finish within 180 s' }
            $outText = $outTask.Result; $errText = $errTask.Result
            if ($outText -notmatch 'CONSENT_REQUIRED') { break }   # first call only raises the question; the second one reads
        }
        "claude exit=$($cc.ExitCode) after attempt $attempt; consent answered=$answered"
        "--- claude stdout (first 500 chars):"; $outText.Substring(0, [math]::Min(500, $outText.Length)) | Write-Host
        if ($errText) { "--- claude stderr (first 300):"; $errText.Substring(0, [math]::Min(300, $errText.Length)) | Write-Host }
        Shot $app 'cc-3-end'
        $secs = [math]::Round(([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() - $script:T0) / 1000.0, 1)
        "claude exit=$($cc.ExitCode) after $secs s since launch; consent answered=$answered"
        "--- claude stdout (first 500 chars):"; $outText.Substring(0, [math]::Min(500, $outText.Length)) | Write-Host
        if ($errText) { "--- claude stderr (first 300):"; $errText.Substring(0, [math]::Min(300, $errText.Length)) | Write-Host }
        Shot $app 'cc-3-end'
        if ($outText -notmatch 'revision|doc|tabs|version|a\.txt') { throw 'claude output does not look like editor_state' }
        "REAL CLAUDE CODE OK: launch -> answer $secs s"
    } finally {
        try { & $claude mcp remove $name 2>&1 | Write-Host } catch {}
        Pop-Location
    }
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
if (@($script:Results | Where-Object { $_.Status -ne 'OK' }).Count -eq 0) { Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue } else { Write-Host "kept for inspection: $DataRoot" }
