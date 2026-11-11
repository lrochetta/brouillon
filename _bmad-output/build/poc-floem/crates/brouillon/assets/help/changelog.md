# Release notes

You are running Brouillon {{version}}, build {{build}} ({{commit}}), built {{date}}.

Brouillon is an alpha version. This page lists what changed in the most recent builds. Each entry was checked, either on screen in the running program or by an automated test; the Documentation lists what is still broken or missing.

## This build

- Welcome page, Documentation and these Release notes are built into the program. Welcome opens by itself the first time you start Brouillon. After an update, the Release notes open once. Help > Welcome, Documentation and Release Notes (and the palette) open them at any time.
- New setting startup.document (welcome, docs, changelog or none) chooses what opens when Brouillon starts with no file and no session to restore. New setting startup.whats_new = false turns the one-time release notes off.
- Brouillon no longer opens a generated 100,000-line sample document when it starts. That document is only produced with the development option --sample.
- A file name given on the command line that does not exist yet now opens as a new empty file, to be created when you save. Before, the 100,000-line sample was written to that path.

## Changed

- The software (CPU) renderer is now the default; render.backend = "gpu" selects Direct3D 12 on Windows. The CPU renderer needs no graphics driver and works in virtual machines; in one measurement an empty document used about 61 MB, against 152 MB with Direct3D 12.

## Added

- Syntax colouring for 30 languages: Rust and 29 defined by tables. The language is also recognised from a first line such as a shebang. You can add or replace languages with .toml files in the syntax folder of your data folder.
- The encoding list opens from the encoding cell of the status bar. A choice sets the encoding of the next save; the text on screen is not decoded again.
- Unsaved drawings of Crayon mode are kept: they make the tab modified, they are saved next to the text as file.sketch.json, and they come back after a restart.

## Fixed: your work is no longer lost

- Documents recovered after a crash, and unsaved tabs left open when you closed the window, now reopen marked as modified and stay in the journal until you save or close them. Before, they looked saved and could be dropped by the next autosave. The status bar says what was recovered or restored.
- A new untitled tab no longer takes the name of the previous file, which used to bring back a duplicate tab at the next start.
- Saves, the session file, the recovery journal and backups are written to a temporary file that is flushed and then renamed over the target, so a crash during a save cannot leave a truncated file.
- Closing a tab that only holds drawn strokes now asks to save them, and Save on a tab without a file asks for a file name instead of closing without saving.

## Fixed: typing and screen

- The space bar types a space. Before, it did nothing.
- Typed text is displayed. The editor column fills the window instead of being squeezed in the middle.
- In the Find field, Enter no longer types a newline into the document: Enter goes to the next match and Shift+Enter to the previous one.
- The auto-complete popup now appears under the caret, and Enter no longer replaces the word you typed with a suggestion: Tab accepts it.
- Menus close when you click outside them, press Esc or switch to another window, and their text is readable in every theme.
- The Find bar no longer hides the Crayon button of the toolbar.
- The palette, the go-to-line box and the Find field receive the keyboard when they open.
- Ctrl+Alt+letter shortcuts, such as Ctrl+Alt+A and Ctrl+Alt+S, now fire on an AZERTY keyboard with the left Alt key; the right Alt key (AltGr) still types characters such as the euro sign.
- Ctrl+1 to Ctrl+0 follow the physical top-row key, so Ctrl+& runs user tool 1 on an AZERTY keyboard.
- Typing in a very large file is fast again: in one measurement on a 100 MB file a keystroke went from more than 8 seconds to about 0.3 second, because live colouring and word completion are skipped above 16 MB.
