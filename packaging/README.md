# Packaging and distribution (Brouillon)

Everything here is OFFLINE tooling. Nothing in this folder publishes, uploads or submits anything.
Every submission below is a manual step for the maintainer.

| Path | Purpose |
|---|---|
| `build-release.ps1` | release exe -> optional signing -> Inno installer -> portable zip -> SHA-256 -> `site/public/downloads/{build-info.json,SHA256SUMS.txt}` -> update manifest |
| `windows/brouillon.iss` | Inno Setup 6 script |
| `make-manifests.mjs` | templates winget / Scoop / Homebrew manifests from `build-info.json` + `SHA256SUMS.txt` |
| `make-manifests.test.mjs` | `node --test packaging/make-manifests.test.mjs` |
| `dist/` | generator output (git-ignored) |

## 1. Windows installer and portable zip

Reviewed properties of the installer (`windows/brouillon.iss`):

- Per-user, never elevated: `PrivilegesRequired=lowest`, and `PrivilegesRequiredOverridesAllowed` is deliberately absent so setup can never switch to an all-users install. Files go to `%LOCALAPPDATA%\Programs\Brouillon`. No admin prompt, also for the silent auto-updater.
- Uninstall: Settings > Apps, `unins000.exe`, or `winget uninstall lrochetta.Brouillon`. It removes the app folder, the Start menu entry, the optional user PATH entry and the optional file-association keys. It does not touch your settings folder (user data stays on purpose).
- File associations are optional and off by default (task `assoc`). They only add Brouillon to the "Open with" list of `.txt .md .log .json .toml .yaml .csv` under `HKCU\Software\Classes`; they never take over the default app. The command is `brouillon.exe "%1"` (the CLI accepts `path[:line[:col]]`).
- Optional "Add to user PATH" task, off by default, reverted on uninstall.
- Known minor limits (not fixed, harmless): the PATH entry is appended as `;<app>`, which leaves a leading `;` when the user PATH was empty; uninstall removes the entry only when it is not the very first PATH element.
- Compiled here with Inno Setup 6 (unsigned): OK.

Portable zip: `brouillon.exe`, `brouillon-mcp.exe`, `brouillon.portable`, `LICENSE.txt`, `README.txt`. Nothing is installed, no registry writes, no admin; delete the folder to remove it. The `brouillon.portable` marker (see `brouillon-core/src/paths.rs`) makes settings, session and journal live in a `data` folder next to the exe; no Rust change was needed.

### Portable zip without Inno Setup

```powershell
.\packaging\build-release.ps1 -TargetDir D:\travail\_target-release -SkipBuild -SkipInstaller -OutDir $env:TEMP\brouillon-out
```

- `-SkipBuild`: no cargo; packages the existing `<TargetDir>\release\brouillon.exe` and `brouillon-mcp.exe` (the script stops if either is missing). Without it the script runs `cargo build --release -p brouillon -p brouillon-mcp`.
- `-SkipInstaller`: portable zip only. This also happens automatically, with a warning, when `ISCC.exe` is not found.
- `-OutDir`: where the zip, `build-info.json` and `SHA256SUMS.txt` go (default `site/public/downloads`).
- Without an installer, `build-info.json` lists only the portable artifact and the signed update manifest (`site/public/updates`) is NOT written, since it needs an installer. winget needs an installer, so only Scoop is generated from such a build.
- Package managers and user steps: `docs/INSTALL.md`.

### Signing hook (optional)

`build-release.ps1 -SignCommand "<command containing $f>"` (or env `BROUILLON_SIGN_COMMAND`). `$f` is replaced by the file to sign. The command signs `brouillon.exe` before packaging and is passed to Inno (`/Sbrouillon=...`, `/DSign=1`) so the installer and uninstaller are signed too. Hashes are computed after signing. `build-info.json` records `"signed": true`.

If no command is configured the script SKIPS signing, prints a warning and records `"signed": false`.

WARNING: an unsigned installer or exe triggers Microsoft Defender SmartScreen ("Windows protected your PC"), and unsigned Rust binaries are more exposed to antivirus false positives. Users must click "More info" > "Run anyway". Even a signed build starts with no SmartScreen reputation unless the certificate type builds it quickly; do not promise users a warning-free first run.

Examples (NOT executed or validated here: no certificate and no network; check each against its vendor docs):

```powershell
# A. SignTool with a certificate file (password from the environment, never on disk in the repo)
.\packaging\build-release.ps1 -SignCommand 'signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /f C:\secure\brouillon.pfx /p $env:CERT_PASSWORD $f'

# B. Azure Trusted Signing (now "Artifact Signing"): SignTool + the Azure dlib and a metadata JSON
.\packaging\build-release.ps1 -SignCommand 'signtool sign /v /fd SHA256 /tr http://timestamp.acs.microsoft.com /td SHA256 /dlib C:\tools\Azure.CodeSigning.Dlib.dll /dmdf C:\secure\metadata.json $f'
```

C. SignPath (free for open source, requires approval and a public repository): it is an asynchronous cloud service (you submit the unsigned file, then download the signed one), so it does not fit a one-line synchronous `$f` command. Its usual flow is a CI job that submits the artifact and waits. This script does not implement it; if you choose SignPath, sign `brouillon.exe` in that flow, then run the Inno step on the signed exe. That involves uploading files, so it is a decision for you.

Order of preference for a solo open-source author: SignPath Foundation (free, if accepted), then Trusted/Artifact Signing (paid, identity validation), then an OV/EV certificate with SignTool.

## 2. Generate package-manager manifests

After `build-release.ps1` has produced `site/public/downloads/`:

```powershell
node packaging/make-manifests.mjs --base-url https://brouillon.rochetta.fr/downloads
# options: --downloads <dir> (default site/public/downloads)  --out <dir> (default packaging/dist)
```

- `--base-url` is the ONLY source of real URLs. Without it every URL is `https://release-base-url.invalid` and each file is marked `NOT SUBMITTABLE`.
- The script refuses to run if `SHA256SUMS.txt` and `build-info.json` disagree.
- Output: `dist/winget/manifests/l/lrochetta/Brouillon/<version>/` (3 files), `dist/scoop/bucket/brouillon.json`, `dist/homebrew/Casks/brouillon.rb` (draft).
- Before submitting, the published files must really exist at those URLs and hash to the listed values (download them and compare with `Get-FileHash`). The winget installer URL must be version-pinned and never overwritten later.

### winget (`lrochetta.Brouillon`)

Validated here: `winget validate --manifest <dir>` passes (winget 1.29, schema 1.10.0) on the generated files. NOT validated: installing from the URL (no network), and the winget-pkgs moderation pipeline.

Human steps:

1. Publish the release files at the URLs in the manifest (site `downloads/` or a GitHub release) and check the hash.
2. Re-run the generator with the final `--base-url`.
3. Local test (needs admin once): `winget settings --enable LocalManifestFiles`, then `winget install --manifest packaging\dist\winget\manifests\l\lrochetta\Brouillon\<version>`, run the app, `winget uninstall lrochetta.Brouillon`.
4. Fork `https://github.com/microsoft/winget-pkgs`, copy the `manifests/l/lrochetta/Brouillon/<version>/` folder into the same path in your fork, commit on a branch (`lrochetta.Brouillon version <version>`).
5. Open a pull request to `master` and follow the checklist in its template. Automated validation installs the package in a sandbox; an unsigned installer may be flagged by SmartScreen/Defender there. Fix or explain in the PR.
6. Open question for you: whether winget-pkgs accepts this pre-release version string (`0.1.0-alpha.13`) as-is; check the current policy before submitting.

### Scoop (bucket JSON, portable zip)

Validated here: valid JSON; the `checkver` regex and the `autoupdate` URL/hash regexes are exercised in tests against a PowerShell-style `build-info.json` and `SHA256SUMS.txt`. NOT validated: Scoop itself (not installed here), so the `$matchBuild` substitution and `hash` extractor are unverified against real Scoop.

Human steps:

1. Create a public GitHub repository named `scoop-brouillon` (any name works).
2. Copy `dist/scoop/bucket/brouillon.json` to `bucket/brouillon.json`. Optional: take the GitHub Actions "Excavator" workflow from `ScoopInstaller/BucketTemplate` to run `checkver` + `autoupdate` on a schedule (this is automation you enable yourself).
3. Local test: `scoop install .\brouillon.json`, then `scoop checkver .\brouillon.json -u -f` (needs the published files).
4. Users install with `scoop bucket add brouillon https://github.com/<your-user>/scoop-brouillon` then `scoop install brouillon`.
5. Optional: later propose it to the official `extras` bucket (it has notability rules).

### Homebrew (DRAFT only)

Brouillon ships Windows x64 only, so `dist/homebrew/Casks/brouillon.rb` is a placeholder draft (file name and sha256 are marked `REPLACE_WITH...` until `build-info.json` lists a `.dmg`). It was not validated (no Ruby, no Homebrew here) beyond a structural check. Do not publish it until a signed and notarized macOS build exists.

Human steps once a macOS build exists:

1. Create a public repository `homebrew-brouillon` and put the cask at `Casks/brouillon.rb`.
2. Test on a Mac: `brew install --cask ./Casks/brouillon.rb`, `brew audit --cask --online brouillon`, `brew style`.
3. Users install with `brew tap <your-user>/brouillon` then `brew install --cask brouillon`.

## 3. What was and was not checked in this environment

Checked: Inno script compiles (unsigned, and the `Sign` hook is wired: Inno invoked the configured command and rejected a fake signature); `build-release.ps1` parses; 10 `node:test` tests pass, including `winget validate`.
Checked on 2026-11-10 (unsigned, local only): `build-release.ps1 -SkipBuild -SkipInstaller` produced the portable zip, `build-info.json` (`signed:false`, portable artifact only) and `SHA256SUMS.txt`; the zip was unzipped to a fresh folder and its `brouillon.exe --version` and `--help` exit 0 (run via `Start-Process -Wait` because the exe is a GUI-subsystem binary and PowerShell does not wait for it); the zip hash matched. The same script without `-SkipInstaller` also built the Inno installer (Inno Setup 6 was present under `%LOCALAPPDATA%`), and `winget validate --manifest` passed on the manifests generated from that output with a placeholder `--base-url`. The Scoop JSON parses; the Homebrew cask is a placeholder (no macOS build) and Ruby is not installed here, so it was not syntax-checked.
Not checked: installing from a real URL, a real signing run, Scoop/Homebrew themselves, installer behaviour on a clean machine (install, "Open with" entries, uninstall), anything requiring network, Scoop, Homebrew.
