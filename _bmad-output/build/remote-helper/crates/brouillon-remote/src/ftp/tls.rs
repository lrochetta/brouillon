//! TLS for FTPS: one `ClientConfig` shared by the control and every data connection (so rustls can
//! resume the control session on data connections, which servers with "require session reuse" demand),
//! and a certificate verifier that cannot be turned off:
//!
//! 1. a pin (`pinned_fingerprint`, SHA-256 of the leaf) is ALWAYS enforced when set;
//! 2. a matching pin is accepted without a chain (ssh `known_hosts` semantics), unless the policy is `Strict`;
//! 3. otherwise the chain, validity dates and host name are verified by `rustls-webpki` against the OS roots;
//! 4. after the control handshake the session is pinned to the certificate it presented, so a full
//!    handshake on a data connection that shows another certificate is refused.
//!
//! Handshake signatures are always verified by the ring provider. There is no code path that accepts
//! a certificate nobody vouched for: an unknown certificate is only ever accepted by the CALLER after the
//! user confirmed its fingerprint, by reconnecting with that fingerprint as the pin.

use super::ctrl::Stream;
use brouillon_remote_proto::{ErrorKind, FingerprintPolicy, RemoteError};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerifier, ServerCertVerified};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme, StreamOwned};
use std::sync::{Arc, Mutex};

pub type TlsStream = StreamOwned<ClientConnection, Box<dyn Stream>>;

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Parse a user-supplied fingerprint: 64 hex digits, `:` / space separators and case tolerated.
pub fn parse_fingerprint(s: &str) -> Option<[u8; 32]> {
    let h: String = s.chars().filter(|c| !matches!(c, ':' | ' ')).collect();
    if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing decided yet.
    None,
    SystemRoots,
    Pinned,
    /// A pin was set and the server presented another certificate.
    PinMismatch,
    /// Not vouched for by the roots (and no matching pin): the reason from the verifier.
    Untrusted(String),
}

#[derive(Debug)]
struct State {
    observed: Option<[u8; 32]>,
    session_pin: Option<[u8; 32]>,
    outcome: Outcome,
}

pub struct Verifier {
    inner: Option<Arc<WebPkiServerVerifier>>,
    algs: WebPkiSupportedAlgorithms,
    pin: Option<[u8; 32]>,
    /// `false` for `Strict`: the pin is an extra constraint, never a substitute for the chain.
    pin_suffices: bool,
    state: Mutex<State>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Verifier")
    }
}

impl Verifier {
    pub fn new(provider: &Arc<CryptoProvider>, roots: Arc<RootCertStore>, policy: FingerprintPolicy, pin: Option<[u8; 32]>) -> Arc<Verifier> {
        let inner = if roots.is_empty() { None } else { WebPkiServerVerifier::builder_with_provider(roots, provider.clone()).build().ok() };
        Arc::new(Verifier {
            inner,
            algs: provider.signature_verification_algorithms,
            pin,
            pin_suffices: policy != FingerprintPolicy::Strict,
            state: Mutex::new(State { observed: None, session_pin: None, outcome: Outcome::None }),
        })
    }

    pub fn observed(&self) -> Option<[u8; 32]> {
        self.state.lock().unwrap().observed
    }
    pub fn outcome(&self) -> Outcome {
        self.state.lock().unwrap().outcome.clone()
    }
    /// After the control handshake: every later full handshake must present this very certificate.
    pub fn pin_session(&self) {
        let mut s = self.state.lock().unwrap();
        s.session_pin = s.observed;
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = sha256(end_entity.as_ref());
        {
            let mut s = self.state.lock().unwrap();
            if s.session_pin.is_some() {
                // a later (data) handshake: the control certificate is the only acceptable one
                return if s.session_pin == Some(fp) {
                    Ok(ServerCertVerified::assertion())
                } else {
                    s.outcome = Outcome::PinMismatch;
                    Err(rustls::Error::General("the data connection presented a different certificate".into()))
                };
            }
            s.observed = Some(fp);
        }
        if let Some(pin) = self.pin {
            if pin != fp {
                self.state.lock().unwrap().outcome = Outcome::PinMismatch;
                return Err(rustls::Error::General("certificate fingerprint does not match the pinned one".into()));
            }
            if self.pin_suffices {
                self.state.lock().unwrap().outcome = Outcome::Pinned;
                return Ok(ServerCertVerified::assertion());
            }
        }
        let res = match &self.inner {
            Some(v) => v.verify_server_cert(end_entity, intermediates, server_name, ocsp, now),
            None => Err(rustls::Error::General("no system root certificates are available".into())),
        };
        let mut s = self.state.lock().unwrap();
        match &res {
            Ok(_) => s.outcome = if self.pin.is_some() { Outcome::Pinned } else { Outcome::SystemRoots },
            Err(e) => s.outcome = Outcome::Untrusted(e.to_string()),
        }
        res
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Everything needed to start TLS on the control connection and on each data connection.
pub struct TlsCtx {
    pub config: Arc<ClientConfig>,
    pub name: ServerName<'static>,
    pub verifier: Arc<Verifier>,
    /// How many data handshakes resumed the control session / needed a full handshake (diagnostics).
    pub resumed: std::sync::atomic::AtomicU32,
    pub full: std::sync::atomic::AtomicU32,
    /// Protocol version negotiated by the first (control) handshake.
    pub version: Mutex<Option<String>>,
}

pub fn system_roots() -> Arc<RootCertStore> {
    static ROOTS: std::sync::OnceLock<Arc<RootCertStore>> = std::sync::OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            let r = rustls_native_certs::load_native_certs();
            for c in r.certs {
                let _ = store.add(c);
            }
            Arc::new(store)
        })
        .clone()
}

impl TlsCtx {
    pub fn new(host: &str, roots: Arc<RootCertStore>, policy: FingerprintPolicy, pin: Option<[u8; 32]>) -> Result<TlsCtx, RemoteError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Verifier::new(&provider, roots, policy, pin);
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| RemoteError::new(ErrorKind::Internal, format!("TLS setup failed: {e}")))?
            .dangerous() // the "dangerous" builder is how a custom verifier is installed; ours never skips validation
            .with_custom_certificate_verifier(verifier.clone())
            .with_no_client_auth();
        config.enable_early_data = false;
        let name = ServerName::try_from(host.to_string()).map_err(|_| RemoteError::new(ErrorKind::InvalidArgument, "invalid host name for TLS"))?;
        Ok(TlsCtx { config: Arc::new(config), name, verifier, resumed: Default::default(), full: Default::default(), version: Mutex::new(None) })
    }

    /// Run the TLS handshake over `io` to completion.
    pub fn handshake(&self, io: Box<dyn Stream>) -> Result<TlsStream, RemoteError> {
        let conn = ClientConnection::new(self.config.clone(), self.name.clone())
            .map_err(|e| RemoteError::new(ErrorKind::ConnectFailed, format!("TLS error: {e}")))?;
        let mut s = StreamOwned::new(conn, io);
        while s.conn.is_handshaking() {
            if let Err(e) = s.conn.complete_io(&mut s.sock) {
                return Err(self.classify(&e));
            }
        }
        let mut v = self.version.lock().unwrap();
        if v.is_none() {
            *v = version_name(&s.conn);
        }
        Ok(s)
    }

    pub fn classify(&self, e: &std::io::Error) -> RemoteError {
        match self.verifier.outcome() {
            Outcome::PinMismatch => RemoteError::new(ErrorKind::HostKeyChanged, "the server certificate differs from the pinned fingerprint; the connection was stopped"),
            Outcome::Untrusted(why) => RemoteError::new(ErrorKind::HostKeyRejected, format!("the server certificate is not trusted: {why}")),
            _ => match e.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => RemoteError::new(ErrorKind::Timeout, "the TLS handshake timed out"),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe => {
                    RemoteError::new(ErrorKind::ConnectionLost, format!("the TLS handshake was cut: {e}"))
                }
                _ => RemoteError::new(ErrorKind::ConnectFailed, format!("TLS handshake failed: {e}")),
            },
        }
    }
}

pub fn version_name(c: &ClientConnection) -> Option<String> {
    c.protocol_version().map(|v| format!("{v:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_parsing() {
        let h = "ab".repeat(32);
        assert_eq!(parse_fingerprint(&h), Some([0xab; 32]));
        let colon: Vec<String> = (0..32).map(|_| "AB".to_string()).collect();
        assert_eq!(parse_fingerprint(&colon.join(":")), Some([0xab; 32]));
        assert_eq!(parse_fingerprint("abc"), None);
        assert_eq!(parse_fingerprint(&"zz".repeat(32)), None);
        assert_eq!(hex(&[0xde, 0xad]), "dead");
    }

    #[test]
    fn sha256_known_answer() {
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
