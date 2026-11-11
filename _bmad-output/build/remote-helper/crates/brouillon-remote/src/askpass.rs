//! `SSH_ASKPASS` plumbing.
//!
//! ssh, started with `SSH_ASKPASS=<this exe>` and `SSH_ASKPASS_REQUIRE=force`, runs `<this exe> "<prompt>"`
//! whenever it needs a password / passphrase / host-key confirmation and reads the answer from that
//! process's stdout. The askpass invocation (same exe, detected by `BROUILLON_ASKPASS_ADDR`) forwards the
//! prompt to the long-running helper over a loopback socket, the helper forwards it to the editor as a
//! `prompt` event, and the answer travels back the same way. The secret never appears in argv, in a file
//! or in a log.
//!
//! The loopback listener binds 127.0.0.1 on an ephemeral port, lives only while the connection lives,
//! accepts only peers that present a 256-bit random token (given to ssh in the environment of the
//! child, readable only by the same user), and handles one prompt per connection.
//!
//! STATUS: the exchange logic is unit-tested over in-memory streams and once over a real loopback
//! socket, plus an end-to-end test that runs the real exe in askpass mode. That ssh.exe on Windows
//! really invokes the askpass exe this way IS verified (OpenSSH_10.3p1 for Windows, no tty, against
//! the dev-only user-space sshd in `crates/brouillon-test-sshd`, test `t10`): with
//! `SSH_ASKPASS_REQUIRE=force` ssh.exe calls the askpass exe with the prompt as its single argument,
//! all three std streams non-terminals, a path containing spaces works; WITHOUT `force` (unset, or
//! `prefer` with no DISPLAY) ssh.exe never calls askpass and fails silently. `force` is mandatory.
//!
//! A named pipe would avoid opening a TCP port at all; std has no server-side named pipe, so it would
//! need a hand-written FFI layer. Left as a possible hardening step.

use crate::transport::{classify_prompt, Prompter};
use brouillon_remote_proto::sanitize_text;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const ENV_ADDR: &str = "BROUILLON_ASKPASS_ADDR";
pub const ENV_TOKEN: &str = "BROUILLON_ASKPASS_TOKEN";

pub struct AskpassServer {
    addr: SocketAddr,
    token: String,
    stop: Arc<AtomicBool>,
}

fn new_token() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).expect("os randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl AskpassServer {
    pub fn start(prompter: Arc<dyn Prompter>) -> std::io::Result<AskpassServer> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let token = new_token();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (token, stop) = (token.clone(), stop.clone());
            std::thread::Builder::new().name("askpass".into()).spawn(move || {
                // polling accept: stopping needs no wake-up connection
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((c, _)) => {
                            let (p, t) = (prompter.clone(), token.clone());
                            std::thread::spawn(move || {
                                let _ = c.set_nonblocking(false);
                                let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
                                if let Ok(w) = c.try_clone() {
                                    serve_conn(c.take(8192), w, &*p, &t);
                                }
                            });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                        Err(_) => break,
                    }
                }
            })?;
        }
        Ok(AskpassServer { addr, token, stop })
    }
    pub fn addr(&self) -> String {
        self.addr.to_string()
    }
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One prompt exchange over any byte stream: `token\nprompt\n` in, `OK\nsecret\n` or `CANCEL\n` out.
fn serve_conn(reader: impl Read, mut w: impl Write, prompter: &dyn Prompter, token: &str) {
    let mut r = BufReader::new(reader);
    let (mut t, mut prompt) = (String::new(), String::new());
    if r.read_line(&mut t).is_err() || r.read_line(&mut prompt).is_err() {
        return;
    }
    if !ct_eq(t.trim_end_matches(['\r', '\n']), token) {
        return; // wrong token: silently drop
    }
    let prompt = sanitize_text(prompt.trim_end_matches(['\r', '\n']));
    match prompter.ask(classify_prompt(&prompt), &prompt) {
        Some(s) if !s.expose().contains(['\r', '\n']) => {
            let _ = writeln!(w, "OK");
            let _ = writeln!(w, "{}", s.expose());
        }
        _ => {
            let _ = writeln!(w, "CANCEL");
        }
    }
}

/// Askpass mode: called by ssh as `exe "<prompt>"`. Returns the process exit code.
pub fn run_client(args: &[String]) -> i32 {
    let (Ok(addr), Ok(token)) = (std::env::var(ENV_ADDR), std::env::var(ENV_TOKEN)) else { return 2 };
    let prompt = args.join(" ").replace(['\r', '\n'], " ");
    match ask(&addr, &token, &prompt) {
        Some(secret) => {
            let mut out = std::io::stdout();
            let _ = writeln!(out, "{secret}");
            let _ = out.flush();
            0
        }
        None => 1,
    }
}

pub fn ask(addr: &str, token: &str, prompt: &str) -> Option<String> {
    let addr: SocketAddr = addr.parse().ok()?;
    if !addr.ip().is_loopback() {
        return None;
    }
    let mut c = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;
    c.set_read_timeout(Some(Duration::from_secs(300))).ok()?;
    writeln!(c, "{token}").ok()?;
    writeln!(c, "{prompt}").ok()?;
    let mut r = BufReader::new(c);
    let mut status = String::new();
    r.read_line(&mut status).ok()?;
    if status.trim_end() != "OK" {
        return None;
    }
    let mut secret = String::new();
    r.read_line(&mut secret).ok()?;
    Some(secret.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brouillon_remote_proto::{PromptKind, Secret};
    use std::io::Cursor;
    use std::sync::Mutex;

    struct Fake(Mutex<Vec<(PromptKind, String)>>, Option<&'static str>);
    impl Prompter for Fake {
        fn ask(&self, k: PromptKind, t: &str) -> Option<Secret> {
            self.0.lock().unwrap().push((k, t.to_string()));
            self.1.map(Secret::new)
        }
    }

    fn exchange(token_sent: &str, prompt: &str, p: &Fake) -> String {
        let mut out = Vec::new();
        serve_conn(Cursor::new(format!("{token_sent}\n{prompt}\n")), &mut out, p, "tok");
        String::from_utf8(out).unwrap()
    }

    // NOTE: the only test that opens a real socket is #[ignore]d. The security product on the dev
    // machine deletes test executables that do loopback listen/connect cycles (reproduced with a
    // 10-line Rust program; the file vanishes after the run and the next relink fails with
    // LNK1104), so the protocol logic is tested over in-memory streams. Run it explicitly with
    // `cargo test -- --ignored round_trip` (it passes; the exe is then lost and the package
    // version must be bumped to relink, a local-machine nuisance only).
    #[test]
    #[ignore = "real loopback socket; see comment"]
    fn round_trip_through_loopback() {
        let p = Arc::new(Fake(Mutex::new(vec![]), Some("s3cret pass")));
        let srv = AskpassServer::start(p.clone()).unwrap();
        let got = ask(&srv.addr(), srv.token(), "Enter passphrase for key 'k': ");
        assert_eq!(got.as_deref(), Some("s3cret pass"));
        let seen = p.0.lock().unwrap();
        assert_eq!(seen[0].0, PromptKind::Passphrase);
        assert!(seen[0].1.contains("passphrase"));
    }

    #[test]
    fn protocol_ok_answer() {
        let p = Fake(Mutex::new(vec![]), Some("pw"));
        assert_eq!(exchange("tok", "u@h's password: ", &p), "OK\npw\n");
        assert_eq!(p.0.lock().unwrap()[0].0, PromptKind::Password);
    }

    #[test]
    fn wrong_token_gets_nothing_and_prompter_is_not_asked() {
        let p = Fake(Mutex::new(vec![]), Some("x"));
        assert_eq!(exchange("0000", "password: ", &p), "");
        assert_eq!(exchange("", "password: ", &p), "");
        assert!(p.0.lock().unwrap().is_empty());
    }

    #[test]
    fn cancel_and_newline_secrets_are_refused() {
        assert_eq!(exchange("tok", "password: ", &Fake(Mutex::new(vec![]), None)), "CANCEL\n");
        assert_eq!(exchange("tok", "password: ", &Fake(Mutex::new(vec![]), Some("a\nb"))), "CANCEL\n");
    }

    #[test]
    fn prompt_text_is_sanitised_before_it_reaches_the_editor() {
        let p = Fake(Mutex::new(vec![]), Some("x"));
        exchange("tok", "pass\x1b[31mword: ", &p);
        assert!(!p.0.lock().unwrap()[0].1.chars().any(|c| c.is_control()));
    }

    #[test]
    fn non_loopback_address_is_refused_by_client() {
        assert_eq!(ask("8.8.8.8:80", "t", "p"), None);
    }

    #[test]
    fn tokens_are_random_and_long() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(ct_eq(&a, &a) && !ct_eq(&a, &b));
    }
}
