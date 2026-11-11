# Brouillon documentation

Version {{version}}, build {{build}} ({{commit}}), built {{date}}. This page describes what the program does today; it was written from the program's own code and test reports, and it says so when something is missing.

## Getting started

- Ctrl+N new tab, Ctrl+O open, Ctrl+S save, Ctrl+Shift+S save as, Ctrl+W close the tab, Ctrl+Q quit.
- Opening a file adds a tab, or switches to it if it is already open. It never overwrites the current tab.
- Closing one tab that has unsaved changes asks first. Closing the whole window does not: the unsaved tabs are kept and come back, still marked as modified, at the next start.
- Ctrl+K opens the command palette: every command, filtered as you type, with its shortcut.
- Ctrl+T cycles the six built-in themes. Ctrl+Shift+V shows a Markdown preview, Ctrl+Shift+H a hex view.
- The status bar shows line and column, number of lines, the character under the caret, the line ending, the encoding, the BOM, INS or OVR, and READ for a read-only document. Clicking a cell runs its command: go to line, change line ending, choose encoding, toggle overwrite.
- Ctrl+G goes to a line: type 120, or 120:5 for line and column, or @ followed by a byte offset.
- Help > Welcome, Documentation and Release notes (also in the palette) reopen the built-in pages. They are read-only Markdown pages: typing in them does nothing, and they are never saved in your session.

## Finding and replacing

- Ctrl+F opens the Find bar, Ctrl+H the same bar for replacing. Esc closes it. In the Find field, Enter goes to the next match and Shift+Enter to the previous one. F3 and Shift+F3 also go to the next and previous match.
- The bar has two switches, one for regular expressions and one for case sensitivity (there is no whole-word switch yet), and the buttons Replace and Replace All.
- Replace All is one step: a single Ctrl+Z restores the original text.
- The replacement is read as a template:

```
\1 to \9, $1 to $9, ${n} : a captured group
$& : the whole match
\t \n \r : tab, line feed, carriage return
\U ... \E or \L ... \E : upper or lower case until \E
\\ and $$ : a backslash, a dollar sign
```

- Regular expressions use Unicode. Look-around and back-references are not available in a pattern.
- Find in Files: Ctrl+Shift+F searches the folder of the current file, in the background, honouring .gitignore. Unsaved tabs are searched too. Results appear in a panel: click one, or press F4 and Shift+F4, to jump to it.
- Replace in Files exists in the engine but has no dialog yet, so it does nothing visible.

## Undo and redo

- Ctrl+Z undoes, Ctrl+Y or Ctrl+Shift+Z redoes, and the selection you had comes back too.
- Typing is grouped: a pause of more than a second ends a group. Replace All, each text command (change case, sort lines, and so on) and Enter with automatic indentation are one group each.
- History survives a save, so you can undo past it. The palette has "undo all" and "undo to last save".
- A tab keeps at most 1000 groups and 64 MB of history. History is not written to disk: closing the tab or quitting ends it.

## Encodings and line endings

- On opening, Brouillon checks for a byte-order mark first, then for valid UTF-8, then guesses among older code pages.
- The encoding, the BOM and the line endings (LF, CRLF or CR, or mixed) of each file are remembered and kept byte for byte when you save. A UTF-16 file with a BOM and CRLF line endings comes back identical.
- Click the line-ending cell of the status bar to cycle LF, CRLF and CR.
- Click the encoding cell to open the encoding list. Your choice sets the encoding used by the next save; the text on screen is not decoded again. This was seen working as a list; saving with a chosen encoding was not checked on screen.
- If a save would lose characters (an emoji in a Windows-1252 file, for example), Brouillon asks first. Answer No and nothing is written.
- Supported: Unicode (UTF-8, UTF-16 and UTF-32 in both byte orders), Western European (US-ASCII, ISO-8859-1, Windows-1252, ISO-8859-15, Mac Roman), Central, Baltic, Nordic and South-Eastern European, Cyrillic (including KOI8-R and KOI8-U), Greek, Turkish, Arabic, Hebrew, Thai, Vietnamese and East Asian (Shift_JIS, EUC-JP, ISO-2022-JP, EUC-KR, GBK, GB18030, Big5). EBCDIC and the DOS pages CP437, CP850 and CP852 are listed but cannot be converted yet.

## Syntax colouring

The language comes from the file name or extension; failing that, from the first line (a shebang such as #!/usr/bin/env python, or a first line like the HTML doctype or the PHP opening tag); otherwise the file is plain text. Colouring is updated while you type.

Thirty languages are coloured: Rust, and 29 defined by tables: Batch, C, C++, C#, CSS, Diff, Dockerfile, Go, HTML, INI, Java, JavaScript, JSON, Log, Lua, Makefile, Markdown, Perl, PHP, PowerShell, Python, Ruby, Shell, SQL, TOML, TypeScript, Visual Basic, XML and YAML.

Nine of them (C, PHP with HTML, HTML, SQL, Python, Rust, JSON, Markdown, Shell) were checked on screen with real colours; the others use the same engine and were checked by tests only.

To add or replace a language, put a .toml file in the folder named syntax inside your data folder and restart. A broken file is reported in the status bar and skipped; everything else keeps working. A minimal definition:

```
name = "MiniLang"
extensions = ["mini"]
line_comments = ["--"]

[[keywords]]
name = "Reserved"
words = "begin end if then else"
```

## User tools

A tool is a command you define once and run from the keyboard. Create tools.toml in your data folder:

```
[[tool]]
name = "Build"
command = "cargo"
args = ["build"]
slot = 1
```

- Ctrl+1 to Ctrl+9 and Ctrl+0 run the tool in that slot. On an AZERTY keyboard the physical top-row key is used, so Ctrl+& runs slot 1.
- Without a tools.toml, three Cargo tools are bound to slots 1 to 3.
- Output streams into a panel (Ctrl+` shows or hides it) with a Stop button. Lines that look like compiler errors are clickable, and F4 and Shift+F4 jump from error to error while the output panel is active.
- Other keys: cwd, shell, group (1 to 20), file_types, save_before (none, current or all), env.
- In args, cwd and env you can write variables. Values are quoted for the target shell, and one that cannot be quoted safely is refused:

```
${file} ${dir} ${filename} ${name} ${ext}
${line} ${col} ${word} ${selection} ${project}
${env:NAME} : an environment variable
${name:fallback} : a fallback value
```

- Not working yet: any output choice other than the panel (it is read but still goes to the panel), auto_run (read, never executed), and the variable prompt (there is no dialog, so the tool fails with a message).
- A tool runs with your rights, like any program you start. Only add commands you trust. Brouillon never runs a tool you did not bind.

## Crash recovery and backups

- Every 30 seconds, tabs with unsaved changes and non-empty untitled tabs are written to the journal folder of your data folder. After a crash or a forced stop, the next start reopens that text as modified tabs and says so in the status bar. Only the text comes back, not the undo history.
- After a normal exit the same journal keeps unsaved tabs, so they also come back modified; the status bar then says the documents were restored from your last session.
- When you save a file that already exists, Brouillon first copies the old version to the backup folder as name.ext.YYYYMMDD-HHMMSS.bak (UTC time), then writes the new one through a temporary file and an atomic rename. If anything fails, the original is untouched. Defaults: 10 copies per file, copies older than 30 days removed. To restore, copy the wanted .bak file out of the backup folder and rename it.
- Tabs, the active tab and caret positions are saved in session.toml and restored at start. Scroll position and folds are not restored.
- Crayon mode drawings are saved next to the text as file.sketch.json when you save; unsaved strokes make the tab modified and survive a restart.
- --no-session starts without restoring or saving the session and without recovery.

## Settings

Edit settings.toml in your data folder (see the Welcome page for its exact path), then restart. Options that take effect:

```
[startup]
document = "welcome"   # welcome, docs, changelog or none
whats_new = true       # open the release notes once after an update

[render]
backend = "cpu"        # cpu (default), gpu, dx12, vulkan, gl, metal

[backup]
enabled = true
max_count = 10         # copies kept per file, 0 = unlimited
max_age_days = 30      # 0 = unlimited

[keymap]
classic = false        # true = an alternative set of default shortcuts
```

- startup.document is the page shown when Brouillon starts with no file on the command line and no session to restore. The very first start always shows Welcome. With none, you get an empty tab.
- startup.whats_new: after the program is updated (a different version or build than the last start), the Release notes open once, as an extra tab. The last start is remembered in last-run.toml in the data folder. When you start Brouillon with a file, the notes wait for the next start without a file.
- render.backend: cpu is a software renderer that needs no graphics driver and also works in virtual machines; in one measurement on Windows an empty document used about 61 MB, against 152 MB with Direct3D 12. gpu uses Direct3D 12 on Windows (auto is accepted as another name for gpu). dx12, vulkan, gl and metal force one graphics backend. An unknown value falls back to cpu. If the environment variable FLOEM_FORCE_TINY_SKIA or WGPU_BACKEND is already set, it wins.
- backup.enabled turns the backup copies on or off; backup.max_count and backup.max_age_days limit how many copies are kept and for how long (see Crash recovery and backups).
- files.auto_reload_unmodified (default true): when another program changes a file that you have not edited, Brouillon reloads it and says so in the status bar. A file you have edited is never reloaded or overwritten silently: a bar offers Reload or Ignore, and Save asks Overwrite, Reload from disk or Save As first.
- keymap.classic adds an alternative set of default shortcuts under your own keymap.toml; yours always win.
- Other keys exist in the defaults (editor.tab_size, editor.word_wrap, the thresholds) but nothing reads them yet.
- A mistake in settings.toml is printed on standard error and the defaults are used; the editor still starts.

Files in your data folder: settings.toml, keymap.toml (your shortcuts), tools.toml, session.toml, last-run.toml, the folders journal, backup and syntax, and a small token file used by the single-instance feature.

Command line: brouillon [options] file[:line[:col]]. Options: --goto LINE[:COL], --new-window, --wait, --readonly, --no-session, --settings-dir DIR and --version (prints version, build, commit and date). A second start hands its files to the running window unless you pass --new-window.

## Keyboard shortcuts

These are the shortcuts bound to commands the editor really runs, read from the program at this moment (including your keymap.toml). Every other command is in the palette, Ctrl+K.
{{shortcuts}}
## Troubleshooting and known problems

When you report a problem, start with the line below and add your Windows version and display scaling.

Brouillon {{version}} (build {{build}}, {{commit}}, {{date}})

First checks:

- Run brouillon --version in a terminal and note the build.
- Start with --no-session to rule out a stale session.
- Start with --settings-dir pointing at an empty folder to rule out your settings, keymap and tools.
- Run it from a terminal: warnings about settings, keymap, syntax files and recovery are printed on standard error.
- If the window does not open or is blank, set backend to gpu or cpu (the other one) under [render] in settings.toml.

Known problems, as of this build:

- The Markdown preview is basic; it is meant for a quick look. Seen on screen: headings, lists, quotes and code blocks are drawn and long lines wrap, but bold, italic, inline code or a link inside a line split that line into several rows; an underscore inside a word (snake_case) or asterisks in a sum are read as italic marks; numbered lists are drawn with bullets; tables are shown as raw text; images are not drawn; links are plain text and cannot be clicked. These help pages are written to avoid those cases.
- The tab bar can show its active underline on two tabs after switching tabs; this is cosmetic.
- After a menu closes, the title of the menu stays highlighted.
- Scroll position and folds are not restored after a restart.
- If a file changed on disk after a crash, the journal text silently wins when it is recovered. Keep a backup of anything important.
- Closing the only open tab that has unsaved changes, and answering No, leaves it open.
- A drawing is compared with its saved version by its number of strokes: erasing one stroke and drawing another after a save looks unchanged.
- A file that another program changes while it is open in Brouillon is not detected. Keep one editor per file.
- Very large files: a 100 MB file of about a million lines opened in about 0.35 s and a keystroke took about 0.3 s to repaint in one measurement. Live colouring and word completion are switched off above 16 MB.
- Memory use: the goal of 80 MB is not met on every setup. The default software renderer measured about 61 MB for an empty document on one machine.
- Some commands are listed in the palette but do nothing yet: print, macros, load project, select line, select word, matching bracket, go to column or function, bookmarks list, paste column, insert time and long date, line numbers toggle, last visited tab.
- Not provided: SHA-1 text transform, a settings window, a shortcut editor, Quick Open, multi-cursor editing, back and forward navigation keys.
- Windows may warn "Windows protected your PC" when you run an installer or program that is not code-signed; this build is not signed. Check the SHA-256 fingerprint published on the project site before choosing "Run anyway".

Wrong characters in a file: Brouillon detects encodings, and a short file in an older code page can be ambiguous. Use the encoding cell of the status bar to pick the encoding for the next save, and tell us the file's origin and the expected encoding.

Site: https://brouillon.rochetta.fr
