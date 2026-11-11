//! FTP / FTPS operations and security properties against the in-process fake server (`ftp_fake`).
//! No socket is opened anywhere in this file.

mod ftp_fake;

use brouillon_remote::ftp::{connect, FtpSession};
use brouillon_remote::ops::WriteParams;
use brouillon_remote::transport::Prompter;
use brouillon_remote_proto::*;
use ftp_fake::*;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

#[derive(Default)]
struct Answers {
    asked: Mutex<Vec<(PromptKind, String)>>,
    reply: Mutex<Option<String>>,
}
impl Prompter for Answers {
    fn ask(&self, kind: PromptKind, text: &str) -> Option<Secret> {
        self.asked.lock().unwrap().push((kind, text.to_string()));
        self.reply.lock().unwrap().clone().map(Secret::new)
    }
}

fn params(proto: Protocol, f: impl FnOnce(&mut FtpOptions)) -> ConnectParams {
    let mut o = FtpOptions { password: Some(Secret::new(PASSWORD)), fingerprint_policy: FingerprintPolicy::Strict, ..FtpOptions::default() };
    f(&mut o);
    ConnectParams {
        host: "localhost".into(),
        port: None,
        user: Some("u".into()),
        identity_file: None,
        proxy_jump: None,
        host_key_policy: HostKeyPolicy::Strict,
        known_hosts_file: None,
        ssh_config_file: None,
        interactive: false,
        connect_timeout_s: Some(2),
        ssh_path: None,
        root: None,
        protocol: proto,
        ftp: Some(o),
    }
}

fn limits() -> Limits {
    Limits { op_idle_timeout_s: 1, ..Limits::default() }
}

fn open(f: &Fake, prm: &ConnectParams) -> Result<(FtpSession, ConnectResult), RemoteError> {
    let pr: Arc<dyn Prompter> = Arc::new(Answers::default());
    connect(prm, &f.env, &pr, &limits())
}

fn ftps(f: &Fake) -> (FtpSession, ConnectResult) {
    open(f, &params(Protocol::Ftps, |_| {})).unwrap()
}

fn cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn write(s: &FtpSession, path: &str, bytes: &[u8], create: CreateMode) -> Result<WriteResult, RemoteError> {
    write_expect(s, path, bytes, create, None)
}

fn write_expect(s: &FtpSession, path: &str, bytes: &[u8], create: CreateMode, expect: Option<Expect>) -> Result<WriteResult, RemoteError> {
    let wp = WriteParams { path: p(path), size: bytes.len() as u64, create, perm: None, expect, fsync: false, allow_symlink: false };
    let mut chunks: VecDeque<Vec<u8>> = bytes.chunks(3000).map(|c| c.to_vec()).collect();
    s.write(&wp, &cancel(), || Ok(chunks.pop_front()), |_| {})
}

fn read_all(s: &FtpSession, path: &str) -> Result<Vec<u8>, RemoteError> {
    let mut out = Vec::new();
    s.read(&p(path), 0, None, None, false, &cancel(), |_, d| {
        out.extend_from_slice(d);
        Ok(())
    })?;
    Ok(out)
}

fn list(s: &FtpSession, path: &str) -> Result<(Vec<Entry>, ListResult), RemoteError> {
    let mut all = Vec::new();
    let r = s.list_dir(&p(path), &cancel(), |e| all.extend(e))?;
    Ok((all, r))
}

fn fixture(f: &Fake) {
    f.world.put("/home/u/a.txt", b"hello");
    f.world.dir("/home/u/sub");
    f.world.link("/home/u/ln", "/etc/passwd");
}

// ------------------------------------------------------------------ the happy path

#[test]
fn explicit_ftps_full_roundtrip_and_truthful_capabilities() {
    let f = Fake::new(Opts::default());
    fixture(&f);
    let (s, res) = ftps(&f);
    assert_eq!(res.home, p("/home/u"));
    assert_eq!(res.protocol, Protocol::Ftps);
    assert!(!res.insecure && !res.posix_rename && !res.fsync);
    let caps = res.ftp.clone().unwrap();
    assert!(caps.tls && caps.mlst && caps.mdtm && caps.size && caps.utf8);
    assert!(!caps.atomic_replace, "FTP never reports an atomic replace");
    assert!(!caps.supports_hash && caps.hash_algos.is_empty(), "no HASH/XSHA offered => no hash support claimed");
    assert_eq!(caps.cert_trust.as_deref(), Some("system_roots"));
    assert_eq!(caps.cert_fingerprint.as_deref(), Some(fingerprint_of(Cert::Leaf).as_str()));
    assert!(caps.tls_version.as_deref().unwrap_or("").starts_with("TLS"));
    // PBSZ/PROT before credentials, credentials only after TLS
    let log = f.world.log.lock().unwrap().clone();
    let pos = |pre: &str| log.iter().position(|l| l.starts_with(pre)).unwrap();
    assert!(pos("AUTH TLS") < pos("PBSZ 0") && pos("PBSZ 0") < pos("PROT P") && pos("PROT P") < pos("USER"));
    assert!(!f.world.pass_in_clear.load(std::sync::atomic::Ordering::Relaxed));

    let (es, r) = list(&s, "/home/u").unwrap();
    assert_eq!(r.count, 3);
    let kind = |n: &str| es.iter().find(|e| e.name.as_wire_str() == n).unwrap().kind;
    assert_eq!((kind("a.txt"), kind("sub"), kind("ln")), (EntryKind::File, EntryKind::Dir, EntryKind::Symlink));
    let st = s.stat(&p("/home/u/a.txt"), false).unwrap();
    assert_eq!((st.kind, st.size, st.mode), (EntryKind::File, Some(5), Some(0o644)));
    assert_eq!(st.mtime, Some(1_700_000_000));
    assert_eq!(s.stat(&p("/home/u/sub"), false).unwrap().kind, EntryKind::Dir);
    assert_eq!(s.stat(&p("/home/u/nope"), false).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(read_all(&s, "/home/u/a.txt").unwrap(), b"hello");

    // overwrite goes through a temp name and a rename
    let big: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let w = write(&s, "/home/u/a.txt", &big, CreateMode::Truncate).unwrap();
    assert!(!w.atomic && !w.swapped);
    assert_eq!(f.world.file("/home/u/a.txt").unwrap(), big);
    let stor: Vec<String> = f.world.log.lock().unwrap().iter().filter(|l| l.starts_with("STOR")).cloned().collect();
    assert_eq!(stor.len(), 1);
    assert!(stor[0].contains("/home/u/.a.txt.brouillon-w-") && stor[0].ends_with(".tmp"), "{stor:?}");
    assert!(f.world.log_has("RNFR /home/u/.a.txt.brouillon-w-") && f.world.log_has("RNTO /home/u/a.txt"));
    assert!(f.world.names().iter().all(|n| !n.contains("brouillon")), "no temp file left: {:?}", f.world.names());

    // create-new writes to the exact name and refuses an existing one
    write(&s, "/home/u/new.txt", b"x", CreateMode::New).unwrap();
    assert_eq!(f.world.file("/home/u/new.txt").unwrap(), b"x");
    assert_eq!(write(&s, "/home/u/new.txt", b"y", CreateMode::New).unwrap_err().kind, ErrorKind::AlreadyExists);

    s.mkdir(&p("/home/u/d2"), Some(0o700)).unwrap();
    assert_eq!(s.mkdir(&p("/home/u/d2"), None).unwrap_err().kind, ErrorKind::AlreadyExists);
    let rr = s.rename(&p("/home/u/new.txt"), &p("/home/u/d2/moved.txt"), false, false).unwrap();
    assert!(!rr.atomic, "FTP rename is never reported atomic");
    s.remove(&p("/home/u/d2/moved.txt")).unwrap();
    s.rmdir(&p("/home/u/d2")).unwrap();
    assert_eq!(s.readlink(&p("/home/u/ln")).unwrap_err().kind, ErrorKind::Unsupported);

    let (resumed, full) = s.tls_data_stats();
    assert!(resumed > 0 && full == 0, "data connections resume the control session: resumed={resumed} full={full}");
}

#[test]
fn implicit_ftps_works() {
    let f = Fake::new(Opts { implicit: true, ..Opts::default() });
    fixture(&f);
    let (s, res) = open(&f, &params(Protocol::Ftps, |o| o.tls = TlsMode::Implicit)).unwrap();
    assert!(res.ftp.unwrap().tls);
    assert!(!f.world.log_has("AUTH"), "implicit TLS needs no AUTH");
    assert_eq!(read_all(&s, "/home/u/a.txt").unwrap(), b"hello");
}

#[test]
fn hash_support_is_reported_only_when_offered() {
    let f = Fake::new(Opts { hash: true, ..Opts::default() });
    let (_s, res) = ftps(&f);
    let caps = res.ftp.unwrap();
    assert!(caps.supports_hash);
    assert_eq!(caps.hash_algos, ["SHA-256", "SHA-1"]);
}

// ------------------------------------------------------------------ TLS resumption on data connections

#[test]
fn servers_that_require_session_reuse_work_on_tls13_and_tls12() {
    for tls12 in [false, true] {
        let f = Fake::new(Opts { require_reuse: true, tls12_only: tls12, ..Opts::default() });
        fixture(&f);
        let (s, _) = ftps(&f);
        assert_eq!(list(&s, "/home/u").unwrap().1.count, 3, "tls12={tls12}");
        assert_eq!(read_all(&s, "/home/u/a.txt").unwrap(), b"hello");
        write(&s, "/home/u/b.txt", b"data", CreateMode::New).unwrap();
        let hs = f.world.data_handshakes_resumed.lock().unwrap().clone();
        assert!(hs.len() >= 3 && hs.iter().all(|r| *r), "every data handshake resumed (tls12={tls12}): {hs:?}");
    }
}

// ------------------------------------------------------------------ plain FTP is opt-in

#[test]
fn plain_ftp_is_refused_unless_opted_in_and_never_touches_the_network_first() {
    let f = Fake::new(Opts { tls: false, ..Opts::default() });
    let e = open(&f, &params(Protocol::Ftp, |_| {})).err().unwrap();
    assert_eq!(e.kind, ErrorKind::InvalidArgument);
    assert_eq!(f.world.control_connections.load(std::sync::atomic::Ordering::Relaxed), 0, "refused before any connection");
    f.world.put("/home/u/a.txt", b"hello");
    let (s, res) = open(&f, &params(Protocol::Ftp, |o| o.allow_plain = true)).unwrap();
    assert!(res.insecure, "permanent warning flag");
    let caps = res.ftp.unwrap();
    assert!(!caps.tls && caps.warnings.iter().any(|w| w.contains("plain FTP")));
    assert_eq!(read_all(&s, "/home/u/a.txt").unwrap(), b"hello");
    assert!(!f.world.log_has("AUTH"), "plain FTP never negotiates TLS");
}

// ------------------------------------------------------------------ downgrade / injection

#[test]
fn stripped_auth_tls_fails_closed_for_ftps_before_any_credential() {
    for reply_refusal in [true, false] {
        let f = Fake::new(Opts { refuse_auth_tls: reply_refusal, tls: reply_refusal, ..Opts::default() });
        let e = open(&f, &params(Protocol::Ftps, |_| {})).err().unwrap();
        assert_eq!(e.kind, ErrorKind::ConnectFailed, "{e}");
        assert!(e.message.contains("AUTH TLS") && e.message.contains("without encryption"), "{e}");
        assert!(!f.world.log_has("USER") && !f.world.log_has("PASS"), "no credential was sent: {:?}", f.world.log.lock().unwrap());
        assert!(!f.world.pass_seen.load(std::sync::atomic::Ordering::Relaxed));
    }
}

#[test]
fn plaintext_injected_after_234_aborts_the_upgrade() {
    let f = Fake::new(Opts { inject_after_auth: true, ..Opts::default() });
    let e = open(&f, &params(Protocol::Ftps, |_| {})).err().unwrap();
    assert_eq!(e.kind, ErrorKind::Protocol, "{e}");
    assert!(!f.world.pass_seen.load(std::sync::atomic::Ordering::Relaxed));
}

// ------------------------------------------------------------------ certificates

#[test]
fn unknown_certificate_is_rejected_without_a_credential() {
    let f = Fake::new(Opts::default());
    f.world.set_cert(Cert::Other);
    for policy in [FingerprintPolicy::Strict, FingerprintPolicy::Pinned, FingerprintPolicy::Ask] {
        let e = open(&f, &params(Protocol::Ftps, |o| o.fingerprint_policy = policy)).err().unwrap();
        assert_eq!(e.kind, ErrorKind::HostKeyRejected, "{policy:?}: {e}");
    }
    assert!(!f.world.pass_seen.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn matching_pin_accepts_a_self_signed_certificate() {
    let f = Fake::new(Opts::default());
    f.world.set_cert(Cert::Other);
    let fp = fingerprint_of(Cert::Other);
    let (_s, res) = open(&f, &params(Protocol::Ftps, |o| {
        o.fingerprint_policy = FingerprintPolicy::Pinned;
        o.pinned_fingerprint = Some(fp.clone());
    }))
    .unwrap();
    let caps = res.ftp.unwrap();
    assert_eq!(caps.cert_trust.as_deref(), Some("pinned"));
    assert_eq!(caps.cert_fingerprint.as_deref(), Some(fp.as_str()));
}

#[test]
fn strict_policy_never_lets_a_pin_replace_the_chain() {
    let f = Fake::new(Opts::default());
    f.world.set_cert(Cert::Other);
    let e = open(&f, &params(Protocol::Ftps, |o| o.pinned_fingerprint = Some(fingerprint_of(Cert::Other)))).err().unwrap();
    assert_eq!(e.kind, ErrorKind::HostKeyRejected, "{e}");
}

#[test]
fn changed_fingerprint_is_a_hard_stop_even_for_a_ca_signed_certificate() {
    let f = Fake::new(Opts::default());
    // the server presents the CA-signed leaf, but the user pinned another certificate
    for policy in [FingerprintPolicy::Strict, FingerprintPolicy::Pinned, FingerprintPolicy::Ask] {
        let e = open(&f, &params(Protocol::Ftps, |o| {
            o.fingerprint_policy = policy;
            o.pinned_fingerprint = Some(fingerprint_of(Cert::Other));
        }))
        .err()
        .unwrap();
        assert_eq!(e.kind, ErrorKind::HostKeyChanged, "{policy:?}: {e}");
    }
    assert!(!f.world.pass_seen.load(std::sync::atomic::Ordering::Relaxed));
    // a malformed pin is an argument error, not a silent downgrade to "no pin"
    let e = open(&f, &params(Protocol::Ftps, |o| o.pinned_fingerprint = Some("not-a-fingerprint".into()))).err().unwrap();
    assert_eq!(e.kind, ErrorKind::InvalidArgument);
}

#[test]
fn ask_policy_prompts_with_the_fingerprint_and_pins_the_confirmed_certificate() {
    let f = Fake::new(Opts::default());
    f.world.set_cert(Cert::Other);
    let mut prm = params(Protocol::Ftps, |o| o.fingerprint_policy = FingerprintPolicy::Ask);
    prm.interactive = true;
    let pr = Arc::new(Answers::default());
    *pr.reply.lock().unwrap() = Some("yes".into());
    let dynpr: Arc<dyn Prompter> = pr.clone();
    let (_s, res) = connect(&prm, &f.env, &dynpr, &limits()).unwrap();
    let asked = pr.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, PromptKind::HostKey);
    let want: String = fingerprint_of(Cert::Other).to_ascii_uppercase().as_bytes().chunks(2).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(":");
    assert!(asked[0].1.contains(&want), "prompt shows the colon-separated SHA-256: {}", asked[0].1);
    let caps = res.ftp.unwrap();
    assert_eq!(caps.cert_trust.as_deref(), Some("user_confirmed"));
    assert_eq!(caps.cert_fingerprint.as_deref(), Some(fingerprint_of(Cert::Other).as_str()));
    assert_eq!(f.world.control_connections.load(std::sync::atomic::Ordering::Relaxed), 2, "reconnected pinned; no credential on the first attempt");
    // the password was sent exactly once, over TLS
    assert_eq!(f.world.log_count("PASS"), 1);
    assert!(!f.world.pass_in_clear.load(std::sync::atomic::Ordering::Relaxed));

    // the user says no
    *pr.reply.lock().unwrap() = Some("no".into());
    let e = connect(&prm, &f.env, &dynpr, &limits()).err().unwrap();
    assert_eq!(e.kind, ErrorKind::HostKeyRejected);
    // no answer at all (cancelled prompt)
    *pr.reply.lock().unwrap() = None;
    assert_eq!(connect(&prm, &f.env, &dynpr, &limits()).err().unwrap().kind, ErrorKind::HostKeyRejected);
    // not interactive: no prompt is possible, so it fails
    prm.interactive = false;
    assert_eq!(connect(&prm, &f.env, &dynpr, &limits()).err().unwrap().kind, ErrorKind::HostKeyRejected);
}

// ------------------------------------------------------------------ passive mode

#[test]
fn pasv_and_epsv_hosts_are_ignored() {
    for no_epsv in [false, true] {
        let f = Fake::new(Opts { no_epsv, ..Opts::default() });
        fixture(&f);
        let (s, _) = ftps(&f);
        assert_eq!(list(&s, "/home/u").unwrap().1.count, 3);
        assert_eq!(read_all(&s, "/home/u/a.txt").unwrap(), b"hello");
        let dials = f.world.dials.lock().unwrap().clone();
        assert!(dials.len() >= 2);
        assert!(dials.iter().all(|(ip, _)| *ip == PEER), "data connections only go to the control peer, never 6.6.6.6: {dials:?}");
        assert!(f.world.log_has("EPSV"));
        assert_eq!(f.world.log_has("PASV"), no_epsv, "PASV only after a refused EPSV");
        if no_epsv {
            assert_eq!(f.world.log_count("EPSV"), 1, "EPSV is not retried after a refusal");
        }
    }
}

#[test]
fn an_address_inside_an_epsv_reply_is_a_protocol_error() {
    let f = Fake::new(Opts { epsv_with_address: true, ..Opts::default() });
    fixture(&f);
    let (s, _) = ftps(&f);
    assert_eq!(list(&s, "/home/u").unwrap_err().kind, ErrorKind::Protocol);
}

// ------------------------------------------------------------------ hostile listings

#[test]
fn hostile_list_output_is_filtered_capped_and_never_trusted() {
    let mut raw: Vec<u8> = Vec::new();
    let line = |s: &str| format!("{s}\r\n").into_bytes();
    raw.extend(line("-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 good.txt"));
    raw.extend(line("-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 ../escape.txt"));
    raw.extend(line("-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 /etc/passwd"));
    raw.extend(line("-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 a\u{1b}[2Jb"));
    raw.extend(b"-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 nul\0name\r\n");
    raw.extend(line("-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 good.txt")); // duplicate
    raw.extend(line("-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 \u{202e}evil"));
    raw.extend(line("this is not a listing line at all"));
    raw.extend(line("-rw-r--r-- 1 ftp ftp 99999999999999999999999 Jan 12  2023 big"));
    raw.extend(b"-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 \xff\xfe-not-utf8\r\n");
    raw.extend(line(&format!("-rw-r--r-- 1 ftp ftp 5 Jan 12  2023 {}", "x".repeat(10_000)))); // over-long
    raw.extend(line("drwxr-xr-x 2 ftp ftp 4096 Jan 12  2023 ."));
    raw.extend(line("drwxr-xr-x 2 ftp ftp 4096 Jan 12  2023 .."));
    raw.extend(line("drwxr-xr-x 2 ftp ftp 4096 Jan 12  2023 fine-dir"));
    raw.extend(line("lrwxrwxrwx 1 ftp ftp 11 Jan 12  2023 lnk -> /etc/shadow"));
    let f = Fake::new(Opts { mlst: false, raw_listing: Some(raw), ..Opts::default() });
    let (s, _) = ftps(&f);
    let (es, r) = list(&s, "/home/u").unwrap();
    let mut names: Vec<&str> = es.iter().map(|e| e.name.as_wire_str()).collect();
    names.sort();
    assert_eq!(names, ["fine-dir", "good.txt", "lnk"], "only safe names survive");
    assert_eq!(es.iter().find(|e| e.name.as_wire_str() == "lnk").unwrap().kind, EntryKind::Symlink);
    assert!(r.hidden_unsafe >= 8, "hostile lines are counted, not shown: {r:?}");
    assert!(!r.truncated);
    // the session is still in sync afterwards
    assert_eq!(s.stat(&p("/home"), false).unwrap().kind, EntryKind::Dir);
}

#[test]
fn hostile_mlsd_output_is_filtered_too() {
    let raw = b"type=file;size=1; ok\r\ntype=file;size=1; ../up\r\ntype=file;size=1; /abs\r\ntype=file;size=1; c\x1bd\r\nnonsense\r\ntype=dir; sub\r\ntype=file;size=notanumber; sz\r\ntype=cdir; .\r\ntype=pdir; ..\r\n".to_vec();
    let f = Fake::new(Opts { raw_listing: Some(raw), ..Opts::default() });
    let (s, _) = ftps(&f);
    let (es, r) = list(&s, "/home/u").unwrap();
    let mut names: Vec<&str> = es.iter().map(|e| e.name.as_wire_str()).collect();
    names.sort();
    assert_eq!(names, ["ok", "sub", "sz"]);
    assert!(f.world.log_has("MLSD"));
    assert!(r.hidden_unsafe >= 4, "{r:?}");
    assert_eq!(es.iter().find(|e| e.name.as_wire_str() == "sz").unwrap().size, None, "a bad size fact is dropped, not guessed");
}

#[test]
fn an_endless_listing_is_truncated_by_the_caps_and_the_session_survives() {
    let f = Fake::new(Opts { endless_listing: true, ..Opts::default() });
    let lim = Limits { max_list_entries: 500, max_list_bytes: 1 << 20, op_idle_timeout_s: 1, ..Limits::default() };
    let pr: Arc<dyn Prompter> = Arc::new(Answers::default());
    let (s, _) = connect(&params(Protocol::Ftps, |_| {}), &f.env, &pr, &lim).unwrap();
    let t = Instant::now();
    let (es, r) = list(&s, "/home/u").unwrap();
    assert!(r.truncated && es.len() == 500 && r.count == 500, "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(5));
    f.world.update(|o| o.endless_listing = false);
    f.world.put("/home/u/a.txt", b"hello");
    assert_eq!(s.stat(&p("/home/u/a.txt"), false).unwrap().size, Some(5), "control channel still in sync after the abort");
}

#[test]
fn mlsd_less_servers_fall_back_to_list_and_dos_style_is_parsed() {
    for dos in [false, true] {
        let f = Fake::new(Opts { mlst: false, dos_list: dos, ..Opts::default() });
        f.world.put("/home/u/a.txt", b"hello");
        f.world.dir("/home/u/sub");
        let (s, res) = ftps(&f);
        assert!(!res.ftp.unwrap().mlst);
        let (es, _) = list(&s, "/home/u").unwrap();
        assert_eq!(es.len(), 2, "dos={dos}");
        assert!(!f.world.log_has("MLSD") && !f.world.log_has("MLST"));
        assert!(f.world.log_has("CWD /home/u") && f.world.log_has("LIST"), "bare LIST after CWD (no globbing argument)");
        // stat without MLST: SIZE + MDTM for files, CWD for directories
        let st = s.stat(&p("/home/u/a.txt"), false).unwrap();
        assert_eq!((st.kind, st.size, st.mtime), (EntryKind::File, Some(5), Some(1_700_000_000)));
        assert_eq!(s.stat(&p("/home/u/sub"), false).unwrap().kind, EntryKind::Dir);
        assert_eq!(s.stat(&p("/home/u/zzz"), false).unwrap_err().kind, ErrorKind::NotFound);
    }
}

// ------------------------------------------------------------------ oversized replies, timeouts

#[test]
fn an_oversized_reply_kills_the_session_instead_of_exhausting_memory() {
    let f = Fake::new(Opts { huge_pwd_reply: 50_000_000, ..Opts::default() });
    let e = open(&f, &params(Protocol::Ftps, |_| {})).err().unwrap();
    assert_eq!(e.kind, ErrorKind::Protocol, "{e}");
}

#[test]
fn a_stalled_server_times_out_and_poisons_the_session() {
    let f = Fake::new(Opts { idle: Duration::from_millis(400), ..Opts::default() });
    fixture(&f);
    let (s, _) = ftps(&f);
    f.world.update(|o| o.stall_on = Some("MLST".into()));
    let t = Instant::now();
    let e = s.stat(&p("/home/u/a.txt"), false).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout, "{e}");
    assert!(t.elapsed() < Duration::from_secs(3));
    assert_eq!(s.stat(&p("/home/u/a.txt"), false).unwrap_err().kind, ErrorKind::ConnectionLost, "no further command on a desynchronised channel");
}

#[test]
fn a_dead_data_connection_times_out() {
    let f = Fake::new(Opts { idle: Duration::from_millis(400), ..Opts::default() });
    fixture(&f);
    let (s, _) = ftps(&f);
    f.world.update(|o| o.stall_on = Some("RETR".into()));
    assert_eq!(read_all(&s, "/home/u/a.txt").unwrap_err().kind, ErrorKind::Timeout);
}

// ------------------------------------------------------------------ safe writes

#[test]
fn a_dropped_connection_mid_upload_leaves_the_old_content() {
    let f = Fake::new(Opts::default());
    f.world.put("/home/u/doc.txt", b"OLD CONTENT");
    let (s, _) = ftps(&f);
    f.world.update(|o| o.drop_upload_after = Some(10_000));
    let big = vec![b'N'; 100_000];
    let e = write(&s, "/home/u/doc.txt", &big, CreateMode::Truncate).unwrap_err();
    assert!(matches!(e.kind, ErrorKind::ConnectionLost | ErrorKind::Timeout | ErrorKind::Protocol), "{e}");
    assert_eq!(f.world.file("/home/u/doc.txt").unwrap(), b"OLD CONTENT", "old-or-new, never partial");
    // the partial data only ever existed under the temp name
    assert!(f.world.names().iter().any(|n| n.contains(".doc.txt.brouillon-w-")), "the orphan temp is reported by the listing, not hidden: {:?}", f.world.names());
    // and the dead session refuses further work
    assert_eq!(s.stat(&p("/home/u/doc.txt"), false).unwrap_err().kind, ErrorKind::ConnectionLost);
}

#[test]
fn a_short_store_is_detected_and_the_target_is_untouched() {
    let f = Fake::new(Opts::default());
    f.world.put("/home/u/doc.txt", b"OLD");
    let (s, _) = ftps(&f);
    f.world.update(|o| o.store_only = Some(5));
    let e = write(&s, "/home/u/doc.txt", &vec![b'N'; 1000], CreateMode::Truncate).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Failure, "{e}");
    assert_eq!(f.world.file("/home/u/doc.txt").unwrap(), b"OLD");
    assert!(f.world.names().iter().all(|n| !n.contains("brouillon")), "temp removed: {:?}", f.world.names());
}

#[test]
fn servers_that_refuse_rename_over_an_existing_file_get_the_three_step_swap() {
    let f = Fake::new(Opts { rnto_overwrites: false, ..Opts::default() });
    f.world.put("/home/u/doc.txt", b"OLD");
    let (s, _) = ftps(&f);
    let w = write(&s, "/home/u/doc.txt", b"NEW CONTENT", CreateMode::Truncate).unwrap();
    assert!(w.swapped && !w.atomic);
    assert_eq!(f.world.file("/home/u/doc.txt").unwrap(), b"NEW CONTENT");
    assert!(f.world.names().iter().all(|n| !n.contains("brouillon")), "temp and old copies are gone: {:?}", f.world.names());
    assert!(f.world.log_has("RNFR /home/u/doc.txt") && f.world.log_has("DELE /home/u/.doc.txt.brouillon-old-"));
    // an explicit rename over an existing file: refused unless the caller accepts the non-atomic path
    f.world.put("/home/u/x", b"1");
    f.world.put("/home/u/y", b"2");
    assert_eq!(s.rename(&p("/home/u/x"), &p("/home/u/y"), true, false).unwrap_err().kind, ErrorKind::Unsupported);
    let r = s.rename(&p("/home/u/x"), &p("/home/u/y"), true, true).unwrap();
    assert!(!r.atomic);
    assert_eq!(f.world.file("/home/u/y").unwrap(), b"1");
    assert_eq!(s.rename(&p("/home/u/y"), &p("/home/u/doc.txt"), false, true).unwrap_err().kind, ErrorKind::AlreadyExists);
}

#[test]
fn expect_detects_a_changed_remote_file_before_uploading() {
    let f = Fake::new(Opts::default());
    f.world.put("/home/u/doc.txt", b"OLD");
    let (s, _) = ftps(&f);
    let ok = Expect { size: 3, mtime: 1_700_000_000 };
    let bad = Expect { size: 4, mtime: 1_700_000_000 };
    let e = write_expect(&s, "/home/u/doc.txt", b"NEW", CreateMode::Truncate, Some(bad)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Conflict);
    assert!(!f.world.log_has("STOR"), "nothing was uploaded");
    let e = write_expect(&s, "/home/u/gone.txt", b"NEW", CreateMode::Truncate, Some(ok)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Conflict, "deleted remotely");
    write_expect(&s, "/home/u/doc.txt", b"NEW", CreateMode::Truncate, Some(ok)).unwrap();
    assert_eq!(f.world.file("/home/u/doc.txt").unwrap(), b"NEW");
}

// ------------------------------------------------------------------ reads

#[test]
fn read_caps_prefix_and_truncation_detection() {
    let f = Fake::new(Opts::default());
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
    f.world.put("/home/u/big.bin", &data);
    let (s, _) = ftps(&f);
    // above the cap
    let e = s.read(&p("/home/u/big.bin"), 0, None, Some(1000), false, &cancel(), |_, _| Ok(())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::TooLarge);
    let e = s.read(&p("/home/u/big.bin"), 0, Some(5000), Some(1000), false, &cancel(), |_, _| Ok(())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::TooLarge);
    // a prefix, then the connection is abandoned cleanly
    let mut got = Vec::new();
    let r = s.read(&p("/home/u/big.bin"), 0, Some(300), None, false, &cancel(), |_, d| {
        got.extend_from_slice(d);
        Ok(())
    })
    .unwrap();
    assert_eq!((r.bytes, r.eof), (300, false));
    assert_eq!(got, data[..300]);
    // resume at an offset
    let mut tail = Vec::new();
    s.read(&p("/home/u/big.bin"), 99_990, None, None, false, &cancel(), |off, d| {
        assert_eq!(off, 99_990 + tail.len() as u64);
        tail.extend_from_slice(d);
        Ok(())
    })
    .unwrap();
    assert_eq!(tail, data[99_990..]);
    // a data channel that ends without close_notify AND with fewer bytes than SIZE is an error, not a short file
    f.world.update(|o| {
        o.no_close_notify = true;
        o.retr_short = Some(50_000);
    });
    let e = read_all(&s, "/home/u/big.bin").unwrap_err();
    assert_eq!(e.kind, ErrorKind::ConnectionLost, "{e}");
    // ... but a full file from a server that never sends close_notify is accepted
    f.world.update(|o| o.retr_short = None);
    assert_eq!(read_all(&s, "/home/u/big.bin").unwrap(), data);
}

#[test]
fn root_confinement_and_symlink_policy() {
    let f = Fake::new(Opts::default());
    fixture(&f);
    f.world.put("/home/other.txt", b"secret");
    let mut prm = params(Protocol::Ftps, |_| {});
    prm.root = Some(p("/home/u"));
    let (s, res) = open(&f, &prm).unwrap();
    assert_eq!(res.home, p("/home/u"));
    assert_eq!(s.stat(&p("/home/other.txt"), false).unwrap_err().kind, ErrorKind::OutsideRoot);
    assert_eq!(read_all(&s, "/home/other.txt").unwrap_err().kind, ErrorKind::OutsideRoot);
    assert_eq!(list(&s, "/home").unwrap_err().kind, ErrorKind::OutsideRoot);
    assert_eq!(write(&s, "/etc/x", b"x", CreateMode::New).unwrap_err().kind, ErrorKind::OutsideRoot);
    // FTP cannot resolve a link target, so a symlink is refused unless explicitly allowed
    assert_eq!(read_all(&s, "/home/u/ln").unwrap_err().kind, ErrorKind::SymlinkRefused);
    assert_eq!(read_all(&s, "/home/u/a.txt").unwrap(), b"hello");
}

// ------------------------------------------------------------------ secrets

#[test]
fn the_password_is_only_sent_over_tls_and_never_appears_in_errors_or_debug() {
    let f = Fake::new(Opts::default());
    let prm = params(Protocol::Ftps, |_| {});
    assert!(!format!("{prm:?}").contains(PASSWORD), "Debug of the connect parameters redacts the password");
    let ok = open(&f, &prm).unwrap();
    assert!(!format!("{:?}", ok.1).contains(PASSWORD));
    assert!(!f.world.pass_in_clear.load(std::sync::atomic::Ordering::Relaxed));
    // wrong password: AuthFailed, message without any secret, server log only holds the masked command
    let mut bad = params(Protocol::Ftps, |o| o.password = Some(Secret::new("wrong-password-123")));
    let e = open(&f, &bad).err().unwrap();
    assert_eq!(e.kind, ErrorKind::AuthFailed);
    assert!(!format!("{e:?}").contains("wrong-password-123") && !e.message.contains("wrong-password-123"));
    assert!(!f.world.log.lock().unwrap().iter().any(|l| l.contains(PASSWORD) || l.contains("wrong-password")));
    // not interactive and no password: no prompt, a clear error
    bad.ftp.as_mut().unwrap().password = None;
    assert_eq!(open(&f, &bad).err().unwrap().kind, ErrorKind::AuthFailed);
    // interactive: the password comes from a prompt
    bad.interactive = true;
    let pr = Arc::new(Answers::default());
    *pr.reply.lock().unwrap() = Some(PASSWORD.into());
    let dynpr: Arc<dyn Prompter> = pr.clone();
    connect(&bad, &f.env, &dynpr, &limits()).unwrap();
    assert_eq!(pr.asked.lock().unwrap()[0].0, PromptKind::Password);
}

#[test]
fn a_hostile_command_argument_cannot_inject_a_second_command() {
    // paths are validated by RemotePath (no CR/LF/NUL) before they reach the wire
    assert!(RemotePath::parse("/a\r\nDELE /b").is_err());
    assert!(RemotePath::parse("/a\nb").is_err());
    assert!(RemotePath::parse("/a\0b").is_err());
}
