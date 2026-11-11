//! Opening an FTP/FTPS session: dial, TLS (explicit or implicit), certificate decision, login,
//! feature discovery. Nothing secret is sent before the TLS handshake of an FTPS session succeeded.

use super::ctrl::{Ctrl, Stream};
use super::parse;
use super::session::{map_reply, FtpSession};
use super::tls::{self, hex, parse_fingerprint, Outcome, TlsCtx};
use super::FtpEnv;
use crate::transport::Prompter;
use brouillon_remote_proto::*;
use std::sync::Arc;
use std::time::Duration;

type R<T> = Result<T, RemoteError>;

struct Established {
    ctrl: Ctrl,
    peer_ip: std::net::IpAddr,
    closer: super::Closer,
    tls: Option<Arc<TlsCtx>>,
}

struct Failed {
    err: RemoteError,
    /// Set when the only problem was an unknown certificate: its SHA-256 and the verifier's reason.
    untrusted: Option<([u8; 32], String)>,
}

impl From<RemoteError> for Failed {
    fn from(err: RemoteError) -> Failed {
        Failed { err, untrusted: None }
    }
}

fn host_ok(h: &str) -> bool {
    !h.is_empty() && h.len() <= 255 && !h.starts_with('-') && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '%' | '[' | ']'))
}

fn user_ok(u: &str) -> bool {
    !u.is_empty() && u.len() <= 128 && u.chars().all(|c| !c.is_control())
}

fn banner(ctrl: &mut Ctrl) -> R<()> {
    for _ in 0..3 {
        let r = ctrl.reply()?;
        match r.class() {
            2 => return Ok(()),
            1 => continue, // 120: service ready in N minutes
            _ => return Err(RemoteError::new(ErrorKind::ConnectFailed, format!("the server refused the connection: {} {}", r.code, r.text()))),
        }
    }
    Err(RemoteError::new(ErrorKind::ConnectFailed, "the server never became ready"))
}

fn establish(p: &ConnectParams, o: &FtpOptions, env: &FtpEnv, limits: &Limits, pin: Option<[u8; 32]>) -> Result<Established, Failed> {
    let plain = p.protocol == Protocol::Ftp;
    let implicit = !plain && o.tls == TlsMode::Implicit;
    let port = p.port.unwrap_or(if implicit { 990 } else { 21 });
    let idle = Duration::from_secs(limits.op_idle_timeout_s.max(1));
    let ct = Duration::from_secs(p.connect_timeout_s.unwrap_or(10).clamp(1, 120) as u64);
    let dialed = env
        .dialer
        .control(&p.host, port, ct, idle)
        .map_err(|e| RemoteError::new(if e.kind() == std::io::ErrorKind::TimedOut { ErrorKind::Timeout } else { ErrorKind::ConnectFailed }, format!("could not connect to {}:{port}: {e}", p.host)))?;
    let tls = if plain {
        None
    } else {
        let roots = env.roots.clone().unwrap_or_else(tls::system_roots);
        Some(Arc::new(TlsCtx::new(&p.host, roots, o.fingerprint_policy, pin)?))
    };
    let handshake = |t: &Arc<TlsCtx>, s: Box<dyn Stream>| -> Result<Box<dyn Stream>, Failed> {
        match t.handshake(s) {
            Ok(ts) => Ok(Box::new(ts)),
            Err(err) => {
                let untrusted = match (t.verifier.outcome(), t.verifier.observed()) {
                    (Outcome::Untrusted(why), Some(fp)) => Some((fp, why)),
                    _ => None,
                };
                Err(Failed { err, untrusted })
            }
        }
    };
    let mut ctrl;
    if implicit {
        let t = tls.as_ref().unwrap();
        ctrl = Ctrl::new(handshake(t, dialed.stream)?);
        t.verifier.pin_session();
        banner(&mut ctrl)?;
    } else {
        ctrl = Ctrl::new(dialed.stream);
        banner(&mut ctrl)?;
        if let Some(t) = &tls {
            let r = ctrl.cmd("AUTH TLS")?;
            if r.code != 234 {
                return Err(RemoteError::new(
                    ErrorKind::ConnectFailed,
                    format!("the server did not accept AUTH TLS ({} {}); refusing to continue without encryption", r.code, r.text()),
                )
                .into());
            }
            let raw = ctrl.take_for_tls()?;
            let secured = handshake(t, raw)?;
            t.verifier.pin_session();
            ctrl.put_stream(secured);
        }
    }
    if tls.is_some() {
        // PBSZ 0 / PROT P are mandatory: a private control channel with clear data would leak the files
        let r = ctrl.cmd("PBSZ 0")?;
        if !r.ok() {
            return Err(RemoteError::new(ErrorKind::ConnectFailed, format!("PBSZ was refused: {} {}", r.code, r.text())).into());
        }
        let r = ctrl.cmd("PROT P")?;
        if r.code != 200 {
            return Err(RemoteError::new(ErrorKind::ConnectFailed, format!("the server would not protect the data channel (PROT P): {} {}", r.code, r.text())).into());
        }
    }
    Ok(Established { ctrl, peer_ip: dialed.peer_ip, closer: dialed.closer, tls })
}

fn login(ctrl: &mut Ctrl, user: &str, pass: Option<&Secret>) -> R<()> {
    let r = ctrl.cmd(&format!("USER {user}"))?;
    match r.code {
        230 | 232 => return Ok(()),
        331 | 332 => {}
        _ => return Err(auth_err(&r)),
    }
    let pw = pass.map(|s| s.expose()).unwrap_or("");
    // the PASS line is built here and nowhere else; errors only ever carry the server's reply
    let r = ctrl.cmd_secret(&format!("PASS {pw}")).map_err(|e| RemoteError::new(e.kind, "connection error during login"))?;
    if r.class() == 2 {
        Ok(())
    } else {
        Err(auth_err(&r))
    }
}

fn auth_err(r: &super::ctrl::Reply) -> RemoteError {
    let mut e = map_reply(r, false);
    if matches!(e.kind, ErrorKind::Failure | ErrorKind::PermissionDenied | ErrorKind::NotFound) || r.code == 530 {
        e.kind = ErrorKind::AuthFailed;
    }
    e
}

fn colon_hex(fp: &[u8; 32]) -> String {
    fp.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

pub fn connect(p: &ConnectParams, env: &FtpEnv, prompter: &Arc<dyn Prompter>, limits: &Limits) -> R<(FtpSession, ConnectResult)> {
    let o = p.ftp.clone().unwrap_or_default();
    if p.protocol == Protocol::Ftp && !o.allow_plain {
        return Err(RemoteError::new(ErrorKind::InvalidArgument, "plain FTP is disabled for this site: it sends the password and every file unencrypted. Use FTPS, or enable plain FTP explicitly for this site."));
    }
    if !host_ok(&p.host) {
        return Err(RemoteError::new(ErrorKind::InvalidArgument, "invalid host"));
    }
    if p.port == Some(0) {
        return Err(RemoteError::new(ErrorKind::InvalidArgument, "invalid port"));
    }
    let user = p.user.clone().unwrap_or_else(|| "anonymous".into());
    if !user_ok(&user) {
        return Err(RemoteError::new(ErrorKind::InvalidArgument, "invalid user"));
    }
    let mut pin = match &o.pinned_fingerprint {
        Some(s) => Some(parse_fingerprint(s).ok_or_else(|| RemoteError::new(ErrorKind::InvalidArgument, "the pinned fingerprint is not a SHA-256 in hex"))?),
        None => None,
    };
    let mut user_confirmed = false;
    let est = loop {
        match establish(p, &o, env, limits, pin) {
            Ok(e) => break e,
            Err(f) => match f.untrusted {
                Some((fp, why)) if o.fingerprint_policy == FingerprintPolicy::Ask && p.interactive && pin.is_none() && !user_confirmed => {
                    let text = format!(
                        "The certificate of {} is not trusted by the system ({}).\nSHA-256 fingerprint: {}\nTrust this certificate for this site? Answer yes to continue.",
                        p.host,
                        why,
                        colon_hex(&fp)
                    );
                    let yes = prompter.ask(PromptKind::HostKey, &text).is_some_and(|a| a.expose().trim().eq_ignore_ascii_case("yes"));
                    if !yes {
                        return Err(RemoteError::new(ErrorKind::HostKeyRejected, "the server certificate was not accepted"));
                    }
                    // reconnect pinned to exactly the certificate the user saw; nothing secret was sent yet
                    pin = Some(fp);
                    user_confirmed = true;
                }
                _ => return Err(f.err),
            },
        }
    };
    let Established { mut ctrl, peer_ip, closer, tls } = est;

    // credentials: only now, after the TLS handshake of an FTPS session
    let anonymous = p.user.is_none();
    let password: Option<Secret> = if anonymous {
        Some(Secret::new("anonymous@"))
    } else if let Some(pw) = o.password.clone() {
        Some(pw)
    } else if p.interactive {
        prompter.ask(PromptKind::Password, &format!("FTP password for {user}@{}", p.host))
    } else {
        None
    };
    if !anonymous && password.is_none() {
        return Err(RemoteError::new(ErrorKind::AuthFailed, "no password was provided"));
    }
    login(&mut ctrl, &user, password.as_ref())?;

    let feat = match ctrl.cmd("FEAT") {
        Ok(r) if r.ok() && r.lines.len() >= 2 => parse::parse_feat(&r.lines[1..r.lines.len() - 1]),
        Ok(_) => parse::Feat::default(),
        Err(e) => return Err(e),
    };
    if feat.utf8 {
        let _ = ctrl.cmd("OPTS UTF8 ON");
    }
    let r = ctrl.cmd("TYPE I")?;
    if r.code != 200 {
        return Err(RemoteError::new(ErrorKind::ConnectFailed, "the server refused binary mode"));
    }
    if let Some(root) = &p.root {
        let r = ctrl.cmd(&format!("CWD {}", root.as_wire_str()))?;
        if !r.ok() {
            return Err(map_reply(&r, true));
        }
    }
    let r = ctrl.cmd("PWD")?;
    if r.code != 257 {
        return Err(map_reply(&r, false));
    }
    let home = parse::parse_pwd(r.first())
        .and_then(|s| RemotePath::parse(&s).ok())
        .ok_or_else(|| RemoteError::new(ErrorKind::Protocol, "the server returned an unusable working directory"))?;

    let secure = tls.is_some();
    let mut warnings = Vec::new();
    if !secure {
        warnings.push("plain FTP: the password and all file contents cross the network unencrypted".to_string());
    }
    if !feat.mlst && !feat.mdtm {
        warnings.push("the server reports no modification times: conflict detection is limited to the file size".to_string());
    }
    if !feat.size {
        warnings.push("the server did not advertise SIZE: uploads cannot be size-verified".to_string());
    }
    let (fp, trust, version) = match &tls {
        Some(t) => {
            let fp = t.verifier.observed().map(|f| hex(&f));
            let trust = if user_confirmed {
                "user_confirmed"
            } else {
                match t.verifier.outcome() {
                    Outcome::Pinned => "pinned",
                    _ => "system_roots",
                }
            };
            (fp, Some(trust.to_string()), t.version.lock().unwrap().clone())
        }
        None => (None, None, None),
    };
    let caps = FtpCaps {
        tls: secure,
        tls_version: version,
        cert_fingerprint: fp,
        cert_trust: trust,
        atomic_replace: false,
        mlst: feat.mlst,
        mdtm: feat.mdtm,
        size: feat.size,
        utf8: feat.utf8,
        supports_hash: !feat.hash.is_empty(),
        hash_algos: feat.hash.clone(),
        warnings,
    };
    let ext: Vec<String> = {
        let mut v = Vec::new();
        if feat.mlst {
            v.push("MLST".to_string());
        }
        if feat.mdtm {
            v.push("MDTM".to_string());
        }
        if feat.size {
            v.push("SIZE".to_string());
        }
        if feat.utf8 {
            v.push("UTF8".to_string());
        }
        v.extend(feat.hash.iter().cloned());
        v
    };
    let ct = Duration::from_secs(p.connect_timeout_s.unwrap_or(10).clamp(1, 120) as u64);
    let session = FtpSession::new(ctrl, peer_ip, closer, p.root.clone(), limits.clone(), feat, tls, env.dialer.clone(), ct);
    let res = ConnectResult {
        home,
        sftp_version: 0,
        extensions: ext,
        posix_rename: false,
        fsync: false,
        protocol: p.protocol,
        insecure: !secure,
        ftp: Some(caps),
    };
    Ok((session, res))
}
