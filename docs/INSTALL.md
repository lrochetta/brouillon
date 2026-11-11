# Installing Brouillon

Brouillon is a free, open-source (MIT) text editor for Windows x64. It has no account and no telemetry.
Download files from the official site, https://brouillon.rochetta.fr/downloads, and nowhere else.

## 1. Portable zip (works today)

1. Download `Brouillon-<version>-build<N>-win-x64-portable.zip`.
2. Verify it (section 4).
3. Unzip anywhere (a USB stick is fine). The folder contains:
   - `brouillon.exe`: the editor.
   - `brouillon-mcp.exe`: the optional relay for connecting an AI agent (section 5).
   - `brouillon-remote.exe`: the helper for remote editing (SFTP, FTPS, FTP; experimental). **It must stay in the same folder as `brouillon.exe`.** If it is missing or moved, connecting to a server fails with a message saying the helper was not found; the rest of the editor is unaffected. SFTP also needs the Windows `ssh` client (Settings > Optional features > OpenSSH Client).
   - `brouillon.portable`: marker file. While it sits next to `brouillon.exe`, settings, session and journal are kept in a `data\` folder next to the exe and nothing is written under your user profile. Delete the marker to use the per-user folder (`%USERPROFILE%\.brouillon`) instead.
   - `LICENSE.txt` and `README.txt`.
4. Double-click `brouillon.exe`. Nothing is installed and there is no registry write. To remove it, delete the folder.

Check it from a terminal: `brouillon.exe --version` and `brouillon.exe --help`.

## 2. Installer (when available)

A per-user installer (`Brouillon-<version>-build<N>-win-x64-setup.exe`) is built when Inno Setup 6 is present on the build machine. Not every release has one; if the downloads page lists no `-setup.exe`, use the portable zip. It never asks for administrator rights and installs to `%LOCALAPPDATA%\Programs\Brouillon`. File associations and the PATH entry are optional and off by default. Uninstall from Settings > Apps.

## 3. Package managers (once published)

Not published yet: these commands do not work today. They are listed so you know what to expect.

```powershell
winget install lrochetta.Brouillon                  # once published
scoop bucket add brouillon <bucket-url>             # once published
scoop install brouillon
brew install --cask brouillon                       # once a macOS build exists
```

### SmartScreen warning (unsigned builds)

Builds are not Authenticode-signed yet (`"signed": false` in `build-info.json`). On first run Windows may show "Windows protected your PC". Click "More info", then "Run anyway", only after you have checked the SHA-256 below. Antivirus products can also flag unsigned binaries; the checksum is how you know the file is the one that was published.

### "Cannot run a file from the temporary folder" during setup

The installer unpacks itself into your temporary folder and runs from there. If your antivirus or Windows blocks that (common for new, unsigned programs; Inno Setup error codes 2, 5 or 193), setup stops with this message. Options, simplest first:

1. Use the portable zip: unzip it into a folder of your choice and run `brouillon.exe`. Nothing is installed and nothing runs from the temporary folder.
2. Right-click the setup file, Properties, tick **Unblock**, then run it again.
3. Add an exception for the setup file (or temporarily for `%TEMP%`) in your antivirus, then run it again.
4. Run setup from a plain local folder such as Downloads, not from a synced or network folder.

Code signing is the lasting fix; see `docs/SIGNING.md`.

## 4. Verify the SHA-256

Each download folder has `SHA256SUMS.txt` (and `build-info.json` with the same hashes). In PowerShell:

```powershell
Get-FileHash .\Brouillon-<version>-build<N>-win-x64-portable.zip -Algorithm SHA256
Get-Content .\SHA256SUMS.txt
```

The two hashes (case does not matter) must be identical. If they differ, delete the file and do not run it.

## 5. Connect Claude Code in under a minute (Brouillon Link)

Brouillon Link lets an AI agent read your open documents through a local relay. It is off by default, runs on your machine only, and asks before the agent can read anything. These are the steps used in the real test with Claude Code (launch to first answer: about 30 seconds).

1. Start Brouillon and turn on Tools > Agents. (For a one-off session you can instead start `brouillon.exe --mcp`; it is not remembered.)
2. In a terminal, register the relay with Claude Code, using the full path of `brouillon-mcp.exe` from your unzipped folder or install folder:

   ```powershell
   claude mcp add brouillon -- "C:\path\to\brouillon-mcp.exe" --client claude-code --link-dir "%LOCALAPPDATA%\Brouillon\link"
   ```

   The link folder is `%LOCALAPPDATA%\Brouillon\link` by default. If you start Brouillon with `--settings-dir <dir>`, it is `<dir>\link` instead; use that folder in the command.
3. Ask Claude Code to use the editor, for example: `Call the brouillon editor_state tool`.
4. Brouillon shows a consent dialog the first time the agent asks. Press Enter to accept the default, which is read only. The first call only raises the question; Claude Code's next call is answered.

To undo it: `claude mcp remove brouillon`, and switch Tools > Agents off.

## 6. Building the packages yourself

See `packaging/README.md` (`packaging/build-release.ps1 -SkipInstaller` builds the portable zip alone).

## 7. Build from source

Needs a recent stable Rust toolchain (rustc 1.94 or newer) and, on Windows, the Visual Studio Build Tools (Desktop development with C++).

```powershell
cd _bmad-output\build\poc-floem
cargo build --release -p brouillon -p brouillon-mcp
.\target\release\brouillon.exe --version
```

On Linux you also need the Wayland or X11 development headers, `pkg-config`, `libxkbcommon`, `libxcb`, `libssl` and `fontconfig` development packages. macOS needs the Xcode Command Line Tools. Only Windows x64 builds are packaged today; Linux and macOS are source-only and less tested.

Uninstall a source build by deleting the binaries. Settings live in `%USERPROFILE%\.brouillon` (or `data\` next to the exe in portable mode); remove that folder if you also want your settings gone.
