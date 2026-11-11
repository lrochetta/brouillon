//! HTTPS GET with hard limits. Redirects are followed by hand, same host and port only.

use crate::manifest::{check_url, split_url, UrlPolicy};
use crate::{UpdateError, USER_AGENT};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

const MAX_REDIRECTS: u32 = 3;

/// One HTTP GET answer. Redirects are NOT followed here: `get` below does that, by hand.
pub struct Reply {
    pub status: u16,
    pub location: Option<String>,
    pub content_length: Option<u64>,
    pub body: Box<dyn Read>,
}

/// The only door to the network. Production uses `UreqTransport`; unit tests plug an in-process fake,
/// so the default test run opens no socket at all.
pub trait Transport {
    /// Plain GET: no query string added, no cookie, no custom header besides the generic User-Agent.
    fn get(&self, url: &str, timeout: Duration) -> Result<Reply, UpdateError>;
}

pub struct UreqTransport {
    pub dev: bool,
}

impl Transport for UreqTransport {
    fn get(&self, url: &str, timeout: Duration) -> Result<Reply, UpdateError> {
        let cfg = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .https_only(!self.dev)
            .user_agent(USER_AGENT)
            .build();
        let resp = ureq::Agent::new_with_config(cfg).get(url).call().map_err(net_err)?;
        let status = resp.status().as_u16();
        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let content_length = resp.body().content_length();
        Ok(Reply { status, location, content_length, body: Box::new(resp.into_body().into_reader()) })
    }
}

/// Development only (`BROUILLON_UPDATE_DEV_DIR`, ignored by shipped builds): serves the files of a
/// folder by the last segment of the URL. No socket, so GUI scenarios can fake an update host.
pub struct DirTransport(pub std::path::PathBuf);

impl Transport for DirTransport {
    fn get(&self, url: &str, _timeout: Duration) -> Result<Reply, UpdateError> {
        let name = url.rsplit('/').next().unwrap_or("");
        let missing = || Reply { status: 404, location: None, content_length: Some(0), body: Box::new(std::io::empty()) };
        if name.is_empty() || name.contains(['\\', ':', '/']) || name.contains("..") {
            return Ok(missing());
        }
        match std::fs::File::open(self.0.join(name)) {
            Ok(f) => {
                let len = f.metadata().ok().map(|m| m.len());
                Ok(Reply { status: 200, location: None, content_length: len, body: Box::new(f) })
            }
            Err(_) => Ok(missing()),
        }
    }
}

fn net_err(e: impl std::fmt::Display) -> UpdateError {
    UpdateError::Network(e.to_string().chars().take(160).collect())
}

/// Absolute target of a `Location` header, or `None` when it is neither absolute nor root-relative.
fn resolve_location(current: &str, loc: &str) -> Option<String> {
    if loc.starts_with("https://") || loc.starts_with("http://") {
        return Some(loc.to_string());
    }
    if loc.starts_with('/') && !loc.starts_with("//") {
        let uri: ureq::http::Uri = current.parse().ok()?;
        return Some(format!("{}://{}{}", uri.scheme_str()?, uri.authority()?, loc));
    }
    None
}

fn get(t: &dyn Transport, timeout: Duration, url: &str, policy: &UrlPolicy) -> Result<Reply, UpdateError> {
    let (start_scheme, start_host, start_port) = split_url(url).ok_or_else(|| UpdateError::InsecureUrl(url.chars().take(120).collect()))?;
    let mut cur = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        check_url(&cur, policy)?;
        let resp = t.get(&cur, timeout)?;
        let st = resp.status;
        if (300..400).contains(&st) {
            let loc = resp.location.as_deref().unwrap_or("");
            let target = resolve_location(&cur, loc).ok_or_else(|| UpdateError::RedirectRefused(loc.chars().take(120).collect()))?;
            let (s, h, p) = split_url(&target).ok_or_else(|| UpdateError::RedirectRefused(target.chars().take(120).collect()))?;
            let downgrade = start_scheme == "https" && s != "https";
            if h != start_host || p != start_port || downgrade {
                return Err(UpdateError::RedirectRefused(target.chars().take(120).collect()));
            }
            cur = target;
            continue;
        }
        return if st == 200 { Ok(resp) } else { Err(UpdateError::BadStatus(st)) };
    }
    Err(UpdateError::RedirectRefused("too many redirects".into()))
}

/// GET into memory, at most `max` bytes.
pub fn fetch_limited(t: &dyn Transport, timeout: Duration, url: &str, max: u64, policy: &UrlPolicy) -> Result<Vec<u8>, UpdateError> {
    let resp = get(t, timeout, url, policy)?;
    if resp.content_length.map_or(false, |n| n > max) {
        return Err(UpdateError::TooLarge);
    }
    let mut buf = Vec::new();
    resp.body.take(max + 1).read_to_end(&mut buf).map_err(net_err)?;
    if buf.len() as u64 > max {
        return Err(UpdateError::TooLarge);
    }
    Ok(buf)
}

/// GET into `dest`, aborting as soon as more than `max` bytes arrive. `dest` is removed on error.
pub fn download_to(t: &dyn Transport, timeout: Duration, url: &str, dest: &Path, max: u64, policy: &UrlPolicy, progress: &mut dyn FnMut(u64, u64)) -> Result<u64, UpdateError> {
    let res = (|| {
        let resp = get(t, timeout, url, policy)?;
        if resp.content_length.map_or(false, |n| n > max) {
            return Err(UpdateError::TooLarge);
        }
        let mut r = resp.body;
        let mut f = std::fs::File::create(dest)?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = r.read(&mut buf).map_err(net_err)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > max {
                return Err(UpdateError::TooLarge);
            }
            f.write_all(&buf[..n])?;
            progress(total, max);
        }
        f.sync_all()?;
        Ok(total)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    res
}
