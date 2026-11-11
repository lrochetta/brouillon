# Brouillon screenshots (real captures, not retouched)

All images are genuine captures of the Brouillon window (window rectangle only), taken with the smoke harness (`_bmad-output/build/poc-floem/tools/smoke/lib.ps1`) on a throwaway settings folder and synthetic demo files (Rust, PHP+HTML, C, Python, SQL, JSON, shell) written for this purpose.

- Executable: `brouillon.exe` **0.1.0-alpha.13, build 110** (2026-11-05T09:06:49Z). Version and build are visible in the window title on purpose.
- Environment: Windows 11, display scale 175 %, CPU renderer (default), theme **Tokyo Night** unless stated, captured 2026-11-05.
- Full-size files: native pixels (1917x1302 for window shots), PNG 24-bit, no scaling. `*-1280.png`: 1280 px wide bicubic downscale, quantised to PNG-8 (256 colours, no dithering), all under 90 KB.
- Only a 6 px margin was cropped (window border, rounded corners). No pixel was edited.
- No personal data: demo folder was `D:\demo-project` (deleted content), no user names, no notifications.
- Naming differs from the art direction (`<id>-<dark|light>`): here `editor-dark` / `editor-light` are the two theme variants of the editor scene; other scenes were captured in dark only.

| File | Shows | Alt EN | Alt FR |
|---|---|---|---|
| editor-dark | Rust file with highlighting, tabs (PHP + Rust), side list, status bar; Tokyo Night | Brouillon editing a Rust file with syntax highlighting in a dark theme | Brouillon édite un fichier Rust avec coloration syntaxique, thème sombre |
| editor-light | Same scene in **GitHub Light** (View > Cycle Theme, 5 presses from default) | The same Rust file in the light theme | Le même fichier Rust en thème clair |
| find-replace | Find/Replace bar, `word` -> `term`, 8 matches counted | Find and replace bar over a Rust file | Barre de recherche et remplacement sur un fichier Rust |
| palette | Command palette (Ctrl+K), unfiltered, with shortcuts | Command palette listing file and tab commands | Palette de commandes listant les commandes fichier et onglet |
| crayon | Pencil mode: pen tools, colours, black wave, red open arc, blue wave and tick | Pencil mode with a few hand-drawn strokes | Mode crayon avec quelques traits dessinés |
| status-bar | Status bar zoom (1917x46): Ln 11, Col 46; 29 lines; LF; Unicode (UTF-8); INS; size; RAM; mode; file; language Rust | Status bar showing position, line ending, encoding and language | Barre d'état : position, fin de ligne, encodage, langage |
| tabs-sidebar | Five tabs (C, Python, JSON, PHP, Rust) and the "Open files" list | Five open tabs and the open files list | Cinq onglets ouverts et liste latérale |
| find-in-files | Find in Files for `print`: 3 hits in 3 files, grouped and foldable | Find in Files results grouped by file | Résultats de recherche dans les fichiers, groupés |
| syntax-grid | 6 tiles of editor body only, row 1: C, PHP+HTML, SQL; row 2: Python, JSON, shell | Six languages highlighted | Six langages colorés |

## Honest limits and visible known issues (not hidden)

- Build 110 is an alpha; later builds may differ.
- The header row shows a debug-looking frame counter ("-- fps" or a value) beside the theme name.
- Light theme is incomplete: the tab strip stays dark, status bar text is low contrast. Status bar text on the left is dim in every theme.
- Find bar: counter reads `?/8` until a match is navigated and matches are not highlighted in the text; focused replace field text is hard to read. Pressing Enter in the Find field inserted a blank line into the document (seen twice; scenes avoid it).
- Find in Files: the results panel is only about 4 lines tall in this window and the bottom is cut. It runs on the Find bar query (Ctrl+F, type, Ctrl+Shift+F).
- Pencil mode: toolbar labels are French (Stylo, Taille, Convertir la Page (IA)) while the rest is English; the tool row is clipped at the right; the header row is shifted/clipped by about 10 px in this mode and in Find in Files. In an exploratory run a closed circle was snapped into a square by the app; the published shot uses open strokes.
- Palette: a filtered list (query `tab`) exposes an importer entry; the published shot is unfiltered.
- Ctrl+PgUp left tab strip and editor out of sync in one run (strip said index.php, editor showed main.rs); scenes avoid it by opening files in the wanted order.
- Startup render flakiness: in 2 of 3 launches of the shell file, line 1 showed stray glyphs from a stale paint, and one capture was blank. The published tile is a clean launch; the bug exists.
- syntax-grid tiles are cut after about 16 lines (C, PHP, Python end mid-file).
- The autocomplete popup was not triggered in any scene; its position (wrong in earlier builds) was not verified.
- Windows 11 rounded corners were cropped, not filled; no window shadow is baked in.
