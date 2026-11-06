//! Content Security Policy, for pages whose scripts you allowed: the
//! server's `Content-Security-Policy` headers (and `<meta http-equiv>`
//! ones) decide which scripts run and where scripts may send requests, so a
//! script injected into a page is held to what the page's own authors
//! allowed.
//!
//! Script execution is enforced in the renderer; image/style sources and
//! redirects are also checked by the browser. Supported script directives: `script-src`,
//! `script-src-elem`, `script-src-attr` and `connect-src`, with `default-src`
//! standing in for any of them that's missing. Image/style support also covers
//! `img-src`, `style-src`, `style-src-elem` and `style-src-attr`. Sources: `'none'`, `'self'`,
//! `*`, schemes (`https:`), hosts (`*.example.com:443/path`),
//! `'unsafe-inline'`, `'unsafe-eval'`, `'unsafe-hashes'`, `'strict-dynamic'`,
//! nonces and SHA-256/384/512 hashes. Several policies all have to agree.
//! Reports (`report-uri`, `report-to`) are never sent: that would be a
//! request you didn't ask for. `Content-Security-Policy-Report-Only` is
//! ignored for the same reason.

use std::collections::HashMap;

/// Most policies, directives per policy and sources per directive kept;
/// a hostile page gets no further.
const MAX_POLICIES: usize = 16;
const MAX_DIRECTIVES: usize = 64;
pub const MAX_SOURCES: usize = 256;
/// Oversized sources fail closed: dropping a nonce could re-enable unsafe-inline.
pub const MAX_SOURCE_LEN: usize = 2048;

#[derive(Debug, Clone, Default)]
pub struct Csp {
    policies: Vec<HashMap<String, Vec<String>>>,
    overflow: bool,
    /// The page's scheme, host and port.
    page: Option<(String, String, u16)>,
}

fn parts(url: &str) -> Option<(String, String, u16)> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    Some((u.scheme().to_string(), host, u.port_or_known_default()?))
}

impl Csp {
    /// No policies yet, for a page at `page_url`.
    pub fn new(page_url: &str) -> Csp {
        Csp {
            policies: Vec::new(),
            overflow: false,
            page: parts(page_url),
        }
    }

    /// Add the policies in a `Content-Security-Policy` header (or meta)
    /// value: commas separate policies, semicolons directives.
    pub fn add(&mut self, value: &str) {
        for policy in value.split(',').filter(|p| !p.trim().is_empty()) {
            if self.policies.len() >= MAX_POLICIES {
                self.overflow = true;
                return;
            }
            let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
            for (i, d) in policy.split(';').filter(|d| !d.trim().is_empty()).enumerate() {
                if i >= MAX_DIRECTIVES {
                    self.overflow = true;
                    break;
                }
                let mut words = d.split_ascii_whitespace();
                let Some(name) = words.next() else { continue };
                let mut sources = Vec::new();
                for word in words {
                    if sources.len() >= MAX_SOURCES || word.len() > MAX_SOURCE_LEN {
                        self.overflow = true;
                        break;
                    }
                    sources.push(word.to_string());
                }
                dirs.entry(name.to_ascii_lowercase()).or_insert(sources);
            }
            if !dirs.is_empty() {
                self.policies.push(dirs);
            }
        }
    }

    pub fn from_headers(url: &str, headers: &[String]) -> Self {
        let mut c = Self::new(url);
        for h in headers {
            c.add(h);
        }
        c
    }
    pub fn add_meta(&mut self, tree: &crate::dom::Node) {
        let mut metas = Vec::new();
        tree.walk(&mut metas, &|n| {
            n.element().is_some_and(|e| {
                e.tag == "meta"
                    && e.attrs
                        .get("http-equiv")
                        .is_some_and(|h| h.trim().eq_ignore_ascii_case("content-security-policy"))
            })
        });
        for m in metas {
            if let Some(v) = m.element().and_then(|e| e.attrs.get("content")) {
                self.add(v);
            }
        }
    }
    pub fn resource_sources(&self, dest: crate::net::Dest) -> Vec<Vec<String>> {
        let names: &[&str] = match dest {
            crate::net::Dest::Image => &["img-src", "default-src"],
            _ => &["style-src-elem", "style-src", "default-src"],
        };
        self.lists(names, |_| true)
    }
    pub fn resource(&self, dest: crate::net::Dest, url: &str) -> bool {
        self.resource_sources(dest)
            .iter()
            .all(|l| self.url_matches(l, url, false))
    }
    pub fn style_inline(&self, nonce: Option<&str>, code: &str, attribute: bool) -> bool {
        let names: &[&str] = if attribute {
            &["style-src-attr", "style-src", "default-src"]
        } else {
            &["style-src-elem", "style-src", "default-src"]
        };
        self.all(names, |list| {
            inline_allowed(list)
                || (!attribute && nonce.is_some_and(|n| !n.is_empty() && has_nonce(list, n)))
                || ((!attribute || has(list, "'unsafe-hashes'")) && hash_matches(list, code))
        })
    }
    pub fn style_link(&self, nonce: Option<&str>, url: &str) -> bool {
        self.all(&["style-src-elem", "style-src", "default-src"], |list| {
            nonce.is_some_and(|n| !n.is_empty() && has_nonce(list, n)) || self.url_matches(list, url, false)
        })
    }
    pub fn style_sources(&self, nonce: Option<&str>) -> Vec<Vec<String>> {
        self.lists(&["style-src-elem", "style-src", "default-src"], |l| {
            !nonce.is_some_and(|n| !n.is_empty() && has_nonce(l, n))
        })
    }

    pub fn is_empty(&self) -> bool {
        !self.overflow && self.policies.is_empty()
    }

    /// May a `<script>` run? `src` for an external one, else its `code`.
    /// `parser_inserted`: it was in the document as sent, rather than added
    /// by a script (which `'strict-dynamic'` trusts).
    pub fn script_element(
        &self,
        nonce: Option<&str>,
        src: Option<&str>,
        code: &str,
        parser_inserted: bool,
    ) -> bool {
        self.all(&["script-src-elem", "script-src", "default-src"], |list| {
            let nonce_ok = nonce.is_some_and(|n| !n.is_empty() && has_nonce(list, n));
            let dynamic = has(list, "'strict-dynamic'");
            match src {
                None => nonce_ok || hash_matches(list, code) || inline_allowed(list),
                Some(url) => {
                    nonce_ok
                        || (dynamic && !parser_inserted)
                        || (!dynamic && self.url_matches(list, url, false))
                }
            }
        })
    }

    /// May an inline event handler (`onclick="..."`) run?
    pub fn handler(&self, code: &str) -> bool {
        self.all(&["script-src-attr", "script-src", "default-src"], |list| {
            inline_allowed(list) || (has(list, "'unsafe-hashes'") && hash_matches(list, code))
        })
    }

    /// May scripts turn strings into code (`eval`, `new Function`, timers
    /// given a string)?
    pub fn eval(&self) -> bool {
        self.all(&["script-src", "default-src"], |list| has(list, "'unsafe-eval'"))
    }

    /// May scripts send a request (`fetch`, `XMLHttpRequest`, beacons) to
    /// `url`?
    pub fn connect(&self, url: &str) -> bool {
        self.all(&["connect-src", "default-src"], |list| {
            self.url_matches(list, url, false)
        })
    }

    /// The source lists a script request must match at each redirect hop,
    /// one per policy that restricts it: the browser follows redirects, so
    /// it checks them ([`redirect_allowed`]).
    pub fn connect_sources(&self) -> Vec<Vec<String>> {
        self.lists(&["connect-src", "default-src"], |_| true)
    }

    /// The same for an external `<script>` that [`Csp::script_element`]
    /// allowed: one let in by its nonce or `'strict-dynamic'` may go
    /// anywhere, the rest only where the list allows.
    pub fn script_sources(&self, nonce: Option<&str>, parser_inserted: bool) -> Vec<Vec<String>> {
        self.lists(&["script-src-elem", "script-src", "default-src"], |list| {
            let nonce_ok = nonce.is_some_and(|n| !n.is_empty() && has_nonce(list, n));
            !nonce_ok && !(has(list, "'strict-dynamic'") && !parser_inserted)
        })
    }

    fn lists(&self, names: &[&str], keep: impl Fn(&[String]) -> bool) -> Vec<Vec<String>> {
        if self.overflow {
            return vec![vec!["'none'".into()]];
        }
        self.policies
            .iter()
            .filter_map(|p| names.iter().find_map(|n| p.get(*n)))
            .filter(|list| keep(list))
            .cloned()
            .collect()
    }

    /// Every policy allows it: the first of `names` each has decides, and
    /// one with none of them doesn't restrict this.
    fn all(&self, names: &[&str], allows: impl Fn(&[String]) -> bool) -> bool {
        !self.overflow
            && self.policies.iter().all(|p| {
                names
                    .iter()
                    .find_map(|n| p.get(*n))
                    .is_none_or(|list| allows(list))
            })
    }

    /// `redirected`: `url` is where a redirect led, which only has to match
    /// a source's host, not its path (as in CSP, so paths don't leak).
    fn url_matches(&self, list: &[String], url: &str, redirected: bool) -> bool {
        let Ok(u) = url::Url::parse(url) else { return false };
        let scheme = u.scheme();
        let host = u.host_str().unwrap_or("").to_ascii_lowercase();
        let port = u.port_or_known_default();
        let page = self.page.as_ref();
        // Same scheme as the page, or a secure version of it.
        let scheme_ok = |want: &str| {
            scheme == want
                || (want == "http" && scheme == "https")
                || (want == "ws" && scheme == "wss")
                || (want == "http" && scheme == "wss")
                || (want == "https" && scheme == "wss")
        };
        list.iter().any(|tok| {
            let lower = tok.to_ascii_lowercase();
            if lower == "*" {
                return matches!(scheme, "http" | "https" | "ws" | "wss")
                    || page.is_some_and(|p| p.0 == scheme);
            }
            if lower == "'self'" {
                return page.is_some_and(|(ps, ph, pp)| {
                    *ph == host
                        && (scheme_ok(ps))
                        && (Some(*pp) == port || (ps == "http" && scheme == "https" && port == Some(443)))
                });
            }
            if lower.starts_with('\'') {
                return false; // keywords, nonces, hashes
            }
            if let Some(s) = lower.strip_suffix(':').filter(|s| !s.contains('/')) {
                return scheme_ok(s);
            }
            // [scheme://]host[:port][/path]
            let (tok_scheme, rest) = match lower.split_once("://") {
                Some((s, r)) => (Some(s), r),
                None => (None, lower.as_str()),
            };
            let scheme_fits = match tok_scheme {
                Some(s) => scheme_ok(s),
                None => page.map_or(matches!(scheme, "http" | "https"), |p| scheme_ok(&p.0)),
            };
            let (hostport, path) = match rest.find('/') {
                Some(i) => {
                    let original = tok.split_once("://").map_or(tok.as_str(), |(_, r)| r);
                    (&rest[..i], Some(&original[i..]))
                }
                None => (rest, None),
            };
            let (h, p) = match hostport.rsplit_once(':') {
                Some((h, p)) if !h.is_empty() && !p.contains(']') => (h, Some(p)),
                _ => (hostport, None),
            };
            let host_fits = match h.strip_prefix("*.") {
                Some(base) => host.ends_with(&format!(".{base}")),
                None => h == host,
            };
            let port_fits = match p {
                Some("*") => true,
                Some(n) => n.parse::<u16>().ok() == port,
                // The default port for the URL's scheme (or an upgrade
                // from the page's).
                None => u.port().is_none() || (scheme == "https" && port == Some(443)),
            };
            let path_fits = redirected
                || path.is_none_or(|want| {
                    let have = u.path();
                    if want.ends_with('/') {
                        have.starts_with(want)
                    } else {
                        have == want
                    }
                });
            scheme_fits && host_fits && port_fits && path_fits
        })
    }
}

/// Lists from the page's renderer, at most this many, each at most
/// [`MAX_SOURCES`] long.
pub const MAX_LISTS: usize = MAX_POLICIES;

/// May a request the page at `page_url` made, under `lists` (from
/// [`Csp::connect_sources`] or [`Csp::script_sources`]), follow a redirect
/// to `url`? Checked by the browser, which follows redirects.
pub fn request_allowed(page_url: &str, lists: &[Vec<String>], url: &str) -> bool {
    let csp = Csp::new(page_url);
    lists.iter().all(|l| csp.url_matches(l, url, false))
}
pub fn redirect_allowed(page_url: &str, lists: &[Vec<String>], url: &str) -> bool {
    let csp = Csp::new(page_url);
    lists.iter().all(|list| csp.url_matches(list, url, true))
}

/// Does `code` match a `<script integrity>` value (Subresource
/// Integrity)? Only the strongest algorithm listed counts, any of its
/// hashes may match, and a value with no hash Brickle knows allows it.
pub fn integrity_matches(integrity: &str, code: &str) -> bool {
    integrity_matches_bytes(integrity, code.as_bytes())
}
pub fn integrity_matches_bytes(integrity: &str, bytes: &[u8]) -> bool {
    let rank = |t: &str| match t.get(..7).map(str::to_ascii_lowercase).as_deref() {
        Some("sha256-") => 1,
        Some("sha384-") => 2,
        Some("sha512-") => 3,
        _ => 0,
    };
    // Options after `?` are ignored.
    let tokens: Vec<&str> = integrity
        .split_ascii_whitespace()
        .take(MAX_SOURCES)
        .map(|t| t.split('?').next().unwrap_or(""))
        .filter(|t| rank(t) > 0)
        .collect();
    let Some(best) = tokens.iter().map(|t| rank(t)).max() else {
        return true;
    };
    tokens
        .iter()
        .filter(|t| rank(t) == best)
        .any(|t| hash_matches_bytes(&[format!("'{t}'")], bytes))
}

fn has(list: &[String], keyword: &str) -> bool {
    list.iter().any(|t| t.eq_ignore_ascii_case(keyword))
}

fn has_nonce(list: &[String], nonce: &str) -> bool {
    list.iter().any(|t| {
        t.len() > 8
            && t[..7].eq_ignore_ascii_case("'nonce-")
            && t.ends_with('\'')
            && t[7..t.len() - 1] == *nonce
    })
}

/// `'unsafe-inline'` counts only if there's no nonce, hash or
/// `'strict-dynamic'` in the list (as in CSP level 2 and later).
fn inline_allowed(list: &[String]) -> bool {
    has(list, "'unsafe-inline'")
        && !has(list, "'strict-dynamic'")
        && !list.iter().any(|t| {
            let t = t.to_ascii_lowercase();
            t.starts_with("'nonce-")
                || t.starts_with("'sha256-")
                || t.starts_with("'sha384-")
                || t.starts_with("'sha512-")
        })
}

/// Does `code`'s hash appear in the list?
fn hash_matches(list: &[String], code: &str) -> bool {
    hash_matches_bytes(list, code.as_bytes())
}
fn hash_matches_bytes(list: &[String], bytes: &[u8]) -> bool {
    use ring::digest;
    list.iter().any(|t| {
        let lower = t.to_ascii_lowercase();
        let alg = if lower.starts_with("'sha256-") {
            &digest::SHA256
        } else if lower.starts_with("'sha384-") {
            &digest::SHA384
        } else if lower.starts_with("'sha512-") {
            &digest::SHA512
        } else {
            return false;
        };
        let Some(want) = t.get(8..t.len().saturating_sub(1)).filter(|_| t.ends_with('\'')) else {
            return false;
        };
        let got = base64(digest::digest(alg, bytes).as_ref());
        // base64 or base64url, padded or not.
        let norm = |s: &str| -> String {
            s.trim_end_matches('=')
                .chars()
                .map(|c| match c {
                    '-' => '+',
                    '_' => '/',
                    c => c,
                })
                .collect()
        };
        norm(want) == norm(&got)
    })
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csp(v: &str) -> Csp {
        let mut c = Csp::new("https://site.example/app/");
        c.add(v);
        c
    }

    #[test]
    fn no_policy_allows_everything() {
        let c = Csp::new("https://a.example/");
        assert!(c.is_empty() && c.eval() && c.handler("x()") && c.connect("https://b.example/"));
        assert!(c.script_element(None, None, "x", true));
        // A policy without script directives doesn't restrict scripts.
        let c = csp("img-src 'none'; frame-ancestors 'none'");
        assert!(c.eval() && c.script_element(None, Some("https://cdn.example/a.js"), "", true));
    }

    #[test]
    fn script_sources() {
        let c = csp("script-src 'self' https://cdn.example *.trusted.example:8443 static.example/js/");
        let ok = |u: &str| c.script_element(None, Some(u), "", true);
        assert!(ok("https://site.example/a.js"));
        assert!(ok("https://cdn.example/lib.js"));
        assert!(ok("https://x.trusted.example:8443/a.js"));
        assert!(ok("https://static.example/js/app.js"));
        assert!(!ok("https://static.example/other.js"), "outside the path");
        assert!(!ok("https://trusted.example:8443/a.js"), "*. is subdomains only");
        assert!(!ok("https://evil.example/a.js"));
        assert!(!ok("http://cdn.example/lib.js"), "no downgrade");
        assert!(!ok("data:text/javascript,alert(1)"));
        // Inline needs 'unsafe-inline', a nonce or a hash.
        assert!(!c.script_element(None, None, "alert(1)", true));
        assert!(!c.handler("alert(1)"));
        assert!(!c.eval());
        assert!(!csp("default-src 'none'").script_element(None, None, "", true));
        assert!(csp("script-src *").script_element(None, Some("https://any.example/x.js"), "", true));
        assert!(!csp("script-src *").script_element(None, Some("data:,x"), "", true));
        assert!(csp("script-src https:").script_element(None, Some("https://any.example/x.js"), "", true));
    }

    #[test]
    fn nonces_hashes_and_unsafe_inline() {
        let c = csp("script-src 'nonce-r4nd0m' 'unsafe-inline'");
        assert!(c.script_element(Some("r4nd0m"), None, "x()", true));
        assert!(!c.script_element(Some("guess"), None, "x()", true));
        assert!(
            !c.script_element(None, None, "x()", true),
            "a nonce cancels 'unsafe-inline'"
        );
        assert!(!c.handler("x()"));
        assert!(c.script_element(Some("r4nd0m"), Some("https://anywhere.example/a.js"), "", true));
        // sha256 of "alert(1)", in base64 and base64url.
        let h = base64(ring::digest::digest(&ring::digest::SHA256, b"alert(1)").as_ref());
        let c = csp(&format!("script-src 'sha256-{h}'"));
        assert!(c.script_element(None, None, "alert(1)", true));
        assert!(!c.script_element(None, None, "alert(2)", true));
        let url_safe = h
            .replace('+', "-")
            .replace('/', "_")
            .trim_end_matches('=')
            .to_string();
        assert!(csp(&format!("script-src 'sha256-{url_safe}'")).script_element(None, None, "alert(1)", true));
        // Handlers: 'unsafe-inline', or 'unsafe-hashes' with a hash.
        assert!(csp("script-src 'unsafe-inline'").handler("go()"));
        assert!(csp(&format!("script-src 'unsafe-hashes' 'sha256-{h}'")).handler("alert(1)"));
        assert!(!csp(&format!("script-src 'sha256-{h}'")).handler("alert(1)"));
        // script-src-attr and -elem take over from script-src.
        let c = csp("script-src 'none'; script-src-attr 'unsafe-inline'");
        assert!(c.handler("go()") && !c.script_element(None, None, "go()", true));
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    #[test]
    fn strict_dynamic_trusts_what_trusted_scripts_add() {
        let c = csp("script-src 'nonce-n' 'strict-dynamic' https://cdn.example 'unsafe-inline'");
        assert!(c.script_element(None, Some("https://other.example/x.js"), "", false));
        assert!(
            !c.script_element(None, Some("https://cdn.example/x.js"), "", true),
            "hosts are ignored"
        );
        assert!(!c.script_element(None, None, "x()", true));
        assert!(c.script_element(Some("n"), None, "x()", true));
    }

    #[test]
    fn eval_connect_and_several_policies() {
        assert!(csp("script-src 'self' 'unsafe-eval'").eval());
        assert!(!csp("default-src 'self'").eval());
        let c = csp("default-src 'self'; connect-src 'self' https://api.example");
        assert!(c.connect("https://site.example/x") && c.connect("https://api.example/v1"));
        assert!(!c.connect("https://tracker.example/"));
        assert!(!csp("connect-src 'none'").connect("https://site.example/x"));
        // Each policy must allow it.
        let c = csp("connect-src *, connect-src 'self'");
        assert!(c.connect("https://site.example/x") && !c.connect("https://other.example/"));
        // Directive names ignore case; the first of a repeat wins.
        assert!(!csp("CONNECT-SRC 'none'; connect-src *").connect("https://site.example/"));
    }

    #[test]
    fn redirects_are_held_to_the_source_lists() {
        let page = "https://site.example/app/";
        let c =
            csp("script-src 'self' https://cdn.example/js/ 'nonce-n'; connect-src https://api.example/v1/");
        let lists = c.connect_sources();
        assert_eq!(lists.len(), 1);
        // After a redirect only the host counts, not the path.
        assert!(redirect_allowed(page, &lists, "https://api.example/elsewhere"));
        assert!(!redirect_allowed(page, &lists, "https://evil.example/v1/"));
        let scripts = c.script_sources(None, true);
        assert!(redirect_allowed(page, &scripts, "https://cdn.example/other.js"));
        assert!(redirect_allowed(page, &scripts, "https://site.example/x.js"));
        assert!(!redirect_allowed(page, &scripts, "https://evil.example/js/a.js"));
        // A script let in by its nonce may go anywhere.
        assert!(c.script_sources(Some("n"), true).is_empty());
        // So may one 'strict-dynamic' trusts, but not one from the document.
        let d = csp("script-src 'strict-dynamic' https://cdn.example");
        assert!(d.script_sources(None, false).is_empty());
        assert_eq!(d.script_sources(None, true).len(), 1);
        // No policy, nothing to check; every policy must allow it.
        assert!(Csp::new(page).connect_sources().is_empty());
        let two = csp("connect-src *, connect-src 'self'");
        assert!(!redirect_allowed(
            page,
            &two.connect_sources(),
            "https://other.example/"
        ));
    }

    #[test]
    fn integrity_hashes() {
        let code = "console.log('sri ok')";
        let good = "sha384-ILTwP2ca5vkltgnvohEsdfVLU99DuqBMCY9WHGHmJiohQJnSaRGlJPKZDXRWuvAy";
        assert!(integrity_matches(good, code));
        assert!(integrity_matches(
            &format!("{good}?ct=application/javascript"),
            code
        ));
        assert!(!integrity_matches(good, "console.log('evil')"));
        // Only the strongest algorithm counts.
        assert!(!integrity_matches(&format!("{good} sha512-AAAA"), code));
        assert!(integrity_matches(&format!("{good} sha256-AAAA"), code));
        // Nothing Brickle knows: not checked (as in browsers).
        assert!(integrity_matches("md5-abc", code));
        assert!(integrity_matches("", code));
        assert!(integrity_matches("é€é€é€é€", code));
    }

    #[test]
    fn hostile_policies_are_bounded() {
        let mut c = Csp::new("https://a.example/");
        c.add(&",script-src 'none'".repeat(10_000));
        assert!(c.policies.len() <= MAX_POLICIES);
        c.add(&format!("connect-src {}", "x ".repeat(100_000)));
        for junk in [
            "",
            ";;;",
            "'nonce-'",
            "script-src 'sha256-'",
            "script-src :::// *.:/",
            "\u{0}\u{ffff}",
        ] {
            let c = csp(junk);
            let _ = c.script_element(Some(""), Some("not a url"), "", true);
            let _ = c.connect("::");
            let _ = c.handler("");
        }
        assert!(!csp("script-src 'nonce-'").script_element(Some(""), None, "", true));
    }
}
