# Language definitions

One TOML file per language. The files in this folder are compiled into the
binary (`include_str!`); users can add or replace languages by dropping
`*.toml` files into `<config dir>/syntax/` (`Paths::syntax_dir()`, e.g.
`~/.brouillon/syntax/`). A user file wins over a shipped one: same `name`
replaces it, and a user language is tried first when detecting by extension or
file name. A broken user file is reported (`file:line: message`, see
`Registry::errors()`) and skipped; everything else keeps working.

## Adding a language

1. Create `mylang.toml` (user folder, or here + one line in `registry.rs`).
2. Give it a `name` and at least `extensions`.
3. Describe comments, strings and keyword groups (below).
4. Reload (`syntax::reload_user_dir`) or restart.

Minimal example:

```toml
name = "MiniLang"
extensions = ["mini"]
line_comments = ["--"]
block_comments = [["(*", "*)"]]
case_sensitive = false

[[string]]
open = '"'
escape = '\'

[[keywords]]
name = "Reserved"
words = "begin end if then else"      # whitespace separated, or a TOML array

[[keywords]]
name = "Types"
kind = "type"
words = "int text"
```

## Top-level keys

| key | meaning |
|---|---|
| `name` | display name (status bar). Required. |
| `extends` | start from another definition (by name): lists and options not set here are inherited, keyword groups with the same `name` are merged (words appended), other groups are added. A user file may `extends` its own name to extend the shipped definition. `extensions`, `filenames`, `shebang`, `first_line` are never inherited. |
| `extensions` | file extensions without dot, case-insensitive. |
| `filenames` | exact names or globs (`Dockerfile`, `Dockerfile.*`), case-insensitive. Checked before extensions. |
| `shebang` | interpreter names matched against `#!/usr/bin/env python3` / `#!/bin/bash` (`python` matches `python3.11`). |
| `first_line` | lower-case prefixes of the first line (`<?xml`, `<!doctype html`). Last resort after name and extension. |
| `case_sensitive` | keyword matching (default `true`). Insensitive languages also match comment tokens case-insensitively. |
| `word_chars` | extra identifier characters besides letters, digits, `_` and non-ASCII (e.g. `-` for CSS, `$` for JS, `?!` for Ruby). |
| `number` | `c` (default: dec, `0x`, `0b`, `0o`, `_`, float, exponent, suffixes), `css` (number + unit, signed), `none`. |
| `operators` | characters coloured as `operator`. Leave empty to colour none. |
| `functions` | `true`: an identifier followed by `(` and not a keyword is `function`. |
| `regex_literals` | `true`: `/re/flags` is a string when a regex can start there (JS style). |
| `line_comments` | tokens starting a comment to end of line. |
| `line_comments_col0` | same, but only at column 0 (INI `;`). |
| `block_comments` | `[open, close]` pairs; multi-line, state is carried across lines. `nested_comments = true` makes them nest. |
| `heredoc` | leaders (`<<`, `<<-`, `<<~`, `<<<`). The identifier after the leader ends the body (alone on its line; `-`, `~`, `<<<` allow indentation). `<<` needs an upper-case or quoted identifier so that shifts are left alone. |
| `host` | name of a markup language; the document is that language with this one embedded in `[script]` regions (PHP -> HTML). |

## Tables

`[[string]]` — string delimiters (longest `open` is tried first)
- `open`, `close` (default = `open`), `escape` (one char), `multiline` (default `false`: an unterminated string ends at end of line, unless the line ends with the escape character), `doubled` (SQL/VB `''`), `check_closing` (only a string when the closing delimiter exists on the line), `kind` (default `string`), `escapes` (colour escape sequences as `escape`; default `true` when `escape` is set).

`[[keywords]]` — keyword groups, in order (max 10)
- `name`, `words` (array or whitespace separated string), `kind`. Without `kind`, group *n* is `keyword1..keyword4`, groups 5 to 10 share `keyword5` (the palette has five keyword colours). The first group containing a word wins.

`[[prefix]]` — a word directly after one of `chars` is coloured as a whole (`$var`, `@Annotation`, `:sym`, `%VAR`)
- `chars`, `kind` (default `variable`), `braced` (`${...}`), `parens` (`$(...)` and `$@ $< $^`).

`[[line_start]]` — a line starting at column 0 with `prefix` is coloured entirely as `kind` (Markdown headings, diff lines, batch labels). First match wins.

`[preprocessor]` — `start` (e.g. `#`) at the start of a line colours `#word`; `angle_include = ["include"]` colours the following `<...>` as a string.

`[keys]` — `separators` (`":"`, `"="`): a word or (`strings = true`) a string followed by a separator is coloured `kind` (default `attribute`); `line_start_only` limits it to the first token of the line.

`[script]` — `begin = ["<?php", "<?="]`, `end = "?>"`, for languages with a `host`.

`[markup]` — HTML/XML mode: tags, attributes, entities, comments, `<![CDATA[`. `pi = true` colours `<?...?>`. `raw_text = [{ tag = "script", lang = "JavaScript" }]` lexes the body of those elements with another language until the closing tag.

## Kinds

`keyword1`..`keyword5`, `string`, `comment`, `type`, `number`, `punct`, `identifier`, `preprocessor` (or `directive`), `operator`, `function`, `tag`, `attribute`, `escape` (or `special`), `variable`. Each one maps to a theme colour (`syn_*`).

## Notes

- Keyword lists are plain facts about each language; write your own.
- Definitions are data: no code runs. Unknown keys are errors (typos are caught), reported with their line.
- Limits: lexing is per line with carried state; there is no real parser, so a few constructs are approximations (JS regex vs division, Perl `s///`, string interpolation).
