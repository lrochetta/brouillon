//! `brouillon-mcp`: stdio relay between an MCP client and the running Brouillon editor.
//!
//! Usage: `brouillon-mcp [--pipe NAME] [--link-dir DIR] [--instance PID] [--client NAME]`
//! Exit codes: 0 clean, 1 editor unreachable or refused (a JSON-RPC error was printed on stdout).

use std::io::{self, BufReader};
use std::path::PathBuf;

use brouillon_mcp::link::{self, ClientHello};
use brouillon_mcp::relay;

fn main() {
    std::process::exit(real_main());
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = arg(&args, "--link-dir").map(PathBuf::from).unwrap_or_else(link::default_link_dir);
    let client = ClientHello { name: arg(&args, "--client").unwrap_or_default(), parent_exe: None, pid: Some(std::process::id()) };
    let stdin = BufReader::new(io::stdin());

    let token = match std::fs::read_to_string(dir.join("link.token")) {
        Ok(t) => t.trim().to_string(),
        Err(_) => return relay::fail_with_error(stdin, io::stdout(), "Brouillon is not running, or Agents is not enabled (no link token found)."),
    };

    let candidates: Vec<String> = match arg(&args, "--pipe") {
        Some(p) => vec![p],
        None => {
            let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned());
            let wanted = arg(&args, "--instance").and_then(|s| s.parse().ok());
            link::select_instances(link::read_instances(&dir), wanted, cwd.as_deref()).into_iter().map(|r| r.pipe).collect()
        }
    };

    for pipe in candidates {
        match link::connect(&pipe) {
            Ok(conn) => {
                let (r, w) = link::Shared::pair(conn);
                return relay::run(stdin, io::stdout(), r, w, &token, &client);
            }
            Err(_) => continue, // stale instance file: try the next one
        }
    }
    relay::fail_with_error(stdin, io::stdout(), "Brouillon is not running. Start it and enable Agents, then retry.")
}
