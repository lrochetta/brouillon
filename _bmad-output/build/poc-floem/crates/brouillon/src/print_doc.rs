//! File > Print: hand the document to the operating system's own print handler.
//!
//! Brouillon has no print layout of its own. The current text (saved or not) is written to a
//! temporary `.txt` copy and the platform's print command is started on it: the shell "print"
//! verb on Windows, `lp` elsewhere. On Windows the file name travels in an environment
//! variable, never in the command text, so no name can change the command. Nothing leaves the
//! machine except what the user's own printer setup does.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A temp file name made of the document title: letters, digits, `-`, `_` only.
pub fn temp_file_name(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .take(60)
        .collect();
    let stem = stem.trim_matches('_');
    format!("{}.txt", if stem.is_empty() { "document" } else { stem })
}

/// Program, arguments and the environment variable that carries the file path.
#[derive(Debug, PartialEq, Eq)]
pub struct PrintCommand {
    pub program: &'static str,
    pub args: Vec<String>,
    pub env: (&'static str, String),
}

pub const ENV_FILE: &str = "BROUILLON_PRINT_FILE";

pub fn print_command(windows: bool, file: &Path) -> PrintCommand {
    let f = file.to_string_lossy().into_owned();
    if windows {
        PrintCommand {
            program: "powershell",
            args: ["-NoProfile", "-NonInteractive", "-Command", "Start-Process -FilePath $env:BROUILLON_PRINT_FILE -Verb Print"]
                .map(String::from)
                .to_vec(),
            env: (ENV_FILE, f),
        }
    } else {
        PrintCommand { program: "lp", args: vec!["--".into(), f.clone()], env: (ENV_FILE, f) }
    }
}

/// Write `text` to a temp copy and start the print handler. Returns the message to show.
pub fn print_text(title: &str, text: &str) -> Result<String, String> {
    let dir: PathBuf = std::env::temp_dir().join("brouillon-print");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Print: cannot create the temporary folder: {e}"))?;
    let file = dir.join(temp_file_name(title));
    std::fs::write(&file, text).map_err(|e| format!("Print: cannot write the temporary copy: {e}"))?;
    let pc = print_command(cfg!(windows), &file);
    let mut cmd = Command::new(pc.program);
    cmd.args(&pc.args).env(pc.env.0, &pc.env.1);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn().map_err(|e| format!("Print: the system print command could not start ({e})"))?;
    Ok(format!("Sent \"{title}\" to the system print handler"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_are_safe_and_never_empty() {
        assert_eq!(temp_file_name("notes v2.md"), "notes_v2_md.txt");
        assert_eq!(temp_file_name("..\\..\\evil"), "evil.txt");
        assert_eq!(temp_file_name("???"), "document.txt");
        assert_eq!(temp_file_name(""), "document.txt");
        assert!(temp_file_name(&"a".repeat(500)).len() <= 64);
    }

    #[test]
    fn the_path_never_enters_the_windows_command_text() {
        let f = Path::new("C:\\t\\x'; calc; '.txt");
        let c = print_command(true, f);
        assert_eq!(c.program, "powershell");
        assert!(c.args.iter().all(|a| !a.contains("calc")), "{:?}", c.args);
        assert_eq!(c.env.0, ENV_FILE);
        assert!(c.env.1.contains("calc"));
    }

    #[test]
    fn unix_uses_lp_with_a_double_dash() {
        let c = print_command(false, Path::new("/tmp/a.txt"));
        assert_eq!(c.program, "lp");
        assert_eq!(c.args, vec!["--".to_string(), "/tmp/a.txt".to_string()]);
    }
}
