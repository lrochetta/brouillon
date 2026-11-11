<#
.SYNOPSIS
  Auto-update smoke scenarios (u1 clean document, u2 modified document, u3 mandatory update).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\update.ps1 -ExeA <build N> -ExeB <build N+1> -BuildA 900 -BuildB 901 [-Only u1,u2,u3]

  SAFETY: only a harmless TEST installer is ever run (built here with rustc: it writes a witness file and
  copies ExeB over the app copy, like a real install would). The server is 127.0.0.1 with a TEST signing key.
  The real site is never contacted: updates.url is only honoured with BROUILLON_UPDATE_DEV=1 (set here, for the
  launched process only). Keystrokes are sent only while our window is the foreground one (lib.ps1 focus guard).
#>
param([string]$ExeA, [string]$ExeB, [int]$BuildA = 900, [int]$BuildB = 901, [string[]]$Only, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-update-smoke'), [string]$Tag = 'upd-')
$ErrorActionPreference = 'Stop'
if ($Only) { $Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ }) }
. (Join-Path $PSScriptRoot 'lib.ps1')
$ExeA = (Resolve-Path $ExeA).Path; $ExeB = (Resolve-Path $ExeB).Path
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Want([string]$id) { (-not $Only) -or ($Only -contains $id) }
function Wait-Until([scriptblock]$Cond, [int]$Ms = 5000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $Ms) { if (& $Cond) { return $true }; Start-Sleep -Milliseconds 100 }
    $false
}
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Shot($app, $name) { try { Save-Shot $app "$Tag$name" 0 | Out-Null } catch {} }
function FileHash([string]$p) { (Get-FileHash $p -Algorithm SHA256).Hash }

$script:Port = 18740
function New-UpdateEnv {
    param([string]$Name, [int]$ServedBuild, [int]$Min)
    $script:Port++
    $d = New-TestEnv $DataRoot $Name
    New-Item -ItemType Directory -Force "$d\app" | Out-Null
    Copy-Item $ExeA "$d\app\brouillon.exe"
    # harmless fake installer: records its arguments, then installs ExeB over the app copy (retrying while the app exits)
    $rs = @'
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = std::fs::write("@WITNESS@", args.join(" "));
    for _ in 0..150 {
        if std::fs::copy("@SRC@", "@DST@").is_ok() { break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = std::fs::write("@DONE@", "done");
}
'@
    $fwd = { param($p) $p.Replace('\', '/') }
    $rs = $rs.Replace('@WITNESS@', (& $fwd "$d\witness.txt")).Replace('@SRC@', (& $fwd $ExeB)).Replace('@DST@', (& $fwd "$d\app\brouillon.exe")).Replace('@DONE@', (& $fwd "$d\installed.txt"))
    Write-Utf8 "$d\fake.rs" $rs
    & rustc --edition 2021 -C opt-level=0 -o "$d\fake-installer.exe" "$d\fake.rs" 2>&1 | Out-Null
    if (-not (Test-Path "$d\fake-installer.exe")) { throw 'could not build the fake installer' }
    $fx = Join-Path $PSScriptRoot 'update-fixture.mjs'
    & node $fx gen --dir $d --port $script:Port --build $ServedBuild --min $Min --installer "$d\fake-installer.exe" | Out-Null
    Write-Utf8 "$d\settings\settings.toml" "[updates]`ncheck = true`nurl = `"http://127.0.0.1:$($script:Port)/updates/latest.json`"`n"
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $srv = Start-Process node -ArgumentList @("`"$fx`"", 'serve', '--dir', "`"$d`"", '--port', $script:Port) -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 600
    $env:BROUILLON_UPDATE_DEV = '1'
    $env:BROUILLON_UPDATE_FORCE_INSTALLED = '1'
    $env:BROUILLON_UPDATE_DEV_PUBKEY = (Get-Content -Raw "$d\pub.hex").Trim()
    [pscustomobject]@{ Dir = $d; Server = $srv; Exe = "$d\app\brouillon.exe" }
}
function Stop-UpdateEnv($e) {
    try { if ($e.Server -and -not $e.Server.HasExited) { $e.Server.Kill() } } catch {}
    Remove-Item Env:BROUILLON_UPDATE_DEV, Env:BROUILLON_UPDATE_FORCE_INSTALLED, Env:BROUILLON_UPDATE_DEV_PUBKEY -ErrorAction SilentlyContinue
}
function Launch-Upd($e, $tag) { Launch-Brouillon -Exe $e.Exe -Dir $e.Dir -Files @("$($e.Dir)\files\a.txt") -Tag $tag }

# ---- u1: nothing modified -> "Updating to X..." then the installer runs and the app exits; relaunch says "Updated to X"
if (Want 'u1') { Invoke-Scenario 'u1' 'clean document: automatic install, message, relaunch message' {
    $e = New-UpdateEnv 'u1' $BuildB 0
    try {
        $app = Launch-Upd $e 'u1a'
        for ($i = 0; $i -lt 10; $i++) { Shot $app "u1-$i"; if ($app.Proc.HasExited) { break }; Start-Sleep -Milliseconds 250 }
        $exited = $app.Proc.WaitForExit(15000)
        $ran = Wait-Until { Test-Path "$($e.Dir)\installed.txt" } 15000
        $args1 = if (Test-Path "$($e.Dir)\witness.txt") { Get-Content -Raw "$($e.Dir)\witness.txt" } else { '' }
        $swapped = (FileHash $e.Exe) -eq (FileHash $ExeB)
        $app2 = Launch-Upd $e 'u1b'
        Start-Sleep -Milliseconds 800
        Shot $app2 'u1-relaunch'
        $reqs = (Get-Content "$($e.Dir)\requests.log" -ErrorAction SilentlyContinue) -join ' | '
        Kill-App $app2
        "app exited=$exited; fake installer ran=$ran; args=[$($args1.Trim())]; exe replaced by B=$swapped; requests: $reqs"
    } finally { Stop-UpdateEnv $e }
} }

# ---- u2: modified document -> install deferred to the normal exit, journal written, nothing lost
if (Want 'u2') { Invoke-Scenario 'u2' 'modified document: install at close, work journaled' {
    $e = New-UpdateEnv 'u2' $BuildB 0
    try {
        $app = Launch-Upd $e 'u2a'
        Send-Text $app 'UNSAVED-WORK-U2'
        Start-Sleep -Milliseconds 3500
        Shot $app 'u2-ready'
        $beforeClose = Test-Path "$($e.Dir)\installed.txt"
        $journal = (Get-ChildItem "$($e.Dir)\settings\journal" -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object { Get-Content -Raw $_.FullName }) -join ' '
        $howClosed = Stop-AppGraceful $app
        $ran = Wait-Until { Test-Path "$($e.Dir)\installed.txt" } 15000
        $swapped = (FileHash $e.Exe) -eq (FileHash $ExeB)
        $app2 = Launch-Upd $e 'u2b'
        Start-Sleep -Milliseconds 800
        Shot $app2 'u2-relaunch'
        Kill-App $app2
        "installer ran before close=$beforeClose; journal has work=$($journal -match 'UNSAVED-WORK-U2'); close: $howClosed; installer ran after close=$ran; exe replaced=$swapped"
    } finally { Stop-UpdateEnv $e }
} }

# ---- u3: mandatory (min_supported_build above the running build) with unsaved work -> banner, journal, install, work restored
if (Want 'u3') { Invoke-Scenario 'u3' 'mandatory update: 30 s banner, work saved, restored after' {
    $e = New-UpdateEnv 'u3' $BuildB $BuildB
    try {
        $app = Launch-Upd $e 'u3a'
        Send-Text $app 'UNSAVED-WORK-U3'
        Start-Sleep -Milliseconds 3000
        Shot $app 'u3-banner-1'
        Start-Sleep -Milliseconds 9000
        Shot $app 'u3-banner-2'
        $journal = (Get-ChildItem "$($e.Dir)\settings\journal" -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object { Get-Content -Raw $_.FullName }) -join ' '
        $exited = $app.Proc.WaitForExit(40000)
        $ran = Wait-Until { Test-Path "$($e.Dir)\installed.txt" } 15000
        $swapped = (FileHash $e.Exe) -eq (FileHash $ExeB)
        $app2 = Launch-Upd $e 'u3b'
        Start-Sleep -Milliseconds 1200
        Shot $app2 'u3-relaunch'
        Kill-App $app2
        "journal had work during countdown=$($journal -match 'UNSAVED-WORK-U3'); app exited by itself=$exited; installer ran=$ran; exe replaced=$swapped"
    } finally { Stop-UpdateEnv $e }
} }

$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
