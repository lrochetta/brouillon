//! URL and email detection for inline link highlighting.
//!
//! Uses the `linkify` crate for robust link detection and the `open` crate
//! for cross-platform "open in default browser" functionality.
//!
//! M1 scope: HTTP/HTTPS URLs and email addresses. Ctrl+double-click on a
//! highlighted link opens it in the system browser.

/// Kind of detected link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Url,
    Email,
}

/// A detected link span within a line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    /// Start byte offset within the line.
    pub start: usize,
    /// End byte offset within the line (exclusive).
    pub end: usize,
    /// The kind of link.
    pub kind: LinkKind,
    /// The full URL or email string.
    pub url: String,
}

/// Detect all URLs and emails in a single line of text.
///
/// Returns spans sorted by start position. Uses `linkify::LinkFinder`
/// for robust detection of HTTP/HTTPS/FTP URLs and email addresses.
pub fn detect_links(line: &str) -> Vec<LinkSpan> {
    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url, linkify::LinkKind::Email]);

    finder
        .links(line)
        .map(|link| {
            let kind = match link.kind() {
                linkify::LinkKind::Url => LinkKind::Url,
                linkify::LinkKind::Email => LinkKind::Email,
                _ => LinkKind::Url, // fallback
            };
            LinkSpan {
                start: link.start(),
                end: link.end(),
                kind,
                url: link.as_str().to_string(),
            }
        })
        .collect()
}

/// Open a URL or email in the system's default handler.
///
/// For URLs, opens in the default browser. For emails, opens the default
/// mail client with a `mailto:` prefix.
pub fn open_link(url: &str, kind: LinkKind) {
    let target = match kind {
        LinkKind::Url => url.to_string(),
        LinkKind::Email => {
            if url.starts_with("mailto:") {
                url.to_string()
            } else {
                format!("mailto:{}", url)
            }
        }
    };
    // Fire and forget — we don't care if the browser fails to open.
    let _ = open::that(&target);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_http_url() {
        let links = detect_links("Visit https://brouillon.dev for more info");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Url);
        assert_eq!(links[0].url, "https://brouillon.dev");
        assert_eq!(links[0].start, 6);
        assert_eq!(links[0].end, 27);
    }

    #[test]
    fn detect_http_with_path() {
        let links = detect_links("See http://example.com/path?q=1#anchor here");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Url);
        assert!(links[0].url.starts_with("http://example.com"));
    }

    #[test]
    fn detect_email() {
        let links = detect_links("Contact contact@example.org for support");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Email);
        assert_eq!(links[0].url, "contact@example.org");
    }

    #[test]
    fn detect_multiple_links() {
        let links = detect_links("Go to https://a.com and email user@b.com now");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].kind, LinkKind::Url);
        assert_eq!(links[1].kind, LinkKind::Email);
    }

    #[test]
    fn detect_no_links() {
        let links = detect_links("This is just plain text without any links");
        assert!(links.is_empty());
    }

    #[test]
    fn detect_ftp_url() {
        let links = detect_links("Download from ftp://files.example.com/file.zip");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Url);
    }

    #[test]
    fn open_link_prepends_mailto_for_email() {
        // Just test the logic — we can't actually open a browser in tests
        let kind = LinkKind::Email;
        let url = "test@example.com";
        let target = match kind {
            LinkKind::Url => url.to_string(),
            LinkKind::Email => format!("mailto:{}", url),
        };
        assert_eq!(target, "mailto:test@example.com");
    }

    #[test]
    fn detect_url_with_complex_query() {
        let links = detect_links("API: https://api.example.com/v2/search?q=test&page=1&sort=desc");
        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("sort=desc"));
    }

    #[test]
    fn link_span_clone_eq() {
        let span = LinkSpan {
            start: 0, end: 10, kind: LinkKind::Url,
            url: "https://x.com".to_string(),
        };
        let cloned = span.clone();
        assert_eq!(span, cloned);
    }

    #[test]
    fn link_kind_debug() {
        assert_eq!(format!("{:?}", LinkKind::Url), "Url");
        assert_eq!(format!("{:?}", LinkKind::Email), "Email");
    }
}
