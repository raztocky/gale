//! Loading resources: local files, file:// and http(s):// URLs.
//!
//! Security rules enforced here:
//! - Only `http`, `https` and `file` are fetchable; everything else is refused.
//! - A page served over the network may never load `file://` resources
//!   (see [`may_load`]).
//! - Plain `http://` is tried over `https://` first (HTTPS-first).
//! - Response size, redirects and time are bounded.

use std::io::Read;
use std::time::Duration;

/// Largest document or stylesheet we will read.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REDIRECTS: u32 = 5;

pub struct Fetched {
    /// The URL actually loaded (after HTTPS upgrade and redirects).
    pub url: String,
    pub body: String,
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

pub fn fetch(url: &str) -> Result<Fetched, String> {
    match scheme(url) {
        "file" => {
            let path = url.trim_start_matches("file://");
            let path = path.split(['?', '#']).next().unwrap_or(path);
            read_file(path).map(|body| Fetched { url: url.to_string(), body })
        }
        "https" => fetch_http(url),
        "http" => {
            // HTTPS-first: quietly upgrade, fall back to plain HTTP only if
            // the secure attempt fails.
            let upgraded = format!("https://{}", &url["http://".len()..]);
            fetch_http(&upgraded).or_else(|_| fetch_http(url))
        }
        "" => read_file(url).map(|body| Fetched { url: url.to_string(), body }),
        s => Err(format!("{url}: the \"{s}\" scheme is not supported")),
    }
}

fn read_file(path: &str) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a regular file"));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("{path}: file is larger than {} MB", MAX_BYTES / 1024 / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn fetch_http(url: &str) -> Result<Fetched, String> {
    let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirects(MAX_REDIRECTS)
        .try_proxy_from_env(!bypass_proxy(&host))
        .user_agent("Brickle/0.1 (toy browser engine)")
        .build();
    let resp = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let final_url = resp.get_url().to_string();
    // A redirect may only land on http(s).
    if !matches!(scheme(&final_url), "http" | "https") {
        return Err(format!("{url}: redirected to a disallowed URL"));
    }
    let ctype = resp.content_type().to_ascii_lowercase();
    let texty = ctype.starts_with("text/")
        || ctype.contains("html")
        || ctype.contains("xml")
        || ctype.contains("css")
        || ctype.is_empty();
    if !texty {
        return Err(format!("{url}: can't display content of type {ctype}"));
    }
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{url}: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{url}: response is larger than {} MB", MAX_BYTES / 1024 / 1024));
    }
    Ok(Fetched { url: final_url, body: String::from_utf8_lossy(&bytes).into_owned() })
}

/// Honour NO_PROXY (ureq 2 doesn't): comma-separated hosts, `.suffix` or
/// `*.suffix` domains, and `*` for everything.
fn bypass_proxy(host: &str) -> bool {
    let list = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")).unwrap_or_default();
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
    if href.contains("://") || href.starts_with("about:") || href.starts_with("mailto:") || href.starts_with("data:") {
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
        assert_eq!(resolve("https://a.com/x/y.html", "z.html"), "https://a.com/x/z.html");
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
        assert_eq!(display_parts("https://google.com@evil.com/x").0, "https://evil.com");
        // Cyrillic "а" in a lookalike domain is shown as punycode.
        assert_eq!(display_parts("https://\u{430}pple.com/").0, "https://xn--pple-43d.com");
        assert_eq!(display_parts("http://a.com:8080/p?q=1").1, "/p?q=1");
    }
}
