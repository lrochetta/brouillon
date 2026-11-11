# Welcome to Brouillon

Version {{version}}, build {{build}} ({{commit}}), built {{date}}.

Brouillon is a fast text editor for Windows, written in Rust. It starts quickly, works on your machine only, has no account and sends no data anywhere.

> This is an alpha version. It works and it is being tested, but it is unfinished: keep a copy of anything important, and read "What is not there yet" below.

## Five things to try

- Open and save: Ctrl+O opens a file, Ctrl+N starts a new tab, Ctrl+S saves. A tab that has no file yet asks for a name.
- Find and replace: Ctrl+F searches the current file, Ctrl+H replaces. Replace All is a single step: one Ctrl+Z puts everything back.
- Undo: Ctrl+Z undoes, Ctrl+Y redoes.
- Every command: Ctrl+K opens the command palette. Type a few letters (for example "upper") and each match shows its shortcut.
- Help: the Help menu, or the palette, reopens this page (Welcome), the Documentation and the Release notes at any time.

This page is a read-only Markdown document. Ctrl+Shift+V shows or hides the preview next to the text.

## Your settings and your data

Everything Brouillon remembers is in one folder:

```
{{data_dir}}
```

On Windows that is the folder .brouillon in your user profile. If a file named brouillon.portable sits next to brouillon.exe, the folder is called data and lives beside the program instead.

Options go in a plain text file named settings.toml in that folder:

```
{{settings_file}}
```

The file does not exist until you create it, and it is read once at start, so restart Brouillon after editing it. A mistake in it never prevents the editor from starting. There is no settings window yet. The Documentation lists every option that works today.

## What is not there yet

- a settings window and a shortcut editor (you edit settings.toml and keymap.toml by hand),
- Quick Open, multi-cursor editing, a Replace in Files dialog, printing and macros,
- detection of a file that another program changed while it is open,
- language servers and plugins.

The Documentation, section "Troubleshooting and known problems", lists real bugs that are known and not fixed.

## Report a bug

Open the Documentation, copy the first lines of the Troubleshooting section into your report, and describe what you did, what you expected and what happened. A small file that reproduces it helps a lot. Never attach files that hold secrets.

The project site is https://brouillon.rochetta.fr. The link to the public repository and its issue tracker is published there; no support e-mail address is published.

## About

Brouillon is free software under the MIT licence. Copyright 2026 Laurent Rochetta.

Website: https://brouillon.rochetta.fr
