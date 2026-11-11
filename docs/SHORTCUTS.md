# Brouillon — Shortcuts Reference

> Every key binding shipped in v0.1.0-alpha.4, on one page.

This is the single-page reference. It is intentionally short: Brouillon
ships a small, deliberate keymap rather than a five-hundred-line list of
commands you will never use. Each row records the action, the binding
that triggers it on Windows and Linux (Cmd substitutes for Ctrl on
macOS), and the release in which that binding became real rather than
scaffolded. "alpha.0" rows are day-one primitives that have been live
since the first internal build; rows tagged "state only" toggle
internal state today and gain their UI surface in a later alpha.

## Bindings

| Action                              | Shortcut                          | Status                                          |
|-------------------------------------|-----------------------------------|-------------------------------------------------|
| Open file                           | Ctrl-O                            | alpha.4 (real load with encoding auto-detect)   |
| Save / Save As                      | Ctrl-S                            | alpha.4 (real save with EOL normalization)      |
| Find                                | Ctrl-F                            | alpha.1                                         |
| Replace                             | Ctrl-H                            | alpha.1                                         |
| Replace All                         | (button in overlay)               | alpha.4 (actually commits)                      |
| Close Overlay / Dialog              | Escape                            | alpha.1                                         |
| Cycle theme                         | Ctrl-T                            | alpha.3                                         |
| Toggle Moldu / Geek                 | Ctrl-Shift-M                      | alpha.3                                         |
| Command palette                     | Ctrl-K                            | alpha.3 (interactive floating panel)            |
| Go-to-line                          | Ctrl-G                            | alpha.11                                        |
| Undo                                | Ctrl-Z                            | alpha.12 (standard) / alpha.15 (Crayon paths)   |
| Redo                                | Ctrl-Y / Ctrl-Shift-Z             | alpha.12 (standard) / alpha.15 (Crayon paths)   |
| Toggle Clavier / Crayon (Sketchpad) | Ctrl-Shift-D                      | alpha.15                                        |
| Cycle bracket matching style        | Ctrl-Shift-P                      | alpha.14                                        |
| New Tab / Scratch                   | Ctrl-N                            | alpha.9                                         |
| Close Tab                           | Ctrl-W                            | alpha.9                                         |
| Next Tab                            | Ctrl-PgDn                         | alpha.9                                         |
| Previous Tab                        | Ctrl-PgUp                         | alpha.9                                         |
| Cursor: Up / Down / Left / Right    | arrow keys                        | alpha.0                                         |
| Cursor: Home / End                  | Home / End                        | alpha.0                                         |
| Cursor: PageUp / PageDown           | PageUp / PageDown                 | alpha.0                                         |
| Select text / drag                  | Mouse click + drag / arrow keys   | alpha.14                                        |
| Column / Block selection            | Alt + Mouse left-click drag       | alpha.14                                        |
| Clipboard history menu              | Ctrl-Shift-V                      | alpha.14                                        |
| Inspector                           | F11                               | day-1                                           |

## Moldu vs Geek and the keymap

Brouillon ships two UX presets — **Moldu** (GUI-first) and **Geek**
(keyboard-first) — and you flip between them with **Ctrl-Shift-M**
(Cmd-Shift-M on macOS) at any time, mid-buffer, without restarting. The
active preset changes which actions are *surfaced* — the command palette
filters its visible entries by preset, and Moldu shows menu chrome that
Geek hides — but the bindings themselves are identical across both
modes in alpha.4. A Ctrl-S in Moldu and a Ctrl-S in Geek run the same
code path; the only difference is whether the Save action is also
reachable through a visible menu item.

Per-preset binding overrides land in alpha.5 alongside the command
palette overlay. Until then, treat the table above as the complete
binding surface: it is the same on Windows, macOS, and Linux, the same
in Moldu and Geek, and the same regardless of which theme is active.
The shortcuts that are not yet on the table — multi-cursor, undo,
redo, tab cycling — are scaffolded in the buffer model but not bound
in alpha.4, and will appear here the day they ship.
