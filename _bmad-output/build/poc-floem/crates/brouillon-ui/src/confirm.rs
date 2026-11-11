//! Confirmation dialogs for dirty-buffer scenarios.
//!
//! Uses the `rfd` crate (already a dependency) for native OS dialogs.
//! M1 scope: "Save changes?" when closing a dirty tab or quitting with
//! unsaved changes. Moldu mode shows friendly native dialogs; Geek mode
//! uses the same mechanism (native dialogs are acceptable for both presets).

/// Result of a save-confirmation dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmResult {
    /// User chose to save before closing.
    Save,
    /// User chose to discard changes.
    Discard,
    /// User cancelled the close/quit operation.
    Cancel,
}

/// Show a "Save changes?" dialog for a single file.
///
/// `title` is the file name or tab title (e.g. "main.rs" or "untitled-1").
/// Returns the user's choice.
///
/// Uses `rfd::MessageDialog` with Yes/No/Cancel buttons. The dialog is
/// modal and blocks until the user responds.
pub fn confirm_save(title: &str) -> ConfirmResult {
    let result = rfd::MessageDialog::new()
        .set_title("Brouillon — Save Changes?")
        .set_description(&format!(
            "\"{}\" has unsaved changes.\n\nDo you want to save before closing?",
            title
        ))
        .set_buttons(rfd::MessageButtons::YesNoCancel)
        .set_level(rfd::MessageLevel::Warning)
        .show();

    match result {
        rfd::MessageDialogResult::Yes => ConfirmResult::Save,
        rfd::MessageDialogResult::No => ConfirmResult::Discard,
        _ => ConfirmResult::Cancel,
    }
}

/// Show a "Quit with unsaved changes?" dialog when multiple tabs are dirty.
///
/// `count` is the number of unsaved files. Returns the user's choice.
pub fn confirm_quit_dirty(count: usize) -> ConfirmResult {
    let desc = if count == 1 {
        "You have 1 unsaved file.\n\nDo you want to save before quitting?".to_string()
    } else {
        format!(
            "You have {} unsaved files.\n\nDo you want to save all before quitting?",
            count
        )
    };

    let result = rfd::MessageDialog::new()
        .set_title("Brouillon — Quit?")
        .set_description(&desc)
        .set_buttons(rfd::MessageButtons::YesNoCancel)
        .set_level(rfd::MessageLevel::Warning)
        .show();

    match result {
        rfd::MessageDialogResult::Yes => ConfirmResult::Save,
        rfd::MessageDialogResult::No => ConfirmResult::Discard,
        _ => ConfirmResult::Cancel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_result_enum_is_copy() {
        let r = ConfirmResult::Save;
        let r2 = r; // Copy
        assert_eq!(r, r2);
    }

    #[test]
    fn confirm_result_variants_are_distinct() {
        assert_ne!(ConfirmResult::Save, ConfirmResult::Discard);
        assert_ne!(ConfirmResult::Discard, ConfirmResult::Cancel);
        assert_ne!(ConfirmResult::Save, ConfirmResult::Cancel);
    }

    #[test]
    fn confirm_result_is_one_byte() {
        assert_eq!(std::mem::size_of::<ConfirmResult>(), 1);
    }

    #[test]
    fn confirm_result_debug_format() {
        let s = format!("{:?}", ConfirmResult::Save);
        assert_eq!(s, "Save");
        let d = format!("{:?}", ConfirmResult::Discard);
        assert_eq!(d, "Discard");
        let c = format!("{:?}", ConfirmResult::Cancel);
        assert_eq!(c, "Cancel");
    }

    #[test]
    fn confirm_result_clone() {
        let r = ConfirmResult::Save;
        let cloned = r.clone();
        assert_eq!(r, cloned);
    }

    #[test]
    fn pattern_match_exhaustive() {
        // Ensures all variants are covered
        let results = [ConfirmResult::Save, ConfirmResult::Discard, ConfirmResult::Cancel];
        for r in results {
            match r {
                ConfirmResult::Save => assert_eq!(r, ConfirmResult::Save),
                ConfirmResult::Discard => assert_eq!(r, ConfirmResult::Discard),
                ConfirmResult::Cancel => assert_eq!(r, ConfirmResult::Cancel),
            }
        }
    }

    #[test]
    fn eq_is_symmetric() {
        let a = ConfirmResult::Save;
        let b = ConfirmResult::Save;
        assert_eq!(a, b);
        assert_eq!(b, a);
    }
}
