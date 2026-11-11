//! Status bar panes (BK-011): a pure view-model (`status_model`) and a thin
//! floem view over it. Clickable panes carry a command id the shell routes.

use brouillon_core::eol::EolKind;
use brouillon_editor_engine::remote::{ChipTone, RemoteDocState};
use floem::reactive::{RwSignal, SignalGet};
use floem::style::CursorStyle;
use floem::views::{dyn_stack, label, Decorators};
use floem::IntoView;

/// Everything the status bar needs to know about the active document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocStatus {
    /// 1-based caret line / column.
    pub line: usize,
    pub col: usize,
    pub total_lines: usize,
    /// Character under the caret (`None` at end of text).
    pub char_at_caret: Option<char>,
    pub eol: Option<EolKind>,
    pub eol_mixed: bool,
    /// Display name, e.g. "UTF-8".
    pub encoding: String,
    pub bom: bool,
    pub overwrite: bool,
    pub readonly: bool,
    pub large_file: bool,
    /// Show the char code in decimal instead of hex.
    pub decimal_char_code: bool,
    /// Origin / save state of a document that lives on a server (`None` for local files).
    pub remote: Option<RemoteDocState>,
}

impl Default for DocStatus {
    fn default() -> Self {
        Self {
            line: 1,
            col: 1,
            total_lines: 1,
            char_at_caret: None,
            eol: None,
            eol_mixed: false,
            encoding: "UTF-8".into(),
            bom: false,
            overwrite: false,
            readonly: false,
            large_file: false,
            decimal_char_code: false,
            remote: None,
        }
    }
}

/// One status-bar cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub id: &'static str,
    pub label: String,
    /// Command id run when the pane is clicked.
    pub command: Option<&'static str>,
    /// Colour hint (the label always says the same thing in words).
    pub tone: Option<ChipTone>,
}

fn pane(id: &'static str, label: impl Into<String>, command: Option<&'static str>) -> Pane {
    Pane { id, label: label.into(), command, tone: None }
}

/// Ordered panes for `doc`. Conditional panes (BOM, READ, LARGE) appear only when set.
pub fn status_model(doc: &DocStatus) -> Vec<Pane> {
    let mut v = vec![
        pane("pos", format!("Ln {}, Col {}", doc.line, doc.col), Some("goto_line")),
        pane("lines", format!("{} lines", doc.total_lines), None),
        pane("char", char_label(doc.char_at_caret, doc.decimal_char_code), None),
    ];
    let eol = match (doc.eol_mixed, doc.eol) {
        (true, _) => "Mixed EOL".to_string(),
        (false, Some(k)) => k.name().to_string(),
        (false, None) => EolKind::platform_default().name().to_string(),
    };
    v.push(pane("eol", eol, Some("eol_cycle")));
    v.push(pane("encoding", doc.encoding.clone(), Some("encoding_pick")));
    if let Some(r) = &doc.remote {
        let chip = r.chip();
        v.push(Pane { tone: Some(chip.tone), ..pane("remote", chip.label, Some("remote_status")) });
    }
    if doc.bom {
        v.push(pane("bom", "BOM", None));
    }
    v.push(pane("mode", if doc.overwrite { "OVR" } else { "INS" }, Some("toggle_overwrite")));
    if doc.readonly {
        v.push(pane("readonly", "READ", None));
    }
    if doc.large_file {
        v.push(pane("large", "LARGE FILE", None));
    }
    v
}

fn char_label(c: Option<char>, decimal: bool) -> String {
    match c {
        None => "--".to_string(),
        Some(c) if decimal => format!("{}", c as u32),
        Some(c) => format!("U+{:04X}", c as u32),
    }
}

/// Floem view over a panes signal; `on_command` receives the clicked pane's command id.
pub fn status_bar_view(
    panes: RwSignal<Vec<Pane>>,
    theme_idx: RwSignal<usize>,
    on_command: impl Fn(&'static str) + 'static + Clone,
) -> impl IntoView {
    let get_theme = move || {
        let idx = theme_idx.get();
        let all = brouillon_core::theme::builtin_themes();
        all[idx % all.len()].clone()
    };
    let bar_theme = get_theme.clone();
    dyn_stack(
        move || panes.get(),
        |p: &Pane| (p.id, p.label.clone(), p.tone.map(|t| t as u8)),
        move |p: Pane| {
            let cb = on_command.clone();
            let command = p.command;
            let tone = p.tone;
            let t = get_theme.clone();
            label(move || p.label.clone())
                .on_click_stop(move |_| {
                    if let Some(c) = command {
                        cb(c);
                    }
                })
                .style(move |s| {
                    let t = t();
                    let fg = match tone {
                        Some(ChipTone::Ok) => t.fps_ok,
                        Some(ChipTone::Busy) | Some(ChipTone::Warn) => t.warn,
                        Some(ChipTone::Error) => t.error,
                        None => t.fg_dim,
                    };
                    let s = s.color(fg).font_size(11.0).padding_horiz(8.0);
                    if command.is_some() {
                        s.cursor(CursorStyle::Pointer).hover(move |h| h.background(t.fg.multiply_alpha(0.08)))
                    } else {
                        s
                    }
                })
        },
    )
    .style(move |s| {
        let t = bar_theme();
        s.flex_row().items_center().background(t.bg_chrome).border_top(1.0).border_color(t.border)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(d: &DocStatus) -> Vec<String> {
        status_model(d).into_iter().map(|p| p.label).collect()
    }

    #[test]
    fn default_panes() {
        let d = DocStatus { line: 3, col: 7, total_lines: 120, char_at_caret: Some('\u{e9}'), eol: Some(EolKind::Crlf), ..Default::default() };
        assert_eq!(labels(&d), ["Ln 3, Col 7", "120 lines", "U+00E9", "CRLF", "UTF-8", "INS"]);
    }

    #[test]
    fn conditional_panes_and_variants() {
        let d = DocStatus {
            bom: true,
            overwrite: true,
            readonly: true,
            large_file: true,
            eol_mixed: true,
            eol: Some(EolKind::Lf),
            decimal_char_code: true,
            char_at_caret: Some('A'),
            encoding: "UTF-16 LE".into(),
            ..Default::default()
        };
        assert_eq!(labels(&d), ["Ln 1, Col 1", "1 lines", "65", "Mixed EOL", "UTF-16 LE", "BOM", "OVR", "READ", "LARGE FILE"]);
        assert_eq!(labels(&DocStatus::default())[2], "--");
    }

    #[test]
    fn a_remote_document_adds_a_chip_pane_after_the_encoding() {
        use brouillon_editor_engine::remote::{RemoteDocState, RemoteSaveState};
        let d = DocStatus { remote: Some(RemoteDocState::synced("vps").with(RemoteSaveState::Conflict)), ..Default::default() };
        assert_eq!(labels(&d), ["Ln 1, Col 1", "1 lines", "--", "LF", "UTF-8", "vps  Conflict", "INS"].map(|x| if x == "LF" { EolKind::platform_default().name() } else { x }.to_string()));
        let p = status_model(&d).into_iter().find(|p| p.id == "remote").unwrap();
        assert_eq!(p.tone, Some(ChipTone::Error));
        assert!(status_model(&DocStatus::default()).iter().all(|p| p.id != "remote"));
    }

    #[test]
    fn clickable_panes_carry_commands() {
        let panes = status_model(&DocStatus::default());
        let cmd = |id: &str| panes.iter().find(|p| p.id == id).unwrap().command;
        assert_eq!(cmd("eol"), Some("eol_cycle"));
        assert_eq!(cmd("encoding"), Some("encoding_pick"));
        assert_eq!(cmd("mode"), Some("toggle_overwrite"));
        assert_eq!(cmd("lines"), None);
    }
}
