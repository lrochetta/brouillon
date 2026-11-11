# IME / CJK manual test plan

The PoC ships **no automation** for input-method-editor testing — these
checks must be done by hand on real hardware. This document is the
checklist + pass/fail criteria.

## Why this matters

IME is the single biggest UX risk for a cross-platform Rust editor
written on top of `winit`. Pre-edit composition windows, candidate
window anchoring, and event-loop integration are all platform-specific
glue that frequently regress in `winit`/`floem` releases. We test
**before** locking in the framework choice for day 2–5.

## Acceptance criteria (per-platform)

For each platform/IME combo:

1. Open `poc-editor` with the default sample file.
2. Focus the editor body (click anywhere in the text area).
3. Switch to the listed IME and type the listed phrase.
4. Verify all checks pass.
5. Take a screenshot. File at `IME_results/<os>-<ime>-<status>.png`.

| Check | Pass criterion (precise — pass requires ALL three: visible, correctly placed, no console errors) |
|-------|----------------|
| C1: Pre-edit text visible | Composition characters with the platform's standard underline (solid on Win11, dotted on macOS, depends on engine on Linux) appear **inline at the caret position** — not in a floating bubble at screen origin. Pre-edit must update on every keystroke without flicker. |
| C2: Candidate window anchored | The IME's candidate popup is rendered within **8 device-independent pixels** below (or above, if near screen bottom) the active caret. Anchoring must hold when the window moves, resizes, or crosses monitor boundaries. **Fail** if the popup appears at screen origin (0,0) or sticks to the previous caret position after the caret moves. |
| C3: Candidate selection works | Number keys (1–9) and arrow keys (↑/↓) cycle the candidate list ; Enter or the active number commits the highlighted candidate ; Space commits the first candidate (where the IME conventionally maps Space→commit, e.g. Japanese). |
| C4: Committed text inserted | After commit, the characters appear in the buffer as normal text — **no underline, no preedit ghost, no doubled characters, no zero-width joiners left over.** The cursor advances by the commit's grapheme count. |
| C5: Backspace deletes commits | Backspace removes the committed sequence **one user-perceived character (grapheme) at a time** — not one UTF-8 byte at a time. Composed Hangul syllables count as one grapheme. |
| C6: ESC cancels pre-edit | ESC during composition drops the pre-edit text without committing. The buffer must be byte-identical to the pre-composition state. |

> **Day-1 caveat**: the PoC view is read-only — committed characters
> will NOT appear in the text body because there is no edit pipeline
> wired yet. For day-1 IME testing, the goal is to verify the **OS-level
> IME UI** integrates correctly (pre-edit window opens, candidates show
> at the caret). Day-3 will add insert + this test becomes end-to-end.

## Test matrix

### Windows 11 (24H2 / 25H2)

| IME              | Phrase to type            | Expected output    | Status |
|------------------|---------------------------|--------------------|--------|
| Microsoft IME — Japanese (Hiragana) | `henkan` → space → enter | へんかん → 変換 | ☐ |
| Microsoft IME — Korean (2-set / 두벌식) | `gksrnreh`             | 한국어             | ☐ |
| Microsoft IME — Chinese Simplified (Pinyin) | `nihao` → 1     | 你好               | ☐ |

Notes:
- The "Microsoft IME" name is shared across languages on Win11 24H2+
  (legacy 2020-era names like "Microsoft Japanese IME" are no longer
  displayed in `Settings → Time & language → Typing`). Select the IME
  via the language bar (`Win+Space`) ; do NOT rely on the old name in
  scripts.
- Test on a fresh user session — Windows IME state is sticky and a stale
  candidate window from a previous app can mislead.
- Test both windowed and maximized — candidate anchoring sometimes breaks
  only when the window crosses monitor boundaries.
- Win11 has both a "new" and a "previous version" IME mode (per-language,
  toggled in IME settings). Test the **new** mode by default ; only fall
  back to legacy if the new mode regresses on `floem-winit`.

### macOS (Tahoe / macOS 26+, also Sonoma 14+ and Sequoia 15+)

| IME              | Phrase to type   | Expected output   | Status |
|------------------|------------------|-------------------|--------|
| 日本語 - かな    | `henkan` + Space | 変換              | ☐ |
| 한국어 - 2-Set   | `gksrnreh`       | 한국어            | ☐ |
| 简体中文 - 拼音  | `nihao` + 1      | 你好              | ☐ |

Notes:
- Test on a Retina display (most users) — the candidate window position
  bug, when it exists, is usually a DPI scaling issue.
- Test with the macOS character picker (Ctrl+Cmd+Space) as a sanity check
  that text input works at all.
- On macOS 26 (Tahoe), the system IME pipeline switched to the new
  `UIIndirectInputView` path on AppKit ; if pre-edit fails only on 26+ and
  works on 15.x, suspect a `floem-winit` macOS-26 regression and bisect
  there first.

### Linux (Wayland + X11)

| Stack                   | Phrase to type             | Expected output | Status |
|-------------------------|----------------------------|-----------------|--------|
| fcitx5 + mozc (Wayland) | `henkan` + space           | 変換            | ☐ |
| fcitx5 + mozc (X11)     | same                       | 変換            | ☐ |
| ibus + hangul (Wayland) | `gksrnreh`                 | 한국어          | ☐ |
| ibus + libpinyin (X11)  | `nihao` + 1                | 你好            | ☐ |

Notes:
- Set `XMODIFIERS=@im=fcitx` (or `ibus`) before launching. Floem/winit
  reads it at startup; mid-run changes are NOT picked up.
- Wayland IME support in winit has historically lagged X11. Test
  Wayland first — if it works, X11 almost certainly does too. If only
  X11 works, file a winit issue and document the workaround.

## Regression tracker template

When a test fails, log it here:

```
- 2026-MM-DD — Win11 — Microsoft Japanese — C2 fail (candidate window
  appears at top-left of screen, not anchored to caret). Reproduces
  100%. Probable root cause: winit `Ime::Preedit` cursor position not
  forwarded. Filed upstream: <issue link>. Workaround: <if any>.
```

## Accessibility cross-check (dyslexia-friendly fonts)

When the editor is configured with an OpenDyslexic, Atkinson Hyperlegible, or Lexend font family, re-run **C1 only** on each platform to confirm the IME pre-edit underline still aligns correctly to the variable-stem baseline (these fonts have unusual descender geometry and have historically broken `cosmic-text`/`swash` baseline math). One pass per OS is enough — not the full 3-IME matrix.

## What "go" looks like

The PoC clears IME validation when **at least one IME per platform**
passes C1–C6 (excluding the day-1 commit caveat). Failing combos must
have an upstream issue filed and a documented workaround path before
we lock in Floem for the rest of the build.
