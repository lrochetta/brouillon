# Remote file systems (SFTP) - design and status

Status (honest, 2026-11-10): **remote editing is EXPERIMENTAL (alpha).** SFTP is wired end to end in code; FTPS and
opt-in plain FTP were added as a second transport of the same helper (`docs/REMOTE_FTP.md`). Nothing has been run against a
real SFTP, FTPS or FTP server through the editor. Seen on screen (release build, built-in fake demo server only): the
browser, connect form, Sites modal, opening a remote file as a tab, edit + Ctrl+S `Saving` -> `Synced`, Disconnect.
NOT seen on screen: the forced conflict and its dialog, the ask-once password and host-key dialogs, recovery windows.
Saved passwords (DPAPI vault, default "use this time only", asked after a successful connect) are wired since 0.1.0-alpha.15 but EXPERIMENTAL: tests only, not seen on screen, not used against a real ssh prompt; portable mode keeps nothing. Cloud drives (Google Drive, Dropbox, Box) are not implemented.
Everything below says which is which.

## How it works

```
editor (UI thread: signals only)
  RemoteUi / Explorer ---- RemoteHub (worker threads) ---- HelperProvider ----+
  SaveService (worker thread per save) ---- SafeSaver ---- HelperFs ----------+--> HelperClient
                                                                                  |  framed protocol on stdin/stdout
                                                                         brouillon-remote.exe  (separate workspace)
                                                                                  |  SFTP v3 over the OS `ssh -s sftp`
                                                                                server
```

* `brouillon-remote.exe` (`_bmad-output/build/remote-helper/`, protocol in `docs/REMOTE_HELPER_PROTOCOL.md`)
  owns every network-facing dependency. It must sit next to `brouillon.exe`; if it does not, the connect
  says so in plain words ("Brouillon's remote helper ... was not found ... Reinstall Brouillon").
  Developer override: `BROUILLON_REMOTE_HELPER=<path>`. The editor links only the protocol types
  (`brouillon-remote-proto`, a path dependency on the helper workspace; it builds inside the main
  workspace without touching the stubs or the `[patch]` table).
* `brouillon-editor-engine/src/remote/`:
  * `client.rs` `HelperClient`: blocking calls with per-call timeouts (stat 30 s, streaming idle 30 s,
    whole transfer 15 min, connect 60 s but paused while a question waits for the user), at most 3 requests
    in flight, write flow control (never more than the announced window unacknowledged). A dead helper
    (process killed, pipe closed, bad frame, `state lost`) fails every pending call at once and every later call
    immediately; a dead client is never revived.
  * `helper.rs` `HelperProvider` (the hub's `RemoteProvider`: list / stat / read / write / mkdir / rename / remove),
    `HelperFs` (the SafeSaver's `RemoteFs` over the same session), `HelperFactory` (spawns the helper),
    `PromptBroker` (the question hand-over to the UI), `RealOrDemoFactory`.
    A dead helper surfaces as `BrokenPipe` -> the explorer marks the site Offline once; Reconnect starts a NEW helper.
  * `saving.rs` `SaveService`: runs `SafeSaver` off the UI thread, keeps one base per open file, publishes
    `SaveEvent`s; `view_of` maps every `SaveOutcome` to the chip state, the dialog and "may the buffer be called saved".
  * `hostkey.rs`: removal of a pinned old identity with `ssh-keygen -R` (only behind the typed-site-name gate).
* `brouillon-ui/src/remote_view.rs` + `remote_dialogs.rs`: the explorer and the dialogs.
* Demo / tests: `FakeRemoteProvider` serves only host `demo` or `BROUILLON_REMOTE_FAKE=1`
  (`BROUILLON_REMOTE_FAKE_LATENCY_MS` slows it). It has no safe save: its Ctrl+S is the old plain upload.
* `sftp.rs` + the "Local SFTP Simulation Panel" (Ctrl+Alt+S) are a **local simulation** (a `sample/sftp` folder).
  They never open a socket and must not be presented as SFTP.

## What happens on Ctrl+S in a remote tab

1. The UI thread hands the text to `SaveService::save` and returns at once (chip: `Saving...`).
2. A worker runs the SafeSaver: journal the buffer locally first, compare the server file with the base
   recorded when it was opened, back up the version about to be replaced (local, per site), upload to a hidden
   `.<name>.brouillon-<rand>.tmp` in the same folder with exclusive create, check size, atomic
   `posix-rename` over the target (without `posix-rename@openssh.com` the swap is 3 steps and the status says
   it was not atomic).
3. Only `Saved` calls `RemoteHost.saved` (clears the modified marker) and shows `Synced`.
4. Conflict (server changed, you changed too), Stale (server newer, you changed nothing) and RemoteGone write
   **zero bytes** and open a dialog: Cancel (focused), Reload, Overwrite (their version is backed up first) /
   Create it again, Save mine as. Comparing the two versions side by side is **not** offered yet.
5. Network trouble: the document is journaled, the chip says `Pending`, the text stays in the tab. Ctrl+S or
   the palette command "Remote: Save again" retries with the current text.
6. Journal: `<settings>/remote/journal/<site>/`, backups: `<settings>/remote/backups/`.

Recovery: when a site connects, `recover_all()` runs on its journal (removes our own orphan temp files, puts a
swapped-out file back; it never uploads a buffer). Buffers that never reached the server are listed from disk at
launch (status line message) and in the palette command **"Remote: Recover unsaved changes"** (Open puts the text back
in a tab as modified, after which the normal save re-checks the server; Put aside keeps it 7 days).

## The sidebar browser (Local, sites, cloud folders)

The left column holds two collapsible sections: Open Files (Moldu preset, unchanged) and the Browser, with a draggable
divider. Split, collapsed flags, favorites, recent folders and sort live in `browser.toml` next to `sites.toml`
(paths and site ids only, never a host, password or key).

* One source switcher: Local, saved / open sites, Add a site. Local is just one more source of the remote machinery:
  `LocalProvider` is served by the same hub worker, `ListingCache` and `RemoteTree` (loading rows, 5 000 entry cap with
  a "N more" row), so a huge local folder never blocks the UI. Nothing starts until the browser is shown.
* Google Drive, Dropbox and Box have no connector yet. If the vendor's desktop app already syncs a folder (`~/Dropbox`,
  `~/Google Drive`, a `G:\My Drive` virtual drive, `~/Library/CloudStorage/...`, Dropbox's `info.json`) the entry is a
  Local shortcut "synced by ..."; otherwise it is a disabled "Not connected (coming soon)" line. No API, no network,
  no login. The folder scan is one short thread started when the browser is first shown.
* Navigation: breadcrumb (every segment clickable), Back / Forward (history per source), Up, Home, F5, Ctrl+L to type a
  path, favorites and recents (Places menu), sort by name / size / modified / type (folders always first),
  type-to-filter, F2 rename, Delete (with confirmation, no recycle bin, a non-empty local folder is refused).
* Agents: the browser is not connected to the agent bridge. An agent sees only documents open in tabs, under the usual
  consent rules (a file opened from a synced cloud folder is still a local file to that policy, see cloud/03 D13).

## Credentials

* Password, passphrase and host-key questions come from ssh through the helper (`SSH_ASKPASS`) as `prompt` events and
  are shown in an ask-once dialog. The answer is sent once as `prompt_answer`, never logged, stored, or put in an
  error. The default is "use it this time only" (decision Q2), said on the dialog; keeping a password is a separate question after a successful connect (EXPERIMENTAL, see `docs/SECURITY.md`).
* Unknown host key: card with ssh's own text, **Cancel has focus**, trusting needs Tab/click on "Trust and connect".
* Changed host key: hard stop (the helper has no override). The dialog needs the **typed site name** before
  "Replace and connect" does `ssh-keygen -R` on the old entry and reconnects.
* The masked field is hand-made (floem 0.2 has no masked input): it shows `*`, takes the keyboard, supports
  Backspace and Ctrl+V. The typed text is a plain `String` (not a signal), moved into the answer and dropped; it is
  not wiped from memory beyond that.

## Proven by tests (no ssh, no sockets)

The server side is the REAL helper code running in-process over `std::io::pipe`s in front of the helper crate's
fake SFTP server (`remote/helper_tests.rs`, in the editor engine; plus one test that spawns the real
`brouillon-remote.exe` for `hello` / `ping` / refusal of an option-looking host, with no connection made):

* save happy path (verified, no temp left, journal cleared, backup made); conflict writes zero bytes (the server's
  mutation log is unchanged); overwrite backs up their version first; no `posix-rename` is reported as not atomic;
* the server hanging up mid-upload, and the helper killed mid-upload: the target holds old or new bytes, never a
  mix, the buffer stays in the journal (and re-opens byte for byte), recovery on a fresh helper removes the orphan
  temp file and does not upload;
* a typed password reaches the login side exactly once and a canary never appears in the client log, the raw bytes
  the helper sent back, error strings, `Debug` output or any file the save service wrote; cancelling a prompt fails
  the connect; an unanswered prompt is withdrawn when the helper dies;
* a silent helper times out and a closed pipe / a bad frame kills the session; a dead helper makes the explorer mark the
  site Offline once and Reconnect starts a fresh helper;
* no server call runs on the calling (UI) thread: a save returns while the server is blocked, and every call runs on
  another thread.

## NOT proven (needs a real sshd and/or a screen)

* **Never run against a real sshd or a real `ssh.exe`**: SFTP through ssh, agent / key / password / passphrase auth,
  the host-key prompt and the changed-key failure as ssh really words them, `posix-rename@openssh.com` on a real server,
  big files and slow links, the helper surviving a real `ssh` crash. The helper's own doc lists its gaps too.
* **Never seen on screen**: the ask-once dialogs, the masked field (does it receive keys? does it paste?), Cancel taking
  focus, the conflict / changed-key / recovery windows, the chip changing, a reloaded or recovered tab being marked
  modified, the palette commands, the explorer against a helper-backed site.
* Not built: side-by-side compare in the conflict dialog, a three-way merge, 
  non-UTF-8 remote files (they are refused to open), resume of an interrupted upload.
* Known cost: opening a file does stat / read / stat (two extra cheap calls) so the save has a sound base.
* Known gap: a helper that stops reading its stdin while we write blocks that one worker thread (the UI stays free);
  that worker is only released when the helper is killed (no watchdog does that yet).
