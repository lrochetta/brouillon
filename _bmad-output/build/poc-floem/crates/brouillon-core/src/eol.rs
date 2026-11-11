//! Line-ending kinds, scan and conversion (BK-007 / BK-010).
//!
//! Distinct from the legacy two-state `encoding::EolMode`, which stays for the
//! existing callers; `EolKind` adds bare CR (classic Mac).

use crate::contract::{EditSet, TextEdit};
use crate::encoding::EolMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EolKind {
    Lf,
    Crlf,
    Cr,
}

impl EolKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
            Self::Cr => "CR",
        }
    }
    pub fn separator(&self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
            Self::Cr => "\r",
        }
    }
    /// Default for new files when no setting overrides it.
    pub fn platform_default() -> Self {
        if cfg!(windows) {
            Self::Crlf
        } else {
            Self::Lf
        }
    }
}

impl From<EolMode> for EolKind {
    fn from(m: EolMode) -> Self {
        match m {
            EolMode::Lf => Self::Lf,
            EolMode::Crlf => Self::Crlf,
        }
    }
}

/// What to do about NUL bytes / stray CR characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrayPolicy {
    #[default]
    Ignore,
    Warn,
    /// Convert stray CR to the dominant EOL (NUL cannot be converted; it warns).
    Convert,
}

/// Counts of each line-ending kind in a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EolScan {
    pub lf: usize,
    pub crlf: usize,
    pub cr: usize,
    /// NUL characters.
    pub nul: usize,
}

impl EolScan {
    pub fn kinds_present(&self) -> usize {
        [self.lf, self.crlf, self.cr].iter().filter(|&&n| n > 0).count()
    }
    pub fn is_mixed(&self) -> bool {
        self.kinds_present() > 1
    }
    /// Most frequent kind (ties: CRLF, LF, CR). `None` = no line break at all.
    pub fn dominant(&self) -> Option<EolKind> {
        if self.kinds_present() == 0 {
            return None;
        }
        let mut best = (EolKind::Crlf, self.crlf);
        for (k, n) in [(EolKind::Lf, self.lf), (EolKind::Cr, self.cr)] {
            if n > best.1 {
                best = (k, n);
            }
        }
        Some(best.0)
    }
}

/// Scan `text` once.
pub fn scan(text: &str) -> EolScan {
    let b = text.as_bytes();
    let mut s = EolScan::default();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => {
                s.crlf += 1;
                i += 1;
            }
            b'\r' => s.cr += 1,
            b'\n' => s.lf += 1,
            0 => s.nul += 1,
            _ => {}
        }
        i += 1;
    }
    s
}

/// One edit per line break that is not already `kind`; applying the set
/// converts the whole text (a single undo group for Lot 1).
pub fn convert_edits(text: &str, kind: EolKind) -> EditSet {
    let b = text.as_bytes();
    let want = kind.separator();
    let mut edits = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let (len, cur) = match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => (2, "\r\n"),
            b'\r' => (1, "\r"),
            b'\n' => (1, "\n"),
            _ => {
                i += 1;
                continue;
            }
        };
        if cur != want {
            edits.push(TextEdit::new(i, i + len, want));
        }
        i += len;
    }
    EditSet(edits)
}

/// Converted copy of `text`.
pub fn convert(text: &str, kind: EolKind) -> String {
    convert_edits(text, kind).apply_to_str(text).unwrap_or_else(|_| text.to_string())
}

/// Edits for the stray-CR / NUL policy; second value is a warning to show.
pub fn apply_stray_policy(text: &str, policy: StrayPolicy) -> (EditSet, Option<String>) {
    let s = scan(text);
    let dominant = s.dominant().unwrap_or(EolKind::Lf);
    let stray = if dominant == EolKind::Cr { s.lf } else { s.cr };
    let mut parts = Vec::new();
    if stray > 0 && policy != StrayPolicy::Ignore {
        parts.push(format!("{stray} stray {} line break(s)", if dominant == EolKind::Cr { "LF" } else { "CR" }));
    }
    if s.nul > 0 && policy != StrayPolicy::Ignore {
        parts.push(format!("{} NUL character(s)", s.nul));
    }
    let warning = (!parts.is_empty()).then(|| format!("File contains {}", parts.join(" and ")));
    let edits = if policy == StrayPolicy::Convert && stray > 0 { convert_edits(text, dominant) } else { EditSet::new() };
    (edits, warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = "a\nb\r\nc\rd\r\ne\n";

    #[test]
    fn scan_counts_and_dominant() {
        let s = scan(MIXED);
        assert_eq!((s.lf, s.crlf, s.cr), (2, 2, 1));
        assert!(s.is_mixed());
        assert_eq!(s.dominant(), Some(EolKind::Crlf));
        assert_eq!(scan("no breaks").dominant(), None);
        assert!(!scan("a\r\nb\r\n").is_mixed());
        assert_eq!(scan("a\0b").nul, 1);
    }

    #[test]
    fn mixed_to_crlf_is_pure_crlf_and_reversible_by_edits() {
        let out = convert(MIXED, EolKind::Crlf);
        assert_eq!(out, "a\r\nb\r\nc\r\nd\r\ne\r\n");
        let s = scan(&out);
        assert_eq!((s.lf, s.cr), (0, 0));
        // Undo = apply the inverse conversion info held by the original text.
        assert_eq!(convert(&out, EolKind::Lf), "a\nb\nc\nd\ne\n");
    }

    #[test]
    fn already_target_yields_no_edits() {
        assert!(convert_edits("a\r\nb\r\n", EolKind::Crlf).is_empty());
        assert_eq!(convert("a\rb", EolKind::Lf), "a\nb");
        assert_eq!(convert("a\nb", EolKind::Cr), "a\rb");
    }

    #[test]
    fn stray_policy() {
        let t = "a\r\nb\rc\r\n\0";
        let (e, w) = apply_stray_policy(t, StrayPolicy::Ignore);
        assert!(e.is_empty() && w.is_none());
        let (e, w) = apply_stray_policy(t, StrayPolicy::Warn);
        assert!(e.is_empty());
        assert!(w.unwrap().contains("1 stray CR"));
        let (e, _) = apply_stray_policy(t, StrayPolicy::Convert);
        assert_eq!(e.apply_to_str(t).unwrap(), "a\r\nb\r\nc\r\n\0");
    }
}
