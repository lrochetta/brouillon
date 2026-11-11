//! DEV-ONLY probe used as `SSH_ASKPASS` by the real-session tests: it records how the OpenSSH client
//! calls an askpass program (argv, the SSH_ASKPASS_* environment, which standard streams are
//! terminals) into the file named by `REC_FILE`, then answers with `REC_ANSWER` (exit 0) or, when
//! `REC_CANCEL` is set, exits 1 without printing anything.

use std::io::{IsTerminal, Write};

fn main() {
    let mut s = String::new();
    s.push_str(&format!("argv={:?}\n", std::env::args().skip(1).collect::<Vec<_>>()));
    for k in ["SSH_ASKPASS", "SSH_ASKPASS_REQUIRE", "SSH_ASKPASS_PROMPT", "DISPLAY"] {
        s.push_str(&format!("{k}={:?}\n", std::env::var(k).ok()));
    }
    s.push_str(&format!("stdin_tty={} stdout_tty={} stderr_tty={}\n", std::io::stdin().is_terminal(), std::io::stdout().is_terminal(), std::io::stderr().is_terminal()));
    if let Ok(f) = std::env::var("REC_FILE") {
        if let Ok(mut o) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
            let _ = o.write_all(s.as_bytes());
            let _ = o.write_all(b"--\n");
        }
    }
    if std::env::var_os("REC_CANCEL").is_some() {
        std::process::exit(1);
    }
    println!("{}", std::env::var("REC_ANSWER").unwrap_or_default());
}
