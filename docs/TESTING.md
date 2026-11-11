# Testing the GUI without taking over the PC (the `test-input` channel)

The smoke scenarios in `_bmad-output/build/poc-floem/tools/smoke/` can run in two ways.

| Backend | Dot-source line | Needs | Side effects |
|---|---|---|---|
| Real input (`lib.ps1`) | `. lib.ps1` | a normal exe, the foreground | drives the real mouse and keyboard; aborts with `FOCUS_LOST` when another window comes forward |
| Test input (`testinput.ps1`) | `. testinput.ps1` | a DEV exe built with `--features test-input` | none: no SendInput, no cursor, no foreground, window invisible |

Use the real backend for what only real input can prove (IME, key repeat, pointer capture, the title bar, a release exe).
Use the test-input backend for everything else, so a run never blocks the owner.

## Build and run

```
cd _bmad-output/build/poc-floem
cargo build -p brouillon --features test-input          # dev profile; the exe is target/debug/brouillon.exe
powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\onboarding.ps1 -Exe <that exe> -Only f1
```

`testinput.ps1` starts the exe with a random pipe name and a random token in its environment. In background mode
(`BROUILLON_TEST_INPUT_BACKGROUND=1`, set by the client; pass `-Visible` to `Launch-Brouillon` to see the window) the
window is created not activated at the bottom of the z-order, then made layered with alpha 1/255, click-through,
tool-window and no-activate: it stays on-screen so it is painted, but it is invisible, takes no click and has no
taskbar button.

## Threat model: why this cannot be on in a shipped build

* **Compile time.** The code exists only with the cargo feature `test-input` (off by default, in no `default` list).
  `crates/brouillon/src/test_input.rs` holds a `compile_error!` for the feature combined with a release profile without
  debug assertions, so `cargo build --release --features test-input` does not build. A profiling build needs the
  deliberate `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true`, which no release script sets.
* **Release proof.** `tools/smoke/check-no-test-input.ps1 -Exe <release exe>` searches the binary for the channel's
  markers (`BROUILLON_TEST_INPUT`, `brouillon-test-input`, `[test-input]` ...) and fails when any is present. Run it
  before publishing.
* **Run time.** Even in a feature build nothing listens unless BOTH `BROUILLON_TEST_INPUT=<pipe name>` and
  `BROUILLON_TEST_INPUT_TOKEN=<token of at least 16 characters>` are set. The first line a client sends must be
  `{"auth":"<token>"}` (compared in constant time); anything else closes the connection.
* **No network.** The channel is a Windows named pipe (`\\.\pipe\<name>`): local to the machine, no TCP, no HTTP.
  The token is random per run and lives only in the child's environment.
* **What someone holding the token could do:** drive the editor like a user, screenshot it, read the open document's
  first 200 characters, caret, tab titles and memory counters. Whoever can set the environment of the process and read
  its memory already owns the user session; the channel adds nothing there.
* A feature build prints `[test-input] DEV-ONLY channel listening ...` on stderr at start.

## Protocol (JSON lines, UTF-8)

The client sends `{"auth":"<token>"}` and gets `{"ok":true,"hello":"brouillon-test-input","proto":1}`. Then one request
per line, one reply per line, matched by `id`. Coordinates are window-client LOGICAL pixels (the units of the views).

| op | fields | notes |
|---|---|---|
| `key` | `chord` (`ctrl+shift+f`, `alt+left`, `f3`, `enter`, `a`) | modifier key events are emitted too |
| `text` | `text` | one press/release per character, `\n` is Enter |
| `click` | `x`,`y`,`button` (left/right/middle),`count`,`mods` | `count` 2 = double click; `mods` e.g. `shift`, `ctrl+shift` held during the click |
| `move`, `drag`, `wheel` | `x`,`y`; `x1,y1,x2,y2,steps`; `x,y,dx,dy` | wheel in lines, like winit's `LineDelta` |
| `resize` | `w`,`h` | asks the OS for a new client size |
| `screenshot` | `path`, `settle_ms` | PNG of the window content (client area) via `PrintWindow(PW_CLIENTONLY \| PW_RENDERFULLCONTENT)` |
| `state` | `lines` | `{state:{focus,active_tab,tab_count,tab_titles,tab_title,dirty,caret,text_len,text_head,palette_open,private_bytes,working_set}, lines:[recent state-trace lines]}` |
| `ping`, `quit` | | `ping` waits until the GUI thread is processing events |

Every input reply is sent after the GUI thread ran the event (a barrier), but the repaint that follows is a separate
turn: wait for it with `Wait-State` or the `settle_ms` of a screenshot. The state-trace lines are the `[remote-dbg]`
lines (see `BROUILLON_REMOTE_DEBUG`), kept in a ring buffer while the channel is on.

## PowerShell client

`testinput.ps1` loads `lib.ps1` and replaces `Launch-Brouillon`, `Send-Chord`, `Send-Text`, `Send-Click` (also
`-Double`), `Send-Drag`, `Save-Shot`, `Get-RegionHash`, `Assert-Focus`, `Set-AppFocus`; it adds `Send-Wheel`,
`Set-AppSize`, `Get-State`, `Wait-State`. Click coordinates keep the `lib.ps1` meaning (logical, from the OUTER frame's
top-left); the client converts them. Screenshots are the client area only (no title bar), so they are a few dozen
pixels shorter than the real-input ones.

```powershell
. (Join-Path $PSScriptRoot 'testinput.ps1')
$app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'demo'
Send-Chord $app 'ctrl+k'
$s = Wait-State $app { param($s) $s.state.palette_open }
Send-Text $app 'About'
Save-Shot $app 'palette' | Out-Null
```

## What does NOT behave like real input

* **No OS events.** The events enter at floem's window handle, the place where winit's `WindowEvent`s become floem
  events. Anything that listens below it is bypassed: Win32 message hooks, `GetKeyState`/`GetAsyncKeyState`, the real
  cursor position, `SetCapture`, cursor shape, drag-and-drop from other applications.
* **No IME.** No composition, preedit or candidate window. Text arrives as key presses with text, as typing on a plain
  layout does. Use the real backend (`IME_TEST.md`) for IME work.
* **No key auto-repeat** and no layout dependence: letters are delivered as `Character` plus the US physical code, so an
  AZERTY-specific bug (digits need Shift) is not reproduced. Dead keys and AltGr are not synthesised.
* **Pointer capture.** A drag is a press, N moves and a release inside the window, delivered back to back. Floem's own
  active-view capture works (it lives inside floem); the OS capture that keeps events flowing outside the window does not.
* **Hit testing and focus** are floem's own, so they behave as for real input; the OS focus is untouched, so the title
  bar looks inactive, `WindowGotFocus` is not sent, and code asking "is my window the foreground" sees `false`.
* **Timing.** Events are injected faster than a human types; use `Wait-State` instead of fixed sleeps.
* **Capture needs the CPU rasteriser.** `PrintWindow` returns a blank image for a GPU swap chain, so the client sets
  `FLOEM_FORCE_TINY_SKIA=1` (pass `-Gpu` to opt out). A window moved off-screen is not painted at all, hence the
  invisible-but-on-screen approach. A minimised window cannot be captured (the channel says so). Commands go to the first window only.
* The test build swaps winit's `KeyEvent` for a field-identical struct inside floem (`keyboard::KeyData`), because
  winit's cannot be built outside winit. The app only reads the public fields, so behaviour is the same.

## Quit with unsaved hex edits

Text tabs are journaled on exit; hex byte edits are not, so Ctrl+Q, `File: Quit` and the window close button first show
a Save / Don't save / Cancel box when any hex session is dirty (Cancel keeps the app open, Save writes then quits).
The window-close veto is a small floem patch (`set_close_guard`, see `vendor/floem/PATCH.md`).
`tools/smoke/hex.ps1` scenarios `hex-quit-ctrl+q` and `hex-quit-wm_close` drive it: they dirty a byte, trigger the quit
(Ctrl+Q through the channel, `WM_CLOSE` posted to the window), find the native box by its title and answer it with
`WM_COMMAND` (IDCANCEL, IDYES, IDNO), then check the process state and the bytes on disk. The channel's own `quit` op
bypasses the guard on purpose. Saving a 100 MB file takes about 0.46 s (dev profile,
`cargo test -p brouillon-ui save_100mb_timing -- --ignored --nocapture`), so the save stays on the UI thread.

## Verification scenarios (`tools/smoke/phasepass.ps1`, test-input backend)

Run with a DEV exe built with `--features test-input`: `phasepass.ps1 -Exe <exe> -Only nav,nav-tabs,switcher,reopen,project,...`.
Besides `key`, `text` and `click`, the channel has `hold` / `tap` / `release` (client helpers `Send-Hold`, `Send-Tap`,
`Send-Release`): `hold` presses the modifier keys and leaves them down, `tap` presses one key while they are held, `release`
lets go. They drive "hold Ctrl, press Tab twice, release Ctrl" (the Ctrl+Tab document popup).

Scenarios added for the navigation, switcher, encoding and project work: `nav` (goto, Alt+Left twice, Alt+Right twice),
`nav-tabs` (tab switch and a Ctrl+P `@symbol` jump), `switcher` (popup order, commit on Ctrl release, Esc cancels),
`reopen` (picker preview, Reopen with Encoding re-reads the file), `project` (Save Project, `--project`).

Retired or fixed old scripts: `shortcuts-ui.ps1` is retired (it clicked stale pixel rows and landed on Open Folder, a native
dialog the invisible window cannot answer): use `phasepass.ps1 -Only shortcuts,shortcut-record`. The `tree` scenario of
`phase3.ps1` is retired (stale rows, and the per-workspace state it expected is not written): use `phasepass.ps1 -Only tree`.
`multicursor.ps1` now defaults to the sidebar layout (`-TextX0 312`). The Save As branch of `external-change.ps1` needs the
native Save As dialog and cannot be driven by this channel.
