//! BK-051 — command line: `brouillon [options] [file[:line[:col]]]...`.

use std::path::PathBuf;

use clap::Parser;

use crate::ipc::Request;

#[derive(Parser, Debug, Clone, PartialEq, Eq, Default)]
#[command(name = "brouillon", display_name = "Brouillon", version = concat!(env!("CARGO_PKG_VERSION"), " (build ", env!("BROUILLON_BUILD_NUMBER"), ", ", env!("BROUILLON_GIT_COMMIT"), ", ", env!("BROUILLON_BUILD_DATE"), ")"), about = "Brouillon - the fast editor that doesn't phone home")]
pub struct Cli {
    /// Files to open, each as `path[:line[:col]]`.
    pub files: Vec<String>,
    /// Jump to `LINE[:COL]` in the first file (a `:line` suffix on the file wins).
    #[arg(long, value_name = "LINE[:COL]")]
    pub goto: Option<String>,
    /// Open in a new window instead of the running instance.
    #[arg(long)]
    pub new_window: bool,
    /// Do not return until the opened file is closed (forwarded to the instance).
    #[arg(long)]
    pub wait: bool,
    /// Open files read-only.
    #[arg(long)]
    pub readonly: bool,
    /// Start without restoring (or saving) the previous session.
    #[arg(long)]
    pub no_session: bool,
    /// Use this settings directory instead of the default.
    #[arg(long, value_name = "DIR")]
    pub settings_dir: Option<PathBuf>,
    /// Import an EditPlus settings folder into the settings directory, print the report and exit.
    #[arg(long, value_name = "DIR")]
    pub import_editplus: Option<PathBuf>,
    /// With --import-editplus: print the report without writing anything.
    #[arg(long, requires = "import_editplus")]
    pub dry_run: bool,
    /// Turn Agents (the MCP server) on for this session only; settings.toml is not changed.
    #[arg(long)]
    pub mcp: bool,
    /// Open this folder in the folder tree (implies a new window; the last folder is reopened otherwise).
    #[arg(long, value_name = "DIR")]
    pub folder: Option<PathBuf>,
    /// Open a project file (.brouillon-project.toml): its folder in the folder tree and its files as tabs (implies a new window).
    #[arg(long, value_name = "FILE")]
    pub project: Option<PathBuf>,
    /// Development: with no file, open the generated 100,000-line sample document.
    #[arg(long, hide = true)]
    pub sample: bool,
}

/// Split `path[:line[:col]]`. Trailing numeric segments only; a Windows drive
/// letter (`C:\x`) or any non-numeric tail stays part of the path.
pub fn parse_target(s: &str) -> (PathBuf, Option<usize>, Option<usize>) {
    let mut rest = s;
    let mut nums: Vec<usize> = Vec::new();
    while nums.len() < 2 {
        match rest.rsplit_once(':') {
            Some((head, tail)) if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                nums.push(tail.parse().unwrap_or(1));
                rest = head;
            }
            _ => break,
        }
    }
    nums.reverse();
    (PathBuf::from(rest), nums.first().copied(), nums.get(1).copied())
}

fn parse_goto(s: &str) -> (Option<usize>, Option<usize>) {
    let mut it = s.split(':').map(|p| p.parse::<usize>().ok());
    (it.next().flatten(), it.next().flatten())
}

impl Cli {
    /// The IPC requests this invocation stands for, in order.
    pub fn requests(&self) -> Vec<Request> {
        let goto = self.goto.as_deref().map(parse_goto);
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (path, mut line, mut col) = parse_target(f);
                if let (0, None, Some((gl, gc))) = (i, line, goto) {
                    line = gl;
                    col = gc;
                }
                Request::Open {
                    path,
                    line,
                    col,
                    readonly: self.readonly,
                    new_window: self.new_window,
                    wait: self.wait,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_line_col() {
        assert_eq!(parse_target("a.rs"), ("a.rs".into(), None, None));
        assert_eq!(parse_target("a.rs:12"), ("a.rs".into(), Some(12), None));
        assert_eq!(parse_target("a.rs:12:3"), ("a.rs".into(), Some(12), Some(3)));
        assert_eq!(parse_target(r"C:\dir\a.rs:7:1"), (r"C:\dir\a.rs".into(), Some(7), Some(1)));
        assert_eq!(parse_target(r"C:\dir\a.rs"), (r"C:\dir\a.rs".into(), None, None));
    }

    #[test]
    fn flags_and_requests() {
        let cli = Cli::parse_from([
            "brouillon", "--readonly", "--new-window", "--wait", "--no-session", "--settings-dir", "/tmp/s", "x.rs:5:2", "y.rs",
        ]);
        assert!(cli.readonly && cli.new_window && cli.wait && cli.no_session);
        assert_eq!(cli.settings_dir, Some(PathBuf::from("/tmp/s")));
        let reqs = cli.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            reqs[0],
            Request::Open { path: "x.rs".into(), line: Some(5), col: Some(2), readonly: true, new_window: true, wait: true }
        );
    }

    #[test]
    fn import_flags() {
        let cli = Cli::parse_from(["brouillon", "--import-editplus", "C:/ep", "--dry-run", "--settings-dir", "/tmp/s"]);
        assert_eq!(cli.import_editplus, Some(PathBuf::from("C:/ep")));
        assert!(cli.dry_run);
        assert!(Cli::try_parse_from(["brouillon", "--dry-run"]).is_err(), "--dry-run alone is refused");
        assert!(!Cli::parse_from(["brouillon"]).dry_run);
    }

    #[test]
    fn goto_applies_to_first_file_only_when_no_suffix() {
        let cli = Cli::parse_from(["brouillon", "--goto", "9:4", "a.rs", "b.rs"]);
        let reqs = cli.requests();
        assert!(matches!(&reqs[0], Request::Open { line: Some(9), col: Some(4), .. }));
        assert!(matches!(&reqs[1], Request::Open { line: None, .. }));
        let cli = Cli::parse_from(["brouillon", "--goto", "9", "a.rs:2"]);
        assert!(matches!(&cli.requests()[0], Request::Open { line: Some(2), .. }));
    }

    #[test]
    fn mcp_flag_is_off_unless_given() {
        assert!(!Cli::parse_from(["brouillon"]).mcp);
        assert!(Cli::parse_from(["brouillon", "--mcp", "a.rs"]).mcp);
    }

    #[test]
    fn no_files_means_no_requests() {
        assert!(Cli::parse_from(["brouillon"]).requests().is_empty());
    }
}
