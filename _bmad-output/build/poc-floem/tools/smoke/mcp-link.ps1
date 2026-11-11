<#
.SYNOPSIS
  Brouillon Link end to end: a REAL stdio MCP client (tools/smoke/mcp-client.mjs, Node) drives the
  relay binary against the RUNNING editor. Preferences > Connect > turn on, copy the snippet, the
  client runs the command from the clipboard, the consent modal is answered on screen, an edit
  arrives as a proposal, is accepted on screen, the text changes, one Ctrl+Z restores it.
  Measures the time from "turn it on in Preferences" to the first successful tools/call.

  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\mcp-link.ps1 -Exe C:\path\brouillon.exe [-Tag x-]

  The relay (brouillon-mcp.exe) must sit next to brouillon.exe. Same safety contract as run.ps1: input only
  while our window is the foreground one; TEMPORARY --settings-dir (so its own link folder, token and pipe);
  BROUILLON_NO_UPDATE=1; the process is always killed. Captures land in tools\smoke\out\.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-mcp-link'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Events($sync) { $f = "$sync\events.jsonl"; if (Test-Path $f) { @(Get-Content $f | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json }) } else { @() } }
function Wait-Event($sync, [string]$ev, [int]$Ms = 20000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $Ms) {
        Test-Deadline
        if (Test-Path "$sync\error.flag") { throw "client error: $(Get-Content -Raw "$sync\error.flag")" }
        $e = Events $sync | Where-Object { $_.ev -eq $ev } | Select-Object -First 1
        if ($e) { return $e }
        Start-Sleep -Milliseconds 100
    }
    throw "timeout waiting for client event '$ev'"
}

# LOGICAL px from the window's outer top-left (see lib.ps1 Send-Click).
$P = @{
    Connect   = @(892, 120)    # "Connect" tab of Preferences
    Switch    = @(188, 172)    # the "Agents (MCP server)" on/off button on the Connect tab
    Copy      = @(222, 309)    # "Copy snippet"
    Allow     = @(434, 239)    # consent modal: "Allow (read + propose edits)"
    AcceptAll = @(640, 690)    # proposal panel: "Accept all"
    TestBtn   = @(301, 309)
}

Invoke-Scenario 'link' 'Agents: enable, snippet, consent, proposal, accept, undo' {
    $d = New-TestEnv $DataRoot 'link'
    $sync = "$d\sync"; New-Item -ItemType Directory -Force $sync | Out-Null
    Write-Utf8 "$d\files\a.txt" "alpha`nbeta`ngamma`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'link'
    $script:Deadline = (Get-Date).AddSeconds(150)   # many steps and a real client: more than the usual 60 s
    Shot $app 'mcp-0-start'
    # Nothing listens before the user turns it on.
    if (Test-Path "$d\settings\link") { throw 'link folder exists before Agents were enabled' }

    Send-Chord $app 'ctrl+comma' 600
    Send-Click $app $P.Connect[0] $P.Connect[1]
    Shot $app 'mcp-1-connect-tab'
    $t0 = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()      # the user turns Agents on
    Send-Click $app $P.Switch[0] $P.Switch[1]
    Start-Sleep -Milliseconds 500
    Shot $app 'mcp-2-enabled'
    $toml = [IO.File]::ReadAllText("$d\settings\settings.toml")
    if ($toml -notmatch 'enabled = true') { throw "settings.toml has no mcp enabled = true:`n$toml" }
    if (-not (Test-Path "$d\settings\link\link.token")) { throw 'no link token after enabling' }

    # Copy the snippet; the clipboard text is what the user would paste into a terminal.
    Send-Click $app $P.Copy[0] $P.Copy[1]
    Start-Sleep -Milliseconds 300
    $snippet = (Get-Clipboard -Raw).Trim()
    if ($snippet -notmatch '^claude mcp add brouillon -- ') { throw "clipboard has no snippet: [$snippet]" }
    "snippet: $snippet" | Write-Host
    # The part after `--` is the command a client launches over stdio.
    $cmdline = $snippet.Substring($snippet.IndexOf('-- ') + 3)
    $parts = [regex]::Matches($cmdline, '"([^"]*)"|(\S+)') | ForEach-Object { if ($_.Groups[1].Success) { $_.Groups[1].Value } else { $_.Groups[2].Value } }
    $relay = $parts[0]; $relayArgs = @($parts | Select-Object -Skip 1)
    if (-not (Test-Path $relay)) { throw "relay from the snippet does not exist: $relay" }

    # Test button: the editor connects to itself like a client would.
    Send-Click $app $P.TestBtn[0] $P.TestBtn[1]
    Start-Sleep -Milliseconds 900
    Shot $app 'mcp-3-test'
    Send-Chord $app 'esc' 400

    # Start the real client.
    $node = (Get-Command node).Source
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = $node
    $psi.Arguments = '"' + (Join-Path $PSScriptRoot 'mcp-client.mjs') + '" "' + $sync + '" "' + $relay + '" ' + (($relayArgs | ForEach-Object { '"' + $_ + '"' }) -join ' ')
    $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
    $client = [Diagnostics.Process]::Start($psi)
    try {
        [void](Wait-Event $sync 'initialize' 90000)
        $tl = Wait-Event $sync 'tools_list'
        if ($tl.count -ne 10) { throw "tools/list returned $($tl.count) tools" }
        [void](Wait-Event $sync 'editor_state_refused')
        Start-Sleep -Milliseconds 800
        Shot $app 'mcp-4-consent'
        Set-AppFocus $app
        Send-Click $app $P.Allow[0] $P.Allow[1]
        Start-Sleep -Milliseconds 700; Shot $app 'mcp-4b-after-click'
        $ok = Wait-Event $sync 'first_call_ok'
        $secs = [math]::Round(($ok.t - $t0) / 1000.0, 1)
        "enable -> first successful tools/call: $secs s" | Write-Host
        Shot $app 'mcp-5-after-consent'
        [void](Wait-Event $sync 'read_before')
        $prop = Wait-Event $sync 'edit_proposed'
        if ($prop.isError) { throw "edit refused: $($prop.result | ConvertTo-Json -Compress)" }
        Start-Sleep -Milliseconds 700
        Shot $app 'mcp-6-proposal'
        Set-AppFocus $app
        Send-Click $app $P.AcceptAll[0] $P.AcceptAll[1]
        $res = Wait-Event $sync 'proposal_result'
        if ($res.result.status -ne 'accepted') { throw "proposal status: $($res.result | ConvertTo-Json -Compress)" }
        $after = Wait-Event $sync 'read_after_accept'
        if ($after.text -notmatch 'ALPHA' -or $after.text -notmatch 'GAMMA') { throw "text did not change: $($after.text)" }
        Shot $app 'mcp-7-accepted'
        [void](Wait-Event $sync 'waiting_for_undo')
        Set-AppFocus $app
        Send-Chord $app 'ctrl+z' 500
        Shot $app 'mcp-8-undone'
        Set-Content -Path "$sync\undone.flag" -Value 'x'
        $undo = Wait-Event $sync 'read_after_undo'
        $before = (Events $sync | Where-Object { $_.ev -eq 'read_before' } | Select-Object -First 1).text
        if ($undo.text -ne $before) { throw "undo did not restore the text in ONE step:`nbefore: $before`nafter:  $($undo.text)" }
        [void](Wait-Event $sync 'done')
        # Kill switch (Ctrl+Alt+K): the chip goes away and the instance file is removed.
        $chipBefore = Get-RegionHash $app -Logical @(600, 28, 500, 24)
        Set-AppFocus $app
        Send-Chord $app 'ctrl+alt+k' 700
        Shot $app 'mcp-9-killed'
        $chipAfter = Get-RegionHash $app -Logical @(600, 28, 500, 24)
        if ($chipBefore -eq $chipAfter) { throw 'kill switch: the header chip did not change' }
        if (@(Get-ChildItem "$d\settings\link\instances" -ErrorAction SilentlyContinue).Count -ne 0) { throw 'kill switch: instance file still present' }
        $audit = if (Test-Path "$d\settings\link\audit.jsonl") { (Get-Content "$d\settings\link\audit.jsonl").Count } else { 0 }
        "enable->first call $secs s; proposal accepted on screen, text changed, ONE Ctrl+Z restored it; audit lines: $audit"
    } finally {
        try { if (-not $client.HasExited) { $client.Kill() } } catch {}
    }
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
if (@($script:Results | Where-Object { $_.Status -ne 'OK' }).Count -eq 0) { Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue } else { Write-Host "kept for inspection: $DataRoot" }
