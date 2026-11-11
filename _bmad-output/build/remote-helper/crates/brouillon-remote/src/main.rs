//! `brouillon-remote`: optional helper process for remote editing.
//!
//! * no arguments: serve the framed protocol on stdin/stdout (the editor spawns it);
//! * invoked by `ssh` as `SSH_ASKPASS` (env `BROUILLON_ASKPASS_ADDR` set): forward the prompt, print the answer.
//!
//! stdout carries protocol frames only: nothing else may ever print there.

use brouillon_remote::{askpass, helper::Helper, transport::SshConnector};
use brouillon_remote_proto::Limits;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if std::env::var_os(askpass::ENV_ADDR).is_some() {
        std::process::exit(askpass::run_client(&args));
    }
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("brouillon-remote {} (protocol {})", env!("CARGO_PKG_VERSION"), brouillon_remote_proto::PROTO_VERSION);
        }
        Some("--help") | Some("-h") => {
            println!("brouillon-remote: helper process of the Brouillon editor (speaks a framed protocol on stdin/stdout).\nNot meant to be run by hand. See docs/REMOTE_HELPER_PROTOCOL.md.");
        }
        None | Some("serve") => {
            let exe = std::env::current_exe().unwrap_or_else(|_| "brouillon-remote".into());
            let helper = Helper::new(Box::new(std::io::stdout()), Arc::new(SshConnector { askpass_exe: exe }), Limits::default());
            helper.serve(std::io::stdin().lock());
        }
        Some(other) => {
            eprintln!("unknown argument {other:?}");
            std::process::exit(2);
        }
    }
}
