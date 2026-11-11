# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Generated and maintained with the help of [git-cliff](https://git-cliff.org/) from
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/). See
[`docs/VERSIONING.md`](docs/VERSIONING.md) for the workflow.

While the project is in **0.x**, anything MAY break between minor versions.

---

## [Unreleased]

### Added
- **Automatic update at launch.** In the background, after the first frame, Brouillon checks the signed `latest.json` (Ed25519, pinned keys), refuses older builds and replays, enforces `min_supported_build`, throttles to once per `updates.check_interval_hours`, and stays silent offline. A newer build is downloaded with progress and verified (size, SHA-256); with `updates.auto_install` (default on) it starts by itself when no document is unsaved, otherwise a banner offers Update now / Later / Skip this version. Portable copies only show the file name, hash and link. The request carries no identifier, no cookie, no query string and no version. New settings: `updates.auto_install` (replaces `updates.auto`), `updates.check_interval_hours`, `updates.channel`. The check is on by default and can be turned off on the first-run screen or in Preferences; an offline launch is unaffected.
- The installer runs per user, never elevated, with its temp folder inside the data folder; if it fails the build is remembered and a manual-download message is shown. The Inno script reopens the app after a silent update (`/RELAUNCH=1`).

### Changed
- Update tests use an in-process fake HTTP transport (no sockets); the two tests that compile and run a fake installer are `#[ignore]`.

---

## [0.1.0-alpha.15] — 2026-11-11

Written by hand from `git log 918c26f..HEAD` (alpha.14 was commit 918c26f). This is an alpha: the two new features below are EXPERIMENTAL. They are covered by tests but have not been seen working on screen.

### Highlights
- **Lower memory in CPU render mode.** The CPU render mode no longer creates GPU resources. Measured on an empty document, 4 launches each, before and after: private memory 88.1 to 79.5 MB, working set 69.7 to 60.8 MB. The window still appears as fast (43 to 106 ms). GPU mode was not re-tested on screen.
- **Saved passwords for remote sites, EXPERIMENTAL.** After a successful connect, Brouillon asks "Keep this password?". The default is "use this time only". A read-only Credentials view lists what is kept (no values) and can Forget each entry. Tests only: never seen on screen, never used against a real ssh password prompt.
- **Hex editor, first slice, EXPERIMENTAL.** Data inspector, go to offset, regions, copy as, hashes and nibble editing. Tests only: never seen on screen.

### Added
- Credential vault wired to the interface (Windows DPAPI, user scope, Standard mode). A wrong saved password falls back to asking and offers to replace it. The vault opens only when needed, locks again after 15 minutes idle, on the kill switch and on Panic. Portable mode keeps nothing.
- Hex: data inspector with a little/big endian toggle, Go to Offset (Ctrl+G), named regions kept per file, Copy as (hex, spaced hex, C array, Rust array, Python bytes, Base64, ASCII), hashes of the selection (CRC-32, MD5, SHA-1, SHA-256) with progress, nibble editing with undo and redo.
- Idle memory trimmer, cached themes, and a memory tooltip showing working set and private bytes. Not measured on screen yet.

### Changed
- The never-share list for agents now also covers vault backups and temporary vault files, and the hex regions folder. Hex commands cannot be run by agents.

### Known limits
- Hex: saving rewrites the whole file on the interface thread, so a big file can freeze the window while it saves. Closing with unsaved byte edits is not guarded.
- Memory on a 100 MB file: about 77 to 90 MB private in a 29-minute run (peak 128 MB). The 80 MB goal is not met. That run was not repeated on the alpha.15 executables.
- Remote editing is still tested against fake servers only. Cloud drives are not implemented. Builds are unsigned.

---

## [0.1.0-alpha.14] — 2026-11-10

Written by hand from `git log v0.1.0-alpha.13..HEAD` (about 260 commits); git-cliff is not installed on the build machine. This is an alpha: what is listed as unproven below has tests but has not been seen working for real.

### Highlights
- **Brouillon Link (optional AI agents).** Off by default. An agent such as Claude Code can read your open documents through a local relay, only after a consent dialog. Agents never see files on the never-share list, and secrets in text are masked. Real test: Claude Code connected and answered about 30 seconds after launch.
- **One-click EditPlus import** at first run, plus the **Classic keymap** (all 20 Classic commands now dispatch).
- **Huge files.** Files of 16 MB and more use a lazy text store. In a 30-minute mixed test on a 100 MB file the editor stayed around 100 MB of private memory (99 to 109 MB) with no crash. This is not yet under the 80 MB goal.
- **Left Browser** below Open Files: Local folders, saved sites, synced cloud folders, with breadcrumb, Back/Forward/Up/Home, filter and sorting.
- **Remote editing, EXPERIMENTAL.** SFTP (through the system `ssh`), FTPS and opt-in plain FTP, with a safe-save model (journal first, conflict check, hidden temp file, atomic swap, local backups). Tests use fake servers only; no real server has been used through the editor yet.
- **Portable zip** with the editor, the agent relay and the remote helper.

### Added
- Dark, Light and Follow System themes, with a dark title bar; no-JavaScript theme picker on the website.
- Preferences dialog (Ctrl+,), Keyboard Shortcuts dialog, Quick Open (Ctrl+P: files, `:line`, `@symbol`, `#text`), Cliptext panel with a default library (Ctrl+Alt+1..0).
- Multi-cursor editing, outline / Go to Function, minimap, compare view, Emmet, macros, bookmarks list, indent guides, column ruler, distraction-free mode, File > Print.
- Folder tree panel remembered per workspace.
- Welcome, Documentation and Release notes tabs (read-only), chosen at start-up.
- External-change bar: unmodified files reload quietly, modified ones ask before anything is overwritten.
- Signed automatic update (opt-out): a signed manifest is checked at start-up; it can be turned off.
- Brand icon on the window, taskbar and installer; CPU renderer by default, optional GPU mode, render-mode badge in the header.
- Credential groundwork: offline secret scanner and redactor, credential vault file format (not wired to the interface, so no password is stored), EditPlus importer hardened against embedded credentials.
- Packaging: portable zip without Inno Setup, winget / Scoop / Homebrew manifest generator (nothing submitted), optional signing hook and a draft signing workflow (nothing is signed).

### Changed
- The toolbar row is gone. Undo, Redo, Keyboard (Clavier) and Pen (Crayon) now sit in the header after the fps counter. Open, Save, Cut, Copy, Paste and Find are in the menus.
- Memory: typing, searching and opening tabs no longer grow memory (leaks fixed); saving, Ctrl+P and the outline no longer copy the whole document.
- Find in Files streams files over 10 MB instead of skipping them; the Find bar searches large buffers on a worker thread with a live count.
- Website: indexable, "Latest build" label from the release manifest, no third-party product names.

### Fixed
- Data integrity: recovered and hot-exit documents open as modified and are never dropped; one durable write path for saves, session, journal and backups; unsaved Crayon strokes survive restarts.
- Keyboard: space bar, Ctrl+Alt+letter chords on AZERTY, Ctrl+digit on the physical key, Enter in Find/Replace, Enter no longer accepts the completion popup (Tab does), arrow keys with the popup open.
- Visible text caret; mouse-wheel scrolling no longer snaps back; long documents no longer push the status bar away; menus open, close and switch correctly; menu text meets contrast AA in every theme.

### Known limits
- No real SFTP, FTPS or FTP server has been used through the editor. The conflict dialog has not been seen on screen.
- No password is stored (the vault is not wired). Cloud drives (Google Drive, Dropbox, Box) are not implemented; only folders already synced by the vendor's app appear.
- Hex editing is upcoming. Startup measured 41 to 62 ms until the window is visible on the test machine; a quiet-machine re-measure is pending.
- The installer was not built on this machine (no Inno Setup); builds are unsigned.

---


---

Earlier pre-alpha development history (PoC days and alphas 0 to 13) is not part of this public repository.
