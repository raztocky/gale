//! Low-level loading: local files and single HTTP(S) exchanges.
//!
//! Policy (blocking, HTTPS-only, redirects, logging) lives in
//! [`crate::network`]. This module is the only place in Brickle that opens
//! network connections; a test enforces that.

use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Largest document or stylesheet we will read.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Brickle presents itself as stock Firefox on Linux, sending the same
/// headers in the same order, so it doesn't stand out. Bump this with
/// Firefox releases (see `browser/config/version_display.txt` in Firefox).
pub const FIREFOX_VERSION: &str = "159.0";

pub fn user_agent() -> String {
    format!(
        "Mozilla/5.0 (X11; Linux x86_64; rv:{v}) Gecko/20100101 Firefox/{v}",
        v = FIREFOX_VERSION
    )
}

static REQUESTS: AtomicUsize = AtomicUsize::new(0);

/// How many HTTP requests this process has sent (for privacy tests).
pub fn requests_sent() -> usize {
    REQUESTS.load(Ordering::SeqCst)
}

pub fn scheme(url: &str) -> &str {
    url.split_once("://").map(|(s, _)| s).unwrap_or("")
}

/// May a document at `from` load `to`? `from` is `None` for user-initiated
/// loads (typed address, command line), which may open local files.
pub fn may_load(from: Option<&str>, to: &str) -> bool {
    match scheme(to) {
        "http" | "https" => true,
        "file" => from.is_none_or(|f| scheme(f) == "file"),
        _ => false,
    }
}

pub fn read_file(path: &str) -> Result<String, String> {
    let path = path.trim_start_matches("file://");
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a regular file"));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "{path}: file is larger than {} MB",
            MAX_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Loopback and private-network hosts, which HTTPS-only mode doesn't
/// upgrade (as in Firefox).
pub fn is_local_host(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") {
        return true;
    }
    match h.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// What kind of request, which decides the headers Firefox would send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    Document,
    Stylesheet,
    /// `brickle filters update`.
    Download,
}

/// Value for `Sec-Fetch-Site` given the requesting page (None = typed).
pub fn fetch_site(initiator: Option<&str>, target: &str) -> &'static str {
    let Some(from) = initiator else { return "none" };
    let (Ok(a), Ok(b)) = (url::Url::parse(from), url::Url::parse(target)) else {
        return "cross-site";
    };
    if a.origin() == b.origin() {
        "same-origin"
    } else if a.scheme() == b.scheme()
        && psl::domain_str(a.host_str().unwrap_or("")) == psl::domain_str(b.host_str().unwrap_or(""))
    {
        "same-site"
    } else {
        "cross-site"
    }
}

pub struct Response {
    pub status: u16,
    /// `Location` header of a redirect, unresolved.
    pub location: Option<String>,
    pub body: String,
}

/// One HTTP GET. Redirects are returned, not followed, so the caller can
/// check every hop. HTTP error statuses are returned with their body.
pub fn http_get(url: &str, dest: Dest, initiator: Option<&str>) -> Result<Response, String> {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirects(0)
        .try_proxy_from_env(!bypass_proxy(&host))
        .build();
    let site = fetch_site(initiator, url);
    // Header names, values and order as sent by Firefox.
    let mut req = agent.get(url).set("User-Agent", &user_agent());
    req = match dest {
        Dest::Document => req
            .set(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .set("Accept-Language", "en-US,en;q=0.5")
            .set("Accept-Encoding", "gzip, deflate, br, zstd")
            .set("Connection", "keep-alive")
            .set("Upgrade-Insecure-Requests", "1")
            .set("Sec-Fetch-Dest", "document")
            .set("Sec-Fetch-Mode", "navigate")
            .set("Sec-Fetch-Site", site)
            .set("Sec-Fetch-User", "?1")
            .set("Priority", "u=0, i"),
        Dest::Stylesheet => req
            .set("Accept", "text/css,*/*;q=0.1")
            .set("Accept-Language", "en-US,en;q=0.5")
            .set("Accept-Encoding", "gzip, deflate, br, zstd")
            .set("Connection", "keep-alive")
            .set("Sec-Fetch-Dest", "style")
            .set("Sec-Fetch-Mode", "no-cors")
            .set("Sec-Fetch-Site", site)
            .set("Priority", "u=2"),
        Dest::Download => req
            .set("Accept", "*/*")
            .set("Accept-Language", "en-US,en;q=0.5")
            .set("Accept-Encoding", "gzip, deflate, br, zstd")
            .set("Connection", "keep-alive"),
    };
    REQUESTS.fetch_add(1, Ordering::SeqCst);
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(e.to_string()),
    };
    let status = resp.status();
    if (300..400).contains(&status) {
        return Ok(Response {
            status,
            location: resp.header("location").map(str::to_string),
            body: String::new(),
        });
    }
    let ctype = resp.content_type().to_ascii_lowercase();
    let texty = ctype.starts_with("text/")
        || ctype.contains("html")
        || ctype.contains("xml")
        || ctype.contains("css")
        || ctype.is_empty();
    if !texty {
        return Err(format!("can't display content of type {ctype}"));
    }
    // ureq decodes gzip and brotli itself; deflate and zstd are done here.
    let encoding = resp
        .header("content-encoding")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let reader: Box<dyn Read> = Box::new(resp.into_reader().take(MAX_BYTES * 4));
    let mut decoded: Box<dyn Read> = match encoding.as_str() {
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(reader)),
        "zstd" => {
            Box::new(ruzstd::decoding::StreamingDecoder::new(reader).map_err(|e| format!("zstd: {e}"))?)
        }
        _ => reader,
    };
    let mut bytes = Vec::new();
    // Bounded after decompression too, so a compression bomb can't blow up.
    (&mut decoded)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("response is larger than {} MB", MAX_BYTES / 1024 / 1024));
    }
    Ok(Response {
        status,
        location: None,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// Honour NO_PROXY (ureq 2 doesn't): comma-separated hosts, `.suffix` or
/// `*.suffix` domains, and `*` for everything.
fn bypass_proxy(host: &str) -> bool {
    let list = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    no_proxy_matches(&list, host)
}

fn no_proxy_matches(list: &str, host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    list.split(',').map(|e| e.trim().to_ascii_lowercase()).any(|e| {
        if e.is_empty() {
            return false;
        }
        if e == "*" {
            return true;
        }
        let e = e.trim_start_matches('*');
        if let Some(suffix) = e.strip_prefix('.') {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == e
        }
    })
}

/// The URL as it should be shown in the address bar: the host is converted to
/// its ASCII (punycode) form and any `user:pass@` part is dropped, so a
/// lookalike or `trusted.com@evil.com` URL can't impersonate another site.
/// Returns (prefix up to and including the host, rest).
pub fn display_parts(input: &str) -> (String, String) {
    match url::Url::parse(input) {
        Ok(u) if u.host_str().is_some() => {
            let mut head = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
            if let Some(port) = u.port() {
                head.push_str(&format!(":{port}"));
            }
            let mut rest = u.path().to_string();
            if rest == "/" {
                rest.clear();
            }
            if let Some(q) = u.query() {
                rest.push('?');
                rest.push_str(q);
            }
            if let Some(f) = u.fragment() {
                rest.push('#');
                rest.push_str(f);
            }
            (head, rest)
        }
        _ => (String::new(), input.to_string()),
    }
}

/// Turn a user-typed address or a path into an absolute URL.
pub fn normalize(input: &str) -> String {
    let input = input.trim();
    if input.contains("://") || input.starts_with("about:") {
        return input.to_string();
    }
    if let Ok(p) = std::fs::canonicalize(input) {
        return format!("file://{}", p.display());
    }
    format!("https://{input}")
}

/// Resolve `href` relative to `base`.
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.contains("://")
        || href.starts_with("about:")
        || href.starts_with("brickle:")
        || href.starts_with("mailto:")
        || href.starts_with("data:")
    {
        return href.to_string();
    }
    let base = base.split('#').next().unwrap_or(base);
    let (scheme, rest) = base.split_once("://").unwrap_or(("file", base));
    if let Some(h) = href.strip_prefix("//") {
        return format!("{scheme}://{h}");
    }
    if href.starts_with('#') {
        return format!("{base}{href}");
    }
    // Split authority from path ("host/path" or "/abs/path" for file URLs).
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = path.split('?').next().unwrap_or(path);
    let joined = if href.starts_with('/') {
        href.to_string()
    } else if href.starts_with('?') {
        format!("{path}{href}")
    } else {
        let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
        format!("{dir}{href}")
    };
    // Normalize dot segments.
    let (p, query) = match joined.split_once('?') {
        Some((p, q)) => (p.to_string(), format!("?{q}")),
        None => (joined.clone(), String::new()),
    };
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = p.split('/').collect();
    for (i, seg) in segs.iter().enumerate() {
        match *seg {
            "." => {}
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
            }
            s => out.push(s),
        }
        if i == segs.len() - 1 && (*seg == "." || *seg == "..") {
            out.push("");
        }
    }
    format!("{scheme}://{authority}{}{query}", out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves() {
        assert_eq!(
            resolve("https://a.com/x/y.html", "z.html"),
            "https://a.com/x/z.html"
        );
        assert_eq!(resolve("https://a.com/x/y.html", "../z"), "https://a.com/z");
        assert_eq!(resolve("https://a.com/x/", "/q?a=1"), "https://a.com/q?a=1");
        assert_eq!(resolve("https://a.com", "b"), "https://a.com/b");
        assert_eq!(resolve("file:///tmp/a/b.html", "c.css"), "file:///tmp/a/c.css");
        assert_eq!(resolve("https://a.com/p", "//cdn.x/y"), "https://cdn.x/y");
    }

    #[test]
    fn web_pages_cannot_load_local_files() {
        assert!(!may_load(Some("https://evil.com/"), "file:///etc/passwd"));
        assert!(!may_load(Some("http://evil.com/"), "file:///home/me/.ssh/id_rsa"));
        assert!(may_load(Some("file:///tmp/a.html"), "file:///tmp/b.css"));
        assert!(may_load(None, "file:///tmp/a.html"));
        assert!(may_load(Some("https://a.com/"), "https://b.com/"));
        assert!(!may_load(Some("https://a.com/"), "javascript:alert(1)"));
        assert!(!may_load(None, "ftp://x/"));
    }

    #[test]
    fn local_hosts_and_fetch_site() {
        for h in [
            "localhost",
            "127.0.0.1",
            "192.168.1.10",
            "10.0.0.2",
            "[::1]",
            "printer.local",
        ] {
            assert!(is_local_host(h), "{h}");
        }
        for h in ["example.com", "8.8.8.8", "localhost.example.com"] {
            assert!(!is_local_host(h), "{h}");
        }
        assert_eq!(fetch_site(None, "https://a.com/"), "none");
        assert_eq!(
            fetch_site(Some("https://a.com/x"), "https://a.com/y"),
            "same-origin"
        );
        assert_eq!(
            fetch_site(Some("https://www.a.co.uk/"), "https://img.a.co.uk/"),
            "same-site"
        );
        assert_eq!(fetch_site(Some("https://a.com/"), "https://b.com/"), "cross-site");
    }

    #[test]
    fn no_proxy() {
        let l = "localhost,127.0.0.1, .corp.example,*.svc.local";
        assert!(no_proxy_matches(l, "127.0.0.1"));
        assert!(no_proxy_matches(l, "a.corp.example"));
        assert!(no_proxy_matches(l, "x.svc.local"));
        assert!(!no_proxy_matches(l, "example.com"));
        assert!(!no_proxy_matches(l, "evilcorp.example"));
    }

    #[test]
    fn address_bar_shows_real_host() {
        assert_eq!(
            display_parts("https://google.com@evil.com/x").0,
            "https://evil.com"
        );
        // Cyrillic "а" in a lookalike domain is shown as punycode.
        assert_eq!(
            display_parts("https://\u{430}pple.com/").0,
            "https://xn--pple-43d.com"
        );
        assert_eq!(display_parts("http://a.com:8080/p?q=1").1, "/p?q=1");
    }
}
