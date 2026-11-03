//! The cookie jar. It lives in the browser process only: sandboxed
//! renderers never see cookies.
//!
//! Policy (see PRIVACY.md, rule 8):
//! - Third-party cookies are never sent or stored: a request only carries
//!   cookies when it goes to the same site as the page you're on.
//! - Cookies are partitioned by the top-level site, so even a bug in the
//!   rule above couldn't share a cookie between two sites you visit.
//! - Everything is session-only (kept in memory, gone when Brickle closes),
//!   except cookies of sites you put on your keep list.
//! - Cookies set during a cross-site redirect (a "bounce" through a tracker
//!   on the way to the real page) are dropped.
//! - Without a `SameSite` attribute a cookie is `Lax`; lifetimes are capped
//!   at 400 days; `Secure` cookies only go over HTTPS.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Longest a cookie may live, as in Firefox and Chrome.
const MAX_AGE: u64 = 400 * 86_400;
/// Largest name + value.
const MAX_COOKIE_SIZE: usize = 4096;
const MAX_PER_SITE: usize = 180;
const MAX_TOTAL: usize = 3000;
/// Most `Set-Cookie` headers looked at in one response.
const MAX_SET_PER_RESPONSE: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lowercase host or domain, without a leading dot.
    pub domain: String,
    /// No `Domain` attribute: only sent to exactly this host.
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// Unix time; `None` = session cookie.
    pub expires: Option<u64>,
    /// The top-level site this cookie belongs to.
    pub partition: String,
    created: u64,
}

/// How a request relates to the page you're on.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The top-level page: for a navigation, the page being loaded.
    pub top: &'a str,
    /// The page or redirect hop that caused this request (None = you typed it).
    pub initiator: Option<&'a str>,
    /// Loading a page (as opposed to a stylesheet etc.).
    pub navigation: bool,
}

/// What happened to the cookies of one response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stored {
    pub set: Vec<String>,
    /// `name (reason)`.
    pub refused: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
    seq: u64,
    /// Sites whose cookies survive a restart.
    keep: HashSet<String>,
    /// Where kept cookies are saved (None: memory only).
    store_path: Option<PathBuf>,
    keep_path: Option<PathBuf>,
    dirty: bool,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_end_matches('.').to_ascii_lowercase()))
        .unwrap_or_default()
}

/// The registrable domain ("site") of a URL or host: `news.example.co.uk`
/// -> `example.co.uk`. IP addresses and single labels are their own site.
pub fn site(url_or_host: &str) -> String {
    let host = if url_or_host.contains("://") {
        host_of(url_or_host)
    } else {
        url_or_host.trim_end_matches('.').to_ascii_lowercase()
    };
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    psl::domain_str(&host).map(str::to_string).unwrap_or(host)
}

/// HTTPS, or plain HTTP to this computer itself (loopback), which never
/// crosses a network. Other local-network addresses (`192.168.x.x`,
/// `printer.local`) are *not* secure: anyone on that network can read and
/// change plain HTTP.
fn is_secure_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    match u.scheme() {
        "https" => true,
        "http" => match u.host() {
            Some(url::Host::Domain(d)) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                d == "localhost" || d.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

fn domain_match(host: &str, domain: &str) -> bool {
    host == domain || (host.ends_with(domain) && host[..host.len() - domain.len()].ends_with('.'))
}

fn path_of(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| "/".into())
}

/// RFC 6265 §5.1.4 path matching.
fn path_match(req: &str, cookie: &str) -> bool {
    req == cookie
        || (req.starts_with(cookie) && (cookie.ends_with('/') || req[cookie.len()..].starts_with('/')))
}

/// Default path: the request path up to (not including) its last `/`.
fn default_path(url: &str) -> String {
    let p = path_of(url);
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

/// Parse a cookie date (RFC 6265 §5.1.1) into Unix time.
pub fn parse_date(s: &str) -> Option<u64> {
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    let tokens = s.split(|c: char| !(c.is_ascii_alphanumeric() || c == ':'));
    for t in tokens.filter(|t| !t.is_empty()) {
        if time.is_none() {
            let parts: Vec<&str> = t.split(':').collect();
            if parts.len() == 3 {
                let num = |p: &str| {
                    let d: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
                    (!d.is_empty() && d.len() <= 2)
                        .then(|| d.parse::<u64>().ok())
                        .flatten()
                };
                if let (Some(h), Some(m), Some(s)) = (num(parts[0]), num(parts[1]), num(parts[2])) {
                    time = Some((h, m, s));
                    continue;
                }
            }
        }
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if day.is_none() && (1..=2).contains(&digits.len()) && digits.len() == t.len().min(2) {
            day = digits.parse::<u64>().ok();
            if day.is_some() {
                continue;
            }
        }
        if month.is_none() && t.len() >= 3 {
            const MONTHS: [&str; 12] = [
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
            ];
            if let Some(m) = MONTHS.iter().position(|m| t[..3].eq_ignore_ascii_case(m)) {
                month = Some(m as u64 + 1);
                continue;
            }
        }
        if year.is_none() && (2..=4).contains(&digits.len()) {
            year = digits.parse::<u64>().ok();
            continue;
        }
    }
    let (h, mi, sec) = time?;
    let (d, m, mut y) = (day?, month?, year?);
    if (70..=99).contains(&y) {
        y += 1900;
    } else if y <= 69 {
        y += 2000;
    }
    if !(1..=31).contains(&d) || y < 1601 || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let (y, m) = (y as i64 - (m <= 2) as i64, m as i64);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let t = days * 86_400 + (h * 3600 + mi * 60 + sec) as i64;
    Some(t.max(0) as u64)
}

/// Parse one `Set-Cookie` header received from `url`. Returns the cookie,
/// or why it was refused. Expired cookies are returned with `expires` in
/// the past (they delete existing ones).
fn parse(header: &str, url: &str, now: u64) -> Result<Cookie, (String, &'static str)> {
    let mut parts = header.split(';');
    let pair = parts.next().unwrap_or("");
    let (name, value) = match pair.split_once('=') {
        Some((n, v)) => (n.trim(), v.trim()),
        None => ("", pair.trim()),
    };
    let label = if name.is_empty() {
        "(unnamed)".to_string()
    } else {
        name.to_string()
    };
    let refuse = |why| Err((label.clone(), why));
    if name.is_empty() && value.is_empty() {
        return refuse("empty");
    }
    if name.len() + value.len() > MAX_COOKIE_SIZE {
        return refuse("too large");
    }
    if (name.to_string() + value).chars().any(|c| c.is_control()) {
        return refuse("invalid characters");
    }
    let host = host_of(url);
    if host.is_empty() {
        return refuse("no host");
    }
    let mut c = Cookie {
        name: name.to_string(),
        value: value.to_string(),
        domain: host.clone(),
        host_only: true,
        path: default_path(url),
        secure: false,
        http_only: false,
        same_site: SameSite::Lax,
        expires: None,
        partition: String::new(),
        created: 0,
    };
    let (mut max_age, mut expires) = (None, None);
    for attr in parts {
        let (k, v) = match attr.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attr.trim(), ""),
        };
        match k.to_ascii_lowercase().as_str() {
            "expires" => expires = parse_date(v),
            "max-age" => {
                if let Ok(n) = v.parse::<i64>() {
                    max_age = Some(n);
                }
            }
            "domain" if !v.is_empty() => {
                let d = v
                    .trim_start_matches('.')
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                let is_ip = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok();
                if !domain_match(&host, &d) || (is_ip && d != host) {
                    return refuse("domain doesn't match");
                }
                // A cookie for a whole public suffix (`.com`, `.co.uk`,
                // `.github.io`) would be a supercookie.
                if psl::suffix_str(&d) == Some(d.as_str()) {
                    if d != host {
                        return refuse("domain is a public suffix");
                    }
                } else {
                    c.host_only = false;
                    c.domain = d;
                }
            }
            "path" if v.starts_with('/') => c.path = v.to_string(),
            "secure" => c.secure = true,
            "httponly" => c.http_only = true,
            "samesite" => {
                c.same_site = match v.to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "none" => SameSite::None,
                    _ => SameSite::Lax,
                }
            }
            _ => {}
        }
    }
    c.expires = match (max_age, expires) {
        (Some(n), _) if n <= 0 => Some(0),
        (Some(n), _) => Some(now + (n as u64).min(MAX_AGE)),
        (None, Some(t)) => Some(t.min(now + MAX_AGE)),
        (None, None) => None,
    };
    if c.secure && !is_secure_url(url) {
        return refuse("Secure cookie over plain HTTP");
    }
    if c.same_site == SameSite::None && !c.secure {
        return refuse("SameSite=None without Secure");
    }
    if name.starts_with("__Secure-") && !c.secure {
        return refuse("__Secure- prefix without Secure");
    }
    if name.starts_with("__Host-") && (!c.secure || !c.host_only || c.path != "/") {
        return refuse("invalid __Host- cookie");
    }
    Ok(c)
}

impl Jar {
    pub fn new() -> Jar {
        Jar::default()
    }

    /// Load your keep list and the cookies saved for those sites.
    pub fn from_disk(config_dir: Option<&Path>, data_dir: Option<&Path>) -> Jar {
        let mut j = Jar::new();
        if let Some(p) = config_dir.map(|d| d.join("keep-cookies.txt")) {
            if let Ok(text) = std::fs::read_to_string(&p) {
                j.keep = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(site)
                    .collect();
            }
            j.keep_path = Some(p);
        }
        if let Some(p) = data_dir.map(|d| d.join("cookies.txt")) {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let t = now();
                for c in text.lines().filter_map(decode_line) {
                    if c.expires.is_some_and(|e| e > t) && j.keep.contains(&c.partition) {
                        j.seq += 1;
                        j.cookies.push(Cookie { created: j.seq, ..c });
                    }
                }
            }
            j.store_path = Some(p);
        }
        j
    }

    /// The `Cookie` header for a request, and the names it contains.
    pub fn header_for(&mut self, url: &str, ctx: Context) -> Option<(String, Vec<String>)> {
        self.cookie_string(url, ctx, true)
    }

    /// `document.cookie` for a page: its cookies as a request to the page
    /// itself would carry them, minus HttpOnly ones.
    pub fn script_string(&mut self, page_url: &str) -> String {
        let ctx = Context {
            top: page_url,
            initiator: Some(page_url),
            navigation: false,
        };
        self.cookie_string(page_url, ctx, false)
            .map(|(s, _)| s)
            .unwrap_or_default()
    }

    /// A page's script setting `document.cookie = value`. Scripts can't
    /// create HttpOnly cookies or replace existing ones.
    pub fn script_set(&mut self, page_url: &str, value: &str) -> Stored {
        let ctx = Context {
            top: page_url,
            initiator: Some(page_url),
            navigation: false,
        };
        match parse(value, page_url, now()) {
            Ok(c) if c.http_only => Stored {
                set: vec![],
                refused: vec![format!("{} (scripts can't set HttpOnly cookies)", c.name)],
            },
            Ok(c)
                if self.cookies.iter().any(|o| {
                    o.http_only
                        && o.partition == site(page_url)
                        && o.name == c.name
                        && o.domain == c.domain
                        && o.path == c.path
                }) =>
            {
                Stored {
                    set: vec![],
                    refused: vec![format!("{} (scripts can't replace HttpOnly cookies)", c.name)],
                }
            }
            _ => self.store(page_url, ctx, &[value.to_string()], false),
        }
    }

    fn cookie_string(&mut self, url: &str, ctx: Context, http_only: bool) -> Option<(String, Vec<String>)> {
        let target = site(url);
        let top = site(ctx.top);
        if target.is_empty() || target != top {
            return None; // third party
        }
        let t = now();
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > t));
        let cross_site = ctx.initiator.is_some_and(|i| site(i) != target);
        let host = host_of(url);
        let path = path_of(url);
        let secure = is_secure_url(url);
        let mut matched: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.partition == top)
            .filter(|c| {
                if c.host_only {
                    c.domain == host
                } else {
                    domain_match(&host, &c.domain)
                }
            })
            .filter(|c| path_match(&path, &c.path))
            .filter(|c| !c.secure || secure)
            .filter(|c| http_only || !c.http_only)
            .filter(|c| match c.same_site {
                SameSite::Strict => !cross_site,
                SameSite::Lax => !cross_site || ctx.navigation,
                SameSite::None => true,
            })
            .collect();
        if matched.is_empty() {
            return None;
        }
        matched.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.created.cmp(&b.created)));
        let header = matched
            .iter()
            .map(|c| {
                if c.name.is_empty() {
                    c.value.clone()
                } else {
                    format!("{}={}", c.name, c.value)
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some((header, matched.iter().map(|c| c.name.clone()).collect()))
    }

    /// Store the `Set-Cookie` headers of a response from `url`.
    /// `redirect` is true if the response was a redirect.
    pub fn store(&mut self, url: &str, ctx: Context, headers: &[String], redirect: bool) -> Stored {
        let mut out = Stored::default();
        if headers.is_empty() {
            return out;
        }
        let target = site(url);
        let top = site(ctx.top);
        let why = if target.is_empty() || target != top {
            Some("third-party")
        } else if redirect && ctx.navigation && ctx.initiator.is_some_and(|i| site(i) != target) {
            Some("cross-site redirect")
        } else {
            None
        };
        let t = now();
        for h in headers.iter().skip(MAX_SET_PER_RESPONSE) {
            let name = h.split(['=', ';']).next().unwrap_or("").trim();
            out.refused
                .push(format!("{name} (too many cookies in one response)"));
        }
        for h in headers.iter().take(MAX_SET_PER_RESPONSE) {
            let c = match parse(h, url, t) {
                Ok(c) => c,
                Err((name, reason)) => {
                    out.refused.push(format!("{name} ({reason})"));
                    continue;
                }
            };
            if let Some(why) = why {
                out.refused.push(format!("{} ({why})", c.name));
                continue;
            }
            // An insecure response may not replace, shadow or delete a
            // Secure cookie (RFC 6265bis §5.7): no `sid=attacker` over HTTP
            // in place of the real `sid` set over HTTPS.
            if !c.secure && !is_secure_url(url) {
                let clobbers = self.cookies.iter().any(|o| {
                    o.partition == top
                        && o.secure
                        && o.name == c.name
                        && (domain_match(&c.domain, &o.domain) || domain_match(&o.domain, &c.domain))
                        && path_match(&c.path, &o.path)
                });
                if clobbers {
                    out.refused.push(format!(
                        "{} (would overwrite a Secure cookie over plain HTTP)",
                        c.name
                    ));
                    continue;
                }
            }
            let replaced = self.cookies.iter().position(|o| {
                o.partition == top && o.name == c.name && o.domain == c.domain && o.path == c.path
            });
            let old = replaced.map(|i| self.cookies.remove(i));
            if self.keep.contains(&top) && (c.expires.is_some() || old.is_some()) {
                self.dirty = true;
            }
            if c.expires.is_some_and(|e| e <= t) {
                out.set.push(format!("{} (deleted)", c.name));
                continue;
            }
            self.seq += 1;
            out.set.push(c.name.clone());
            self.cookies.push(Cookie {
                partition: top.clone(),
                created: old.map(|o| o.created).unwrap_or(self.seq),
                ..c
            });
            self.evict(&top);
        }
        out
    }

    fn evict(&mut self, top: &str) {
        while self.cookies.iter().filter(|c| c.partition == top).count() > MAX_PER_SITE {
            self.remove_oldest(|c| c.partition == top);
        }
        while self.cookies.len() > MAX_TOTAL {
            self.remove_oldest(|_| true);
        }
    }

    fn remove_oldest(&mut self, pred: impl Fn(&Cookie) -> bool) {
        if let Some(i) = self
            .cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| pred(c))
            .min_by_key(|(_, c)| c.created)
            .map(|(i, _)| i)
        {
            self.cookies.remove(i);
        }
    }

    /// Cookies stored for a site.
    pub fn for_site(&self, s: &str) -> Vec<&Cookie> {
        let s = site(s);
        self.cookies.iter().filter(|c| c.partition == s).collect()
    }

    pub fn sites(&self) -> Vec<String> {
        let mut v: Vec<String> = self.cookies.iter().map(|c| c.partition.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Delete every cookie of a site. Returns how many.
    pub fn forget(&mut self, s: &str) -> usize {
        let s = site(s);
        let before = self.cookies.len();
        self.cookies.retain(|c| c.partition != s);
        if self.keep.contains(&s) {
            self.dirty = true;
        }
        before - self.cookies.len()
    }

    pub fn is_kept(&self, s: &str) -> bool {
        self.keep.contains(&site(s))
    }

    pub fn kept(&self) -> Vec<String> {
        let mut v: Vec<String> = self.keep.iter().cloned().collect();
        v.sort();
        v
    }

    /// Add or remove a site from the keep list. Returns whether it's kept now.
    pub fn set_kept(&mut self, s: &str, keep: bool) -> Result<bool, String> {
        let s = site(s);
        if s.is_empty() {
            return Err("not a website".into());
        }
        if keep {
            self.keep.insert(s);
        } else {
            self.keep.remove(&s);
        }
        self.dirty = true;
        if let Some(p) = &self.keep_path {
            let mut text = String::from(
                "# Sites whose cookies Brickle keeps after you close it (one per line).\n\
                 # All other cookies are deleted when Brickle closes.\n",
            );
            for k in self.kept() {
                text.push_str(&k);
                text.push('\n');
            }
            write_private(p, &text)?;
        }
        self.save()?;
        Ok(keep)
    }

    /// Save the cookies of kept sites, if anything changed.
    pub fn save(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let Some(p) = &self.store_path else { return Ok(()) };
        let t = now();
        let mut text = String::from("# Brickle cookies for the sites in keep-cookies.txt\n");
        for c in &self.cookies {
            if self.keep.contains(&c.partition) && c.expires.is_some_and(|e| e > t) {
                text.push_str(&encode_line(c));
                text.push('\n');
            }
        }
        write_private(p, &text)
    }
}

/// Write a file only you can read (cookies are login credentials).
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let res = (|| {
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    res.map_err(|e| format!("{}: {e}", path.display()))
}

fn encode_line(c: &Cookie) -> String {
    let ss = match c.same_site {
        SameSite::Strict => "strict",
        SameSite::Lax => "lax",
        SameSite::None => "none",
    };
    let flags = format!(
        "{}{}{}",
        if c.host_only { "h" } else { "" },
        if c.secure { "s" } else { "" },
        if c.http_only { "o" } else { "" }
    );
    [
        c.partition.as_str(),
        &c.domain,
        &c.path,
        &flags,
        ss,
        &c.expires.unwrap_or(0).to_string(),
        &c.name,
        &c.value,
    ]
    .join("\t")
}

fn decode_line(l: &str) -> Option<Cookie> {
    if l.starts_with('#') {
        return None;
    }
    let f: Vec<&str> = l.splitn(8, '\t').collect();
    if f.len() != 8 {
        return None;
    }
    Some(Cookie {
        partition: f[0].into(),
        domain: f[1].into(),
        path: f[2].into(),
        host_only: f[3].contains('h'),
        secure: f[3].contains('s'),
        http_only: f[3].contains('o'),
        same_site: match f[4] {
            "strict" => SameSite::Strict,
            "none" => SameSite::None,
            _ => SameSite::Lax,
        },
        expires: Some(f[5].parse().ok()?),
        name: f[6].into(),
        value: f[7].into(),
        created: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nav<'a>(top: &'a str, from: Option<&'a str>) -> Context<'a> {
        Context {
            top,
            initiator: from,
            navigation: true,
        }
    }
    fn sub(top: &str) -> Context<'_> {
        Context {
            top,
            initiator: Some(top),
            navigation: false,
        }
    }
    fn set(j: &mut Jar, url: &str, ctx: Context, h: &[&str]) -> Stored {
        let h: Vec<String> = h.iter().map(|s| s.to_string()).collect();
        j.store(url, ctx, &h, false)
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(
            parse_date("Wednesday, 21-Oct-15 07:28:00 GMT"),
            Some(1_445_412_480)
        );
        assert_eq!(parse_date("Thu Jan  1 00:00:00 1970"), Some(0));
        assert_eq!(parse_date("nonsense"), None);
    }

    #[test]
    fn first_party_round_trip_and_attributes() {
        let mut j = Jar::new();
        let u = "https://www.example.com/a/b";
        let s = set(
            &mut j,
            u,
            nav(u, None),
            &[
                "sid=1; HttpOnly",
                "pref=dark; Domain=example.com; Path=/",
                "x=1; Secure",
            ],
        );
        assert_eq!(s.set, vec!["sid", "pref", "x"]);
        // sid defaults to path /a, host-only.
        let h = j.header_for(
            "https://www.example.com/a/c",
            nav("https://www.example.com/a/c", None),
        );
        assert_eq!(h.unwrap().0, "sid=1; x=1; pref=dark", "longest path first");
        let h = j.header_for(
            "https://shop.example.com/",
            nav("https://shop.example.com/", None),
        );
        assert_eq!(
            h.unwrap().0,
            "pref=dark",
            "domain cookie reaches subdomains, host-only doesn't"
        );
        // Secure cookies never go over HTTP.
        let h = j.header_for(
            "http://www.example.com/a/",
            nav("http://www.example.com/a/", None),
        );
        assert_eq!(h.unwrap().0, "sid=1; pref=dark");
        // Deleting.
        set(&mut j, u, nav(u, None), &["sid=; Max-Age=0"]);
        assert!(!j.for_site(u).iter().any(|c| c.name == "sid"));
    }

    #[test]
    fn third_parties_get_nothing() {
        let mut j = Jar::new();
        let page = "https://news.example/";
        let s = set(&mut j, "https://tracker.example/p.css", sub(page), &["id=123"]);
        assert!(s.set.is_empty() && s.refused[0].contains("third-party"));
        // Even a cookie tracker.example set as a first party isn't sent when
        // it's embedded elsewhere.
        let t = "https://tracker.example/";
        set(&mut j, t, nav(t, None), &["id=123; SameSite=None; Secure"]);
        assert!(j.header_for("https://tracker.example/p.css", sub(page)).is_none());
        assert!(j.header_for(t, nav(t, None)).is_some());
    }

    #[test]
    fn samesite_and_bounces() {
        let mut j = Jar::new();
        let u = "https://bank.example/";
        set(
            &mut j,
            u,
            nav(u, None),
            &["s=1; SameSite=Strict", "l=1", "n=1; SameSite=None; Secure"],
        );
        // Arriving from another site: Strict stays home.
        let h = j.header_for(u, nav(u, Some("https://evil.example/")));
        assert_eq!(h.unwrap().0, "l=1; n=1");
        // A redirect hop through a tracker can't set cookies.
        let hop = "https://bounce.example/r?to=bank";
        let h: Vec<String> = vec!["id=1".into()];
        let s = j.store(hop, nav(hop, Some("https://news.example/")), &h, true);
        assert!(s.refused[0].contains("cross-site redirect"));
        // A same-site redirect (login -> home) can.
        let s = j.store(u, nav(u, Some("https://bank.example/login")), &h, true);
        assert_eq!(s.set, vec!["id"]);
    }

    #[test]
    fn refuses_supercookies_and_bad_prefixes() {
        let mut j = Jar::new();
        let u = "https://a.github.io/";
        let s = set(
            &mut j,
            u,
            nav(u, None),
            &["x=1; Domain=github.io", "y=1; Domain=other.io"],
        );
        assert!(s.refused[0].contains("public suffix"), "{s:?}");
        assert!(s.refused[1].contains("doesn't match"), "{s:?}");
        let u = "http://example.org/";
        let s = set(
            &mut j,
            u,
            nav(u, None),
            &[
                "a=1; Secure",
                "__Secure-b=1",
                "__Host-c=1; Secure; Path=/",
                "d=1; SameSite=None",
            ],
        );
        assert_eq!(s.refused.len(), 4, "{s:?}");
    }

    #[test]
    fn lifetimes_are_capped() {
        let mut j = Jar::new();
        let u = "https://example.com/";
        set(
            &mut j,
            u,
            nav(u, None),
            &[
                "a=1; Max-Age=999999999",
                "b=1; Expires=Fri, 01 Jan 2100 00:00:00 GMT",
            ],
        );
        let limit = now() + MAX_AGE + 5;
        assert!(j.for_site(u).iter().all(|c| c.expires.unwrap() <= limit));
    }

    #[test]
    fn domain_and_path_boundaries() {
        let mut j = Jar::new();
        let u = "https://example.com/foo";
        set(
            &mut j,
            u,
            nav(u, None),
            &["d=1; Domain=example.com; Path=/foo", "h=1; Path=/"],
        );
        let send = |j: &mut Jar, url: &str| j.header_for(url, nav(url, None)).map(|h| h.0);
        // Same suffix but a different site: nothing (and a different
        // partition anyway).
        assert_eq!(send(&mut j, "https://evilexample.com/foo"), None);
        // Path "/foo" covers "/foo" and "/foo/..." but not "/foobar".
        assert_eq!(
            send(&mut j, "https://example.com/foo/x").as_deref(),
            Some("d=1; h=1")
        );
        assert_eq!(send(&mut j, "https://example.com/foobar").as_deref(), Some("h=1"));
        // Host-only cookies stay on their host; domain cookies reach subdomains.
        assert_eq!(send(&mut j, "https://a.example.com/foo").as_deref(), Some("d=1"));
        // A subdomain can't set a cookie for a sibling.
        let sub = "https://a.example.com/";
        let s = set(&mut j, sub, nav(sub, None), &["x=1; Domain=b.example.com"]);
        assert!(s.refused[0].contains("doesn't match"), "{s:?}");
        // IP hosts: no Domain tricks.
        let ip = "http://10.1.2.3/";
        let s = set(&mut j, ip, nav(ip, None), &["x=1; Domain=1.2.3"]);
        assert!(s.refused[0].contains("doesn't match"), "{s:?}");
    }

    #[test]
    fn http_only_and_injection() {
        let mut j = Jar::new();
        let u = "https://example.com/";
        set(&mut j, u, nav(u, None), &["sid=abc; HttpOnly"]);
        assert!(
            j.for_site(u)[0].http_only,
            "kept for when scripts can read cookies"
        );
        // Header injection and junk.
        let s = set(&mut j, u, nav(u, None), &["a=1\r\nX-Evil: 1", "=", "b=\u{7}"]);
        assert_eq!(s.set.len(), 0, "{s:?}");
        assert_eq!(s.refused.len(), 3, "{s:?}");
    }

    #[test]
    fn count_and_size_limits() {
        let mut j = Jar::new();
        let u = "https://example.com/";
        let big = format!("big={}", "x".repeat(MAX_COOKIE_SIZE));
        let s = set(&mut j, u, nav(u, None), &[&big]);
        assert!(s.refused[0].contains("too large"));
        // Floods are capped per response and per site, oldest evicted first.
        for round in 0..5 {
            let hs: Vec<String> = (0..100).map(|i| format!("c{round}_{i}=1")).collect();
            let s = j.store(u, nav(u, None), &hs, false);
            assert_eq!(s.set.len(), MAX_SET_PER_RESPONSE);
            assert_eq!(s.refused.len(), 100 - MAX_SET_PER_RESPONSE);
        }
        let kept = j.for_site(u);
        assert_eq!(kept.len(), MAX_PER_SITE);
        assert!(!kept.iter().any(|c| c.name == "c0_0"), "oldest went first");
        assert!(kept.iter().any(|c| c.name == "c4_49"));
        // And in total, across sites.
        for site_n in 0..20 {
            let u = format!("https://site{site_n}.example/");
            for round in 0..4 {
                let hs: Vec<String> = (0..MAX_SET_PER_RESPONSE)
                    .map(|i| format!("c{round}_{i}=1"))
                    .collect();
                j.store(&u, nav(&u, None), &hs, false);
            }
        }
        assert_eq!(j.cookies.len(), MAX_TOTAL, "20 sites x 180 > the total cap");
    }

    #[test]
    fn only_loopback_http_counts_as_secure() {
        for u in [
            "https://example.com/",
            "http://localhost:8080/",
            "http://app.localhost/",
            "http://127.0.0.1/",
            "http://[::1]/",
        ] {
            assert!(is_secure_url(u), "{u}");
        }
        for u in [
            "http://192.168.1.1/",
            "http://10.0.0.2/",
            "http://printer.local/",
            "http://[fd00::1]/",
            "http://example.com/",
            "file:///tmp/x",
        ] {
            assert!(!is_secure_url(u), "{u}");
        }
        // A Secure cookie a LAN device set over HTTPS never goes over its HTTP.
        let mut j = Jar::new();
        let s = "https://192.168.1.1/";
        set(&mut j, s, nav(s, None), &["sid=1; Secure"]);
        assert!(j
            .header_for("http://192.168.1.1/", nav("http://192.168.1.1/", None))
            .is_none());
        assert!(j.header_for(s, nav(s, None)).is_some());
        // And plain HTTP on the LAN can't set Secure cookies.
        let h = "http://192.168.1.1/";
        let r = set(&mut j, h, nav(h, None), &["x=1; Secure"]);
        assert!(r.refused[0].contains("Secure cookie over plain HTTP"), "{r:?}");
    }

    #[test]
    fn http_cannot_overwrite_secure_cookies() {
        let mut j = Jar::new();
        let https = "https://www.example.com/app/";
        set(
            &mut j,
            https,
            nav(https, None),
            &["sid=real; Secure; Path=/; Domain=example.com"],
        );
        let http = "http://www.example.com/app/";
        for attack in [
            "sid=attacker; Path=/",                            // same name, host-only, same path
            "sid=attacker; Path=/; Domain=example.com",        // exact same key
            "sid=attacker; Path=/app; Domain=www.example.com", // narrower domain and path
            "sid=attacker",                                    // default path /app shadows /
            "sid=; Max-Age=0; Path=/; Domain=example.com",     // deletion
        ] {
            let r = set(&mut j, http, nav(http, None), &[attack]);
            assert!(
                r.set.is_empty() && r.refused[0].contains("overwrite a Secure"),
                "{attack}: {r:?}"
            );
        }
        let c = j.for_site(https);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].value, "real");
        // Unrelated names are fine, and so is a path a Secure cookie
        // doesn't cover.
        let r = set(&mut j, http, nav(http, None), &["theme=dark"]);
        assert_eq!(r.set, vec!["theme"]);
        set(&mut j, https, nav(https, None), &["tok=1; Secure; Path=/admin"]);
        let r = set(&mut j, http, nav(http, None), &["tok=2; Path=/blog"]);
        assert_eq!(r.set, vec!["tok"], "different path: no overlap");
        // HTTPS may still replace it.
        let r = set(
            &mut j,
            https,
            nav(https, None),
            &["sid=new; Secure; Path=/; Domain=example.com"],
        );
        assert_eq!(r.set, vec!["sid"]);
    }

    #[test]
    fn scripts_see_and_set_only_non_http_only_cookies() {
        let mut j = Jar::new();
        let u = "https://example.com/";
        set(&mut j, u, nav(u, None), &["sid=secret; HttpOnly", "theme=dark"]);
        assert_eq!(j.script_string(u), "theme=dark");
        assert_eq!(j.script_set(u, "lang=en; Path=/").set, vec!["lang"]);
        assert!(j.script_set(u, "x=1; HttpOnly").refused[0].contains("HttpOnly"));
        assert!(j.script_set(u, "sid=stolen").refused[0].contains("replace HttpOnly"));
        assert_eq!(j.script_string(u), "theme=dark; lang=en");
        let h = j.header_for(u, nav(u, None)).unwrap().0;
        assert!(h.contains("sid=secret") && h.contains("lang=en"), "{h}");
    }

    #[test]
    fn only_kept_sites_are_saved() {
        let dir = std::env::temp_dir().join(format!("brickle-jar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut j = Jar::from_disk(Some(&dir), Some(&dir));
        let (a, b) = ("https://mail.example/", "https://shop.example/");
        j.set_kept(a, true).unwrap();
        set(&mut j, a, nav(a, None), &["login=1; Max-Age=3600", "tmp=1"]);
        set(&mut j, b, nav(b, None), &["cart=1; Max-Age=3600"]);
        j.save().unwrap();
        let text = std::fs::read_to_string(dir.join("cookies.txt")).unwrap();
        assert!(
            !dir.join("cookies.tmp").exists(),
            "saved atomically, no leftovers"
        );
        assert!(text.contains("login") && !text.contains("tmp") && !text.contains("cart"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("cookies.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // "Restart".
        let j2 = Jar::from_disk(Some(&dir), Some(&dir));
        assert_eq!(j2.for_site(a).len(), 1);
        assert!(j2.for_site(b).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
