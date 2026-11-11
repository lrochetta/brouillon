//! Build metadata embedded by `build.rs`.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD: &str = env!("BROUILLON_BUILD_NUMBER");
pub const COMMIT: &str = env!("BROUILLON_GIT_COMMIT");
pub const DATE_UTC: &str = env!("BROUILLON_BUILD_DATE");

/// `Brouillon <version> (build <N>, <commit>, <date UTC>)`
pub fn version_line() -> String {
    format!("Brouillon {VERSION} (build {BUILD}, {COMMIT}, {DATE_UTC})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_has_all_fields() {
        let l = version_line();
        assert!(l.starts_with(&format!("Brouillon {VERSION} (build ")), "{l}");
        assert!(BUILD.parse::<u64>().is_ok());
        assert!(DATE_UTC.len() == 20 && DATE_UTC.ends_with('Z') && &DATE_UTC[10..11] == "T", "{DATE_UTC}");
        assert!(l.ends_with(')'));
    }
}

#[cfg(test)]
mod cli_tests {
    use clap::CommandFactory;

    #[test]
    fn cli_version_matches_version_line() {
        let mut cmd = crate::cli::Cli::command();
        let out = cmd.render_version().trim().to_string();
        assert_eq!(out, super::version_line());
    }
}
