<#
.SYNOPSIS
  Credential vault UI, driven through the TEST-INPUT channel (no SendInput, no foreground, TEMPORARY --settings-dir):
  a saved demo site connects and ASKS for a password (dev hook BROUILLON_REMOTE_FAKE_ASK_PASSWORD=1, which also lets a
  --settings-dir run keep passwords), the "Sign in" card (masked field), the "Keep this password?" card ("Save on this
  computer"), the proof that the secret is neither in sites.toml nor in clear in vault.bsv, 'Credentials: Show' (a row
  with metadata, never the value), Delete + its confirmation, then a second connect that must ask again.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\vault-ui.ps1 -Exe D:\travail\_t-verifyhv\debug\brouillon.exe [-Tag x-]
  Needs a build with --features test-input. Screenshots: tools\smoke\out\. Pixel clicks assume a 1.75 scale window
  (the coordinates are screenshot pixels / 1.75).
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-vault-ui'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
$env:BROUILLON_REMOTE_DEBUG = '1'
$env:BROUILLON_REMOTE_FAKE_ASK_PASSWORD = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" 400 | Out-Null }
function Px($app, [double]$x, [double]$y) {
    [void](Invoke-TI $app @{ op = 'click'; x = ($x / 1.75); y = ($y / 1.75) })
    Start-Sleep -Milliseconds 300
}
function Palette($app, [string]$cmd) {
    Send-Chord $app 'ctrl+k' 250
    $null = Wait-State $app { param($s) $s.state.palette_open }
    Send-Text $app $cmd
    Start-Sleep -Milliseconds 250
    Send-Chord $app 'enter' 600
}
function Dbg($app, [string]$pat) { @((Get-State $app 150).lines | Where-Object { $_ -match $pat }) }

Invoke-Scenario 'vault-ui' 'Vault UI: ask, keep, credentials, delete, ask again' {
    $d = New-TestEnv $DataRoot 'v1'
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\local.txt") -Tag 'v1'
    $script:Deadline = (Get-Date).AddSeconds(150)   # a long scenario: Launch-Brouillon resets the 60 s budget
    # --- 1. connect to a remembered site: the demo server asks for a password
    Palette $app 'Remote: Connect...'
    Send-Text $app 'me@demo'
    Send-Chord $app 'enter' 1200
    if (-not (Dbg $app 'ask_secret=true')) { throw 'the Sign in card did not open' }
    Shot $app 'vault-1-sign-in'
    Send-Text $app 'hunter2'
    Shot $app 'vault-2-typed-masked'
    Send-Chord $app 'enter' 1000
    Start-Sleep -Milliseconds 2500
    Shot $app 'vault-3-keep-card'
    # --- 2. "Save on this computer", Continue
    Px $app 745 238
    Shot $app 'vault-4-save-selected'
    Px $app 1112 337
    Start-Sleep -Milliseconds 800
    $toml = Get-Content -Raw "$d\settings\sites.toml"
    if ($toml -notmatch 'keep_asked = true') { throw 'sites.toml: keep_asked was not recorded' }
    if ($toml -match 'hunter2') { throw 'the password is in sites.toml' }
    if (-not (Test-Path "$d\settings\vault.bsv")) { throw 'no vault file after Save on this computer' }
    if ([Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes("$d\settings\vault.bsv")) -match 'hunter2') { throw 'the password is in clear in the vault file' }
    Shot $app 'vault-5-kept'
    # --- 3. Credentials: Show
    Palette $app 'Credentials: Show'
    Start-Sleep -Milliseconds 600
    Shot $app 'vault-6-credentials'
    # --- 4. disconnect, connect again: the saved password answers silently (no Sign in card)
    Send-Chord $app 'escape' 400
    $asks = @(Dbg $app 'ask_secret=true').Count
    Palette $app 'Remote: Disconnect'
    Palette $app 'Remote: Connect...'
    Send-Text $app 'me@demo'
    Send-Chord $app 'enter' 1500
    Shot $app 'vault-7-reconnect-silent'
    if (@(Dbg $app 'ask_secret=true').Count -ne $asks) { throw 'a saved password was asked for again' }
    if (-not (Dbg $app 'demo:Connected\*' | Select-Object -Last 1)) { throw 'the reconnect with the saved password did not connect' }
    # --- 5. Delete from Credentials: confirmation card, then the list is empty
    Palette $app 'Credentials: Show'
    Start-Sleep -Milliseconds 500
    Px $app 1366 207
    Shot $app 'vault-8-delete-confirm'
    Px $app 1366 641
    Shot $app 'vault-9-deleted'
    Send-Chord $app 'escape' 400
    # --- 6. after Delete the site asks again; keep it again, then Forget from Manage Sites
    Palette $app 'Remote: Disconnect'
    Palette $app 'Remote: Connect...'
    Send-Text $app 'me@demo'
    Send-Chord $app 'enter' 1500
    if (@(Dbg $app 'ask_secret=true').Count -le $asks) { throw 'after Delete the password was not asked again' }
    Send-Text $app 'again'
    Send-Chord $app 'enter' 1000
    Start-Sleep -Milliseconds 2500
    Px $app 745 238
    Px $app 1112 337
    Start-Sleep -Milliseconds 800
    Palette $app 'Remote: Manage Sites...'
    Start-Sleep -Milliseconds 600
    Shot $app 'vault-10-sites-with-forget'
    if ((Get-Content -Raw "$d\settings\sites.toml") -notmatch 'secret_ref') { throw 'no secret_ref in sites.toml after the second keep' }
    Px $app 1150 158
    Start-Sleep -Milliseconds 600
    Shot $app 'vault-11-forgotten'
    if ((Get-Content -Raw "$d\settings\sites.toml") -match 'secret_ref') { throw 'Forget this password left a secret_ref in sites.toml' }
    Send-Chord $app 'escape' 400
    Palette $app 'Credentials: Show'
    Start-Sleep -Milliseconds 500
    Shot $app 'vault-12-credentials-empty'
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
