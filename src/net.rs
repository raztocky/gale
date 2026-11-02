//! Loading resources: local files, file:// and http(s):// URLs.

use std::time::Duration;

pub fn fetch(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .try_proxy_from_env(true)
            .user_agent("Gale/0.1 (toy browser engine)")
            .build();
        let resp = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        return resp.into_string().map_err(|e| format!("{url}: {e}"));
    }
    std::fs::read_to_string(url).map_err(|e| format!("{url}: {e}"))
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
    use super::resolve;

    #[test]
    fn resolves() {
        assert_eq!(resolve("https://a.com/x/y.html", "z.html"), "https://a.com/x/z.html");
        assert_eq!(resolve("https://a.com/x/y.html", "../z"), "https://a.com/z");
        assert_eq!(resolve("https://a.com/x/", "/q?a=1"), "https://a.com/q?a=1");
        assert_eq!(resolve("https://a.com", "b"), "https://a.com/b");
        assert_eq!(resolve("file:///tmp/a/b.html", "c.css"), "file:///tmp/a/c.css");
        assert_eq!(resolve("https://a.com/p", "//cdn.x/y"), "https://cdn.x/y");
    }
}
