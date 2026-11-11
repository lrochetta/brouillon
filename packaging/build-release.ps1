#requires -Version 5.1
<#
 Reproducible alpha release build: release exe -> Inno Setup installer -> portable zip
 -> SHA-256 -> site/public/downloads/{build-info.json,SHA256SUMS.txt}.
 Usage: pwsh packaging/build-release.ps1 [-TargetDir <cargo target dir>] [-SignCommand <command containing $f>]
                                         [-SkipBuild] [-SkipInstaller] [-OutDir <dir>]
 -SkipBuild      do not run cargo; package the already-built <TargetDir>\release\brouillon.exe and brouillon-mcp.exe.
 -SkipInstaller  portable zip only. Also automatic (with a warning) when ISCC.exe (Inno Setup 6) is missing.
                 Without an installer no update manifest (site/public/updates) is written.
 -OutDir         where artifacts, build-info.json and SHA256SUMS.txt go (default site/public/downloads).

 Authenticode signing is OPTIONAL. -SignCommand (or env BROUILLON_SIGN_COMMAND) is a command line in
 which $f stands for the file to sign (examples for SignTool, Trusted Signing and SignPath are in
 packaging/README.md). When it is empty nothing is signed, a warning is printed and build-info.json
 says "signed": false. Unsigned installers trigger Microsoft Defender SmartScreen.
#>
param([string]$TargetDir = "", [string]$SignCommand = "", [string]$OutDir = "", [switch]$SkipBuild, [switch]$SkipInstaller, [string]$HelperExe = "")
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$ws   = Join-Path $root '_bmad-output\build\poc-floem'
$out  = if ($OutDir) { $OutDir } else { Join-Path $root 'site\public\downloads' }
New-Item -ItemType Directory -Force $out | Out-Null
if ($TargetDir) { $env:CARGO_TARGET_DIR = $TargetDir }
$tdir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $ws 'target' }

# version / build metadata
$toml = Get-Content (Join-Path $ws 'crates\brouillon\Cargo.toml') -Raw
if ($toml -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'version not found' }
$version = $Matches[1]
$build  = [int](git -C $root rev-list --count HEAD)
$commit = (git -C $root rev-parse --short=7 HEAD).Trim()
$now    = [DateTime]::UtcNow
$env:SOURCE_DATE_EPOCH = [string][DateTimeOffset]::new($now).ToUnixTimeSeconds()
$env:BROUILLON_BUILD_NUMBER = [string]$build
$date = $now.ToString('yyyy-MM-ddTHH:mm:ssZ')

# 1. release build
if ($SkipBuild) { Write-Host 'SkipBuild: packaging the existing release binaries.' }
else {
  Push-Location $ws
  try { cargo build --release -p brouillon -p brouillon-mcp; if ($LASTEXITCODE) { throw 'cargo build failed' } } finally { Pop-Location }
}
$exe = Join-Path $tdir 'release\brouillon.exe'
$mcpExe = Join-Path $tdir 'release\brouillon-mcp.exe'
if ($HelperExe -and -not (Test-Path $HelperExe)) { throw "missing helper $HelperExe" }
foreach ($bin in @($exe, $mcpExe)) { if (-not (Test-Path $bin)) { throw "missing $bin (build it, or fix -TargetDir)" } }
$vline = & $exe --version
Write-Host $vline

# 1b. optional Authenticode signing hook (skipped, with a warning, when no command is configured)
if (-not $SignCommand) { $SignCommand = $env:BROUILLON_SIGN_COMMAND }
$signed = $false
if ($SignCommand) {
  if (-not $SignCommand.Contains('$f')) { throw 'SignCommand must contain $f (the file to sign)' }
  $cmd = $SignCommand.Replace('$f', '"' + $exe + '"')
  Write-Host "signing $exe"
  Invoke-Expression $cmd
  if ($LASTEXITCODE) { throw 'signing the exe failed' }
  Invoke-Expression $SignCommand.Replace('$f', '"' + $mcpExe + '"')
  if ($LASTEXITCODE) { throw 'signing brouillon-mcp.exe failed' }
  $signed = $true
} else {
  Write-Warning 'No signing command (-SignCommand / BROUILLON_SIGN_COMMAND): exe and installer are UNSIGNED. Windows SmartScreen will warn users on first run.'
}

# 2. installer
$iscc = $null
if ($SkipInstaller) { Write-Warning 'SkipInstaller: building the portable zip only (no installer, no update manifest).' }
else { $iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $iscc) { Write-Warning 'ISCC.exe (Inno Setup 6) not found: building the portable zip only (no installer, no update manifest).' } }
$base = "Brouillon-$version-build$build-win-x64"
$signArgs = @()
if ($signed) { $signArgs = @('/DSign=1', ('/Sbrouillon=' + $SignCommand)) }   # Inno substitutes $f itself (installer + uninstaller)
if ($iscc) {
& $iscc @signArgs "/DAppVersion=$version" "/DBuildNumber=$build" "/DSourceExe=$exe" "/DMcpExe=$mcpExe" $(if ($HelperExe) { "/DHelperExe=$HelperExe" }) "/DLicenseFile=$root\LICENSE" "/DOutDir=$out" (Join-Path $root 'packaging\windows\brouillon.iss')
if ($LASTEXITCODE) { throw 'ISCC failed' }
}

# 3. portable zip
$stage = Join-Path ([IO.Path]::GetTempPath()) "brouillon-portable-$build"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $stage | Out-Null
Copy-Item $exe (Join-Path $stage 'brouillon.exe')
Copy-Item $mcpExe (Join-Path $stage 'brouillon-mcp.exe')
if ($HelperExe) { Copy-Item $HelperExe (Join-Path $stage 'brouillon-remote.exe') }
# Portable marker (brouillon-core paths.rs): settings, session and journal live in .\data next to the exe.
Set-Content (Join-Path $stage 'brouillon.portable') -Value 'Delete this file to use the per-user settings folder instead.' -Encoding ascii
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt')
@"
Brouillon $version (build $build, $commit, $date)
Portable alpha test build - Windows x64, $(if ($signed) { 'Authenticode-signed' } else { 'unsigned (SmartScreen will warn)' }).
Run brouillon.exe. Nothing is installed; delete this folder to remove it.
Portable mode: settings live in the data folder next to the exe (delete brouillon.portable to use the per-user folder).
brouillon-mcp.exe is the optional relay used to connect an AI agent (Tools > Agents on); see docs/INSTALL.md.
brouillon-remote.exe (when present) is the helper for the experimental remote editing (SFTP/FTPS); keep it next to brouillon.exe.
Licence: MIT. Author: Laurent Rochetta. https://brouillon.rochetta.fr
"@ | Set-Content (Join-Path $stage 'README.txt') -Encoding utf8
$zip = Join-Path $out "$base-portable.zip"
Remove-Item $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Remove-Item $stage -Recurse -Force

# 4. hashes + manifest
$setup = Join-Path $out "$base-setup.exe"
function Art($kind, $path) {
  [ordered]@{ kind = $kind; file = (Split-Path $path -Leaf); size_bytes = (Get-Item $path).Length
              sha256 = (Get-FileHash $path -Algorithm SHA256).Hash.ToLower() }
}
$arts = @()
if ($iscc) { $arts += ,(Art 'installer' $setup) }
$arts += ,(Art 'portable' $zip)
$manifest = [ordered]@{
  product = 'Brouillon'; version = $version; build = $build; build_date_utc = $date; published_utc = $date; min_supported_build = 0; git_commit = $commit
  channel = 'alpha-test'; platform = 'windows-x64'; min_os = ('Windows 10 21H2+ (' + [char]0xE0 + ' confirmer)')
  artifacts = $arts; signed = $signed; license = 'MIT'; author = 'Laurent Rochetta'
}
$json = $manifest | ConvertTo-Json -Depth 5
[IO.File]::WriteAllText((Join-Path $out 'build-info.json'), $json + "`n", (New-Object Text.UTF8Encoding $false))
$sums = ($arts | ForEach-Object { "$($_.sha256)  $($_.file)" }) -join "`n"
[IO.File]::WriteAllText((Join-Path $out 'SHA256SUMS.txt'), $sums + "`n", (New-Object Text.UTF8Encoding $false))

# 5. signed update manifest (site/public/updates/latest.json + .sig). The app only installs an
# update whose latest.json.sig verifies; the site build refuses to run without a valid one.
# Nothing is published here. Design: internal design notes (not published)
if (-not $iscc) { Write-Host "Done (portable only, no update manifest): $out"; return }
$keyFile = Join-Path $env:USERPROFILE '.brouillon-release\update-signing.key'
node (Join-Path $root 'site\scripts\update-manifest.mjs') --downloads $out
if ($LASTEXITCODE) { throw 'update-manifest.mjs failed' }
$latest = Join-Path $root 'site\public\updates\latest.json'
if (Test-Path $keyFile) {
  cargo run -q --release --manifest-path (Join-Path $root 'tools\release\Cargo.toml') -- sign $latest
  if ($LASTEXITCODE) { throw 'signing failed' }
} else {
  Write-Warning "Signing key not found ($keyFile): latest.json is NOT signed. Existing installs will not update and the site build will fail until you run 'brouillon-release sign $latest'."
}
Write-Host "Done: $out"
