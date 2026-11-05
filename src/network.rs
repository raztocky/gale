//! The browser's network service: every request Brickle makes goes through
//! here, so this is where the privacy rules are enforced and logged.
//!
//! For each request, in order:
//! 1. Tracking parameters are stripped from the URL.
//! 2. Scheme policy: web pages can't load `file://`.
//! 3. Your rules (`~/.config/brickle/rules.txt`): deny or allow a host.
//! 4. Filter lists (EasyList, EasyPrivacy, uBlock filters).
//! 5. HTTPS-only: `http://` is upgraded. If the site has no HTTPS, a page
//!    asks before falling back; a subresource of an HTTPS page is refused
//!    (mixed content).
//! 6. Redirects are followed one hop at a time, each hop re-checked from 1.
//!
//! Every step is recorded in the request log, shown in the request panel.

use crate::cookies::{self, Jar};
use crate::filter::{self, Filters, Req, Verdict};
use crate::forms::Submission;
use crate::net::{self, Cancel, Credentials, Dest, Mode, NetError, Redirect, ScriptRequest};
use crate::page::error_page;
use crate::urlclean;
use std::collections::HashSet;
use std::path::PathBuf;

const MAX_REDIRECTS: usize = 5;
/// Most `Content-Security-Policy` headers kept for a page, and the longest.
const MAX_CSP_HEADERS: usize = 8;
const MAX_CSP_HEADER: usize = 16 * 1024;
/// Most external stylesheets one page may pull in.
pub const MAX_STYLESHEETS: usize = 16;
/// Total time a page's subresources may take, however many there are.
pub const PAGE_TIME_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
/// Total bytes a page and its subresources may download.
pub const PAGE_BYTE_BUDGET: usize = 32 * 1024 * 1024;
/// Images per page.
pub const MAX_IMAGES: usize = 300;
/// A page's images in total (separate from [`PAGE_BYTE_BUDGET`]).
pub const IMAGE_BYTE_BUDGET: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Fetched over the network.
    Loaded {
        status: u16,
    },
    /// A local file.
    Local,
    Redirected {
        to: String,
    },
    /// Blocked by a filter list rule.
    Blocked {
        rule: String,
    },
    /// Blocked by one of your rules.
    Denied {
        rule: String,
    },
    /// HTTP subresource on an HTTPS page.
    MixedContent,
    /// Site has no HTTPS; waiting for you to allow plain HTTP.
    NeedsHttpConsent,
    /// Not an allowed scheme, or a web page reaching for `file://`.
    NotAllowed,
    /// Over the page's stylesheet count, time or size budget.
    OverLimit,
    /// A public website reaching for your local network (router, printer, ...).
    LocalNetwork,
    /// The server's certificate failed verification.
    BadCertificate(String),
    /// A page's script sending to another site (a request it made, or a
    /// file it put on the page). `allow` is the rule that would let it.
    CrossSite {
        allow: String,
    },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub url: String,
    pub dest: Dest,
    pub outcome: Outcome,
    /// Tracking parameters removed from the URL.
    pub stripped: Vec<String>,
    /// `http://` upgraded to `https://`.
    pub upgraded: bool,
    /// Loaded over plain HTTP after you allowed it.
    pub insecure: bool,
    /// An allow rule (yours or a filter exception) let this through.
    pub allowed_by: Option<String>,
    /// Names of the cookies sent with the request.
    pub cookies_sent: Vec<String>,
    /// Cookies the response set (or deleted).
    pub cookies_set: Vec<String>,
    /// Cookies the response tried to set but weren't stored, with why.
    pub cookies_refused: Vec<String>,
    /// A script's cross-origin request whose response the server didn't
    /// allow the page to read (CORS), so it was withheld from the page.
    pub cors_blocked: bool,
}

impl LogEntry {
    pub fn host(&self) -> String {
        filter::host_of(&self.url)
    }

    /// Did this request leave the browser?
    pub fn was_sent(&self) -> bool {
        matches!(self.outcome, Outcome::Loaded { .. } | Outcome::Redirected { .. })
            || matches!(self.outcome, Outcome::Failed(_) | Outcome::NeedsHttpConsent)
    }
}

/// What to do when you click "continue" on an interstitial page.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    AllowHttp { host: String, url: String },
    AllowBlocked { host: String, url: String },
    AllowLocal { host: String, url: String },
}

pub enum Nav {
    Page {
        url: String,
        html: String,
        /// The page's `Content-Security-Policy` headers, for its scripts.
        csp: Vec<String>,
    },
    Interstitial {
        url: String,
        html: String,
        action: Action,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// One of your rules blocks it.
    Denied(String),
    /// A filter list blocks it.
    Blocked(String),
    /// Go ahead (with the allow rule responsible, if any).
    Allowed(Option<String>),
}

enum Fetch {
    Done {
        url: String,
        body: String,
        /// An image's bytes.
        bytes: Vec<u8>,
        status: u16,
        headers: Vec<(String, String)>,
    },
    Stopped(String),
    NeedsHttp {
        host: String,
        url: String,
    },
    FilterBlocked {
        host: String,
        url: String,
        rule: String,
    },
    LocalNetwork {
        host: String,
        url: String,
    },
    Certificate {
        host: String,
        err: String,
    },
    /// A script request with `redirect: "manual"` was redirected: that's
    /// its answer, with nothing in it the page can read.
    OpaqueRedirect,
}

pub struct Network {
    filters: Filters,
    user: Filters,
    user_rules: Vec<String>,
    rules_path: Option<PathBuf>,
    /// Hosts you allowed over plain HTTP this session.
    http_allowed: HashSet<String>,
    /// Hosts you chose to visit despite a filter block this session.
    visit_allowed: HashSet<String>,
    /// Local-network hosts you chose to open from a website this session.
    local_allowed: HashSet<String>,
    log: Vec<LogEntry>,
    page_stylesheets: usize,
    page_scripts: usize,
    page_script_requests: usize,
    page_images: usize,
    image_bytes: usize,
    /// Sites where you allowed JavaScript (`~/.config/brickle/js-sites.txt`).
    js_sites: HashSet<String>,
    js_path: Option<PathBuf>,
    /// `--js`: allow JavaScript everywhere for this run.
    pub js_everywhere: bool,
    /// When the current page's loading must be finished, document,
    /// redirects and subresources together.
    deadline: std::time::Instant,
    /// Pressed by the window's Stop (Esc); set a fresh one per page.
    pub cancel: Cancel,
    page_bytes: usize,
    pub lists_loaded: usize,
    /// The cookie jar (see `cookies.rs` for the policy).
    pub cookies: Jar,
    /// The last page loaded was the answer to a POST (not redirected to a
    /// GET), so loading it again means sending the form again.
    document_posted: bool,
    /// CORS preflights that said yes, until when (for this session only).
    preflights: std::collections::HashMap<String, std::time::Instant>,
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("brickle"))
}

pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("brickle"))
}

impl Network {
    /// No filter lists, no saved rules (tests, `--no-filters`).
    pub fn empty() -> Network {
        Network {
            filters: Filters::default(),
            user: Filters::default(),
            user_rules: Vec::new(),
            rules_path: None,
            http_allowed: HashSet::new(),
            visit_allowed: HashSet::new(),
            local_allowed: HashSet::new(),
            log: Vec::new(),
            page_stylesheets: 0,
            page_scripts: 0,
            page_script_requests: 0,
            page_images: 0,
            image_bytes: 0,
            js_sites: HashSet::new(),
            js_path: None,
            js_everywhere: false,
            deadline: std::time::Instant::now() + PAGE_TIME_BUDGET,
            cancel: Cancel::new(),
            page_bytes: 0,
            lists_loaded: 0,
            cookies: Jar::new(),
            document_posted: false,
            preflights: Default::default(),
        }
    }

    pub fn with_filters(filters: Filters) -> Network {
        Network {
            filters,
            ..Network::empty()
        }
    }

    /// Load filter lists from the data directory and your rules file.
    pub fn from_disk() -> Network {
        let mut n = Network::empty();
        if let Some(dir) = data_dir().map(|d| d.join("filters")) {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
                paths.sort();
                for p in paths.iter().filter(|p| p.extension().is_some_and(|e| e == "txt")) {
                    if let Ok(text) = std::fs::read_to_string(p) {
                        n.filters.add_list(&text);
                        n.lists_loaded += 1;
                    }
                }
            }
        }
        if let Some(path) = config_dir().map(|d| d.join("rules.txt")) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                for line in text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('!'))
                {
                    n.user_rules.push(line.to_string());
                }
            }
            n.rules_path = Some(path);
        }
        n.rebuild_user();
        n.cookies = Jar::from_disk(config_dir().as_deref(), data_dir().as_deref());
        if let Some(path) = config_dir().map(|d| d.join("js-sites.txt")) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                n.js_sites = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(cookies::site)
                    .collect();
            }
            n.js_path = Some(path);
        }
        n
    }

    /// May pages on this URL run JavaScript? Off unless you allowed the
    /// site (or ran with `--js`). Only web pages ever run scripts.
    pub fn scripts_allowed(&self, url: &str) -> bool {
        matches!(net::scheme(url).as_str(), "http" | "https")
            && (self.js_everywhere || self.js_sites.contains(&cookies::site(url)))
    }

    pub fn js_sites(&self) -> Vec<String> {
        let mut v: Vec<String> = self.js_sites.iter().cloned().collect();
        v.sort();
        v
    }

    /// Allow or stop allowing JavaScript on a site, and save the list.
    pub fn set_scripts_allowed(&mut self, site_or_url: &str, allow: bool) -> Result<String, String> {
        let site = cookies::site(site_or_url);
        if site.is_empty() {
            return Err("not a website".into());
        }
        if allow {
            self.js_sites.insert(site.clone());
        } else {
            self.js_sites.remove(&site);
        }
        if let Some(path) = &self.js_path {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let mut text = String::from(
                "# Sites where Brickle runs JavaScript (one per line). Everywhere else it's off.\n",
            );
            for s in self.js_sites() {
                text.push_str(&s);
                text.push('\n');
            }
            std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(site)
    }

    pub fn set_user_rules(&mut self, rules: Vec<String>) {
        self.user_rules = rules;
        self.rebuild_user();
    }

    fn rebuild_user(&mut self) {
        let mut f = Filters::default();
        for r in &self.user_rules {
            f.add_rule(r);
        }
        self.user = f;
    }

    /// A fresh time and request budget for the current page (when it is
    /// loaded again without JavaScript).
    pub fn restart_page_budget(&mut self) {
        self.deadline = std::time::Instant::now() + PAGE_TIME_BUDGET;
        self.page_stylesheets = 0;
        self.page_scripts = 0;
        self.page_script_requests = 0;
        self.page_images = 0;
        self.image_bytes = 0;
        self.page_bytes = 0;
    }

    /// When the current page must have finished loading.
    pub fn deadline(&self) -> std::time::Instant {
        self.deadline
    }

    pub fn take_log(&mut self) -> Vec<LogEntry> {
        std::mem::take(&mut self.log)
    }

    pub fn cosmetic_css(&self, page_url: &str) -> String {
        if self
            .user
            .allows(&Req::new(page_url, None, filter::Dest::Document))
            .is_some()
        {
            return String::new(); // you turned blocking off for this site
        }
        self.filters.cosmetic_css(page_url)
    }

    pub fn filter_stats(&self) -> filter::Stats {
        self.filters.stats
    }

    /// Load a top-level page. `from` is the page that linked here, if any.
    pub fn navigate(&mut self, url: &str, from: Option<&str>) -> Nav {
        self.navigate_with(url, from, None)
    }

    /// Submit a form: a GET is a plain navigation to its URL; a POST sends
    /// its body (with your cookies, under the usual cookie rules).
    pub fn submit_form(&mut self, sub: &Submission, from: Option<&str>) -> Nav {
        if !sub.post {
            return self.navigate(&sub.url, from);
        }
        let req = ScriptRequest {
            method: "POST".into(),
            headers: sub
                .content_type
                .iter()
                .map(|c| ("content-type".to_string(), c.clone()))
                .collect(),
            body: sub.body.clone(),
            credentials: Credentials::Include,
            ..ScriptRequest::default()
        };
        self.navigate_with(&sub.url, from, Some(&req))
    }

    /// Whether the last page loaded was the answer to a POST.
    pub fn document_posted(&self) -> bool {
        self.document_posted
    }

    fn navigate_with(&mut self, url: &str, from: Option<&str>, post: Option<&ScriptRequest>) -> Nav {
        self.document_posted = false;
        self.page_stylesheets = 0;
        self.page_scripts = 0;
        self.page_script_requests = 0;
        self.page_images = 0;
        self.image_bytes = 0;
        self.deadline = std::time::Instant::now() + PAGE_TIME_BUDGET;
        self.page_bytes = 0;
        let fetched = self.fetch_with(url, from, Dest::Document, post, false);
        if let Err(e) = self.cookies.save() {
            eprintln!("brickle: saving cookies: {e}");
        }
        match fetched {
            Fetch::Done {
                url, body, headers, ..
            } => {
                self.page_bytes = body.len();
                let csp = headers
                    .into_iter()
                    .filter(|(k, v)| k == "content-security-policy" && v.len() <= MAX_CSP_HEADER)
                    .map(|(_, v)| v)
                    .take(MAX_CSP_HEADERS)
                    .collect();
                Nav::Page { url, html: body, csp }
            }
            Fetch::LocalNetwork { host, url } => Nav::Interstitial {
                html: interstitial(
                    "A website is sending you to your local network",
                    &format!(
                        "This link leads to <b>{}</b>, a device on your own network (such as a router or \
                         printer) rather than a website. Websites sometimes do this to change settings on \
                         your devices. Only continue if you expected this.",
                        esc(&host)
                    ),
                    "Open it anyway (this session)",
                ),
                action: Action::AllowLocal {
                    host,
                    url: url.clone(),
                },
                url,
            },
            Fetch::Certificate { host, err } if err.ends_with("Revoked") => Nav::Page {
                csp: vec![],
                html: interstitial_page(
                    "This site's certificate has been revoked",
                    &format!(
                        "The certificate presented by <b>{}</b> was revoked by the authority that issued \
                         it, usually because its private key was stolen or it was issued by mistake. \
                         Whoever is answering may not be the real site, so Brickle didn't load it.",
                        esc(&host)
                    ),
                ),
                url: url.to_string(),
            },
            Fetch::Certificate { host, err } => Nav::Page {
                csp: vec![],
                html: interstitial_page(
                    "Brickle couldn't verify this site's identity",
                    &format!(
                        "The certificate presented by <b>{}</b> isn't valid (<code>{}</code>). Someone \
                         may be impersonating the site or intercepting your connection, so Brickle \
                         didn't load it, and won't fall back to an insecure connection.",
                        esc(&host),
                        esc(&err)
                    ),
                ),
                url: url.to_string(),
            },
            Fetch::Stopped(why) => Nav::Page {
                url: url.to_string(),
                html: error_page(url, &why),
                csp: vec![],
            },
            // Only script requests stop at a redirect.
            Fetch::OpaqueRedirect => Nav::Page {
                url: url.to_string(),
                html: error_page(url, "redirected"),
                csp: vec![],
            },
            Fetch::NeedsHttp { host, url } => Nav::Interstitial {
                html: interstitial(
                    "This site doesn't support a secure connection",
                    &format!(
                        "Brickle is in HTTPS-only mode and couldn't load <b>{}</b> securely. \
                         If you continue, anyone on your network can read and change what you see \
                         and send on this site.",
                        esc(&host)
                    ),
                    "Continue to HTTP site (this session)",
                ),
                action: Action::AllowHttp {
                    host,
                    url: url.clone(),
                },
                url,
            },
            Fetch::FilterBlocked { host, url, rule } => Nav::Interstitial {
                html: interstitial(
                    "Brickle blocked this page",
                    &format!(
                        "<b>{}</b> is on a filter list (rule <code>{}</code>), usually because it serves \
                         ads or tracks you.",
                        esc(&host),
                        esc(&rule)
                    ),
                    "Visit anyway (this session)",
                ),
                action: Action::AllowBlocked {
                    host,
                    url: url.clone(),
                },
                url,
            },
        }
    }

    /// Fetch a stylesheet for the page at `page_url`. The page URL comes from
    /// the browser's own state, never from the renderer.
    pub fn subresource(&mut self, page_url: &str, url: &str, dest: Dest) -> Result<String, String> {
        self.subresource_from(page_url, url, dest, false)
    }

    /// A stylesheet or script a page's script put on the page: it may only
    /// come from the page's site (see [`Network::script_added`]), wherever
    /// it redirects.
    pub fn subresource_added(&mut self, page_url: &str, url: &str, dest: Dest) -> Result<String, String> {
        self.script_added(page_url, url, dest)?;
        self.subresource_from(page_url, url, dest, true)
    }

    fn subresource_from(
        &mut self,
        page_url: &str,
        url: &str,
        dest: Dest,
        added: bool,
    ) -> Result<String, String> {
        let why = match dest {
            Dest::Stylesheet => {
                self.page_stylesheets += 1;
                (self.page_stylesheets > MAX_STYLESHEETS).then_some("too many stylesheets")
            }
            Dest::Script if !self.scripts_allowed(page_url) => {
                // Decided here, from the browser's own record of the page,
                // whatever the renderer claims.
                self.record(url, dest, Outcome::NotAllowed, vec![]);
                return Err(format!("JavaScript is off for this site, skipped {url}"));
            }
            Dest::Script => {
                self.page_scripts += 1;
                (self.page_scripts > crate::js::MAX_SCRIPTS).then_some("too many scripts")
            }
            _ => {
                self.record(url, dest, Outcome::NotAllowed, vec![]);
                return Err(format!("not a subresource type, skipped {url}"));
            }
        };
        let why = if why.is_some() {
            why
        } else if std::time::Instant::now() >= self.deadline || self.cancel.is_cancelled() {
            Some("page took too long to load")
        } else if self.page_bytes > PAGE_BYTE_BUDGET {
            Some("page downloaded too much")
        } else {
            None
        };
        if let Some(why) = why {
            self.record(url, dest, Outcome::OverLimit, vec![]);
            return Err(format!("{why}, skipped {url}"));
        }
        match self.fetch_with(url, Some(page_url), dest, None, added) {
            Fetch::Done { body, .. } => {
                self.page_bytes += body.len();
                Ok(body)
            }
            Fetch::OpaqueRedirect => Err(format!("{url} redirected")),
            Fetch::LocalNetwork { url, .. } => Err(format!("blocked local-network request {url}")),
            Fetch::Stopped(why) => Err(why),
            Fetch::NeedsHttp { url, .. } => Err(format!("blocked insecure {url}")),
            Fetch::FilterBlocked { url, .. } => Err(format!("blocked {url}")),
            Fetch::Certificate { host, err } => Err(format!("{host}: {err}")),
        }
    }

    /// Scripts may only send to their page's site: a request a script made,
    /// or a file it put on the page (`added`), for another site is refused
    /// unless one of your allow rules lets it through. Same-site and
    /// `data:` addresses pass.
    pub fn script_added(&mut self, page_url: &str, url: &str, dest: Dest) -> Result<(), String> {
        let site = cookies::site(page_url);
        if url.starts_with("data:")
            || cookies::site(url) == site
            || self
                .user
                .allows(&Req::new(url, Some(page_url), filter_dest(dest)))
                .is_some()
        {
            return Ok(());
        }
        let kind = match dest {
            Dest::Xhr => "xhr",
            Dest::Image => "image",
            Dest::Script => "script",
            _ => "stylesheet",
        };
        let allow = format!("@@||{}^${kind},domain={site}", filter::host_of(url));
        self.record(url, dest, Outcome::CrossSite { allow }, vec![]);
        Err(format!(
            "scripts may only send to this page's site, skipped {url}"
        ))
    }

    /// Fetch an image for the page at `page_url` (the browser's own record
    /// of the page). Only its bytes come back: it is decoded in the renderer.
    /// `added`: a script put it on the page (see [`Network::script_added`]).
    pub fn image(&mut self, page_url: &str, url: &str, added: bool) -> Result<Vec<u8>, String> {
        if added {
            self.script_added(page_url, url, Dest::Image)?;
        }
        self.page_images += 1;
        let why = if self.page_images > MAX_IMAGES {
            Some("too many images")
        } else if std::time::Instant::now() >= self.deadline || self.cancel.is_cancelled() {
            Some("page took too long to load")
        } else if self.image_bytes > IMAGE_BYTE_BUDGET {
            Some("page's images are too large")
        } else {
            None
        };
        if let Some(why) = why {
            self.record(url, Dest::Image, Outcome::OverLimit, vec![]);
            return Err(format!("{why}, skipped {url}"));
        }
        match self.fetch_with(url, Some(page_url), Dest::Image, None, added) {
            Fetch::OpaqueRedirect => Err(format!("{url} redirected")),
            Fetch::Done { bytes, status, .. } if (200..300).contains(&status) => {
                self.image_bytes += bytes.len();
                Ok(bytes)
            }
            Fetch::Done { status, .. } => Err(format!("{url}: HTTP {status}")),
            Fetch::LocalNetwork { url, .. } => Err(format!("blocked local-network request {url}")),
            Fetch::Stopped(why) => Err(why),
            Fetch::NeedsHttp { url, .. } => Err(format!("blocked insecure {url}")),
            Fetch::FilterBlocked { url, .. } => Err(format!("blocked {url}")),
            Fetch::Certificate { host, err } => Err(format!("{host}: {err}")),
        }
    }

    /// A request from a page's script (`fetch`, `XMLHttpRequest`). The page
    /// URL is the browser's own record of the page, not the renderer's word.
    ///
    /// Requests to other sites are refused (see [`Network::script_added`]).
    /// Cross-origin ones that aren't "simple" are sent only after a CORS
    /// preflight says the server takes them, and the page only sees the
    /// response if the server allows it (`Access-Control-Allow-Origin`).
    /// Every other rule (your rules, the blocker, HTTPS-only, the
    /// local-network block, cookies) applies as for any request.
    pub fn script_request(
        &mut self,
        page_url: &str,
        url: &str,
        req: ScriptRequest,
    ) -> Result<ScriptResponse, String> {
        let dest = Dest::Xhr;
        if !self.scripts_allowed(page_url) {
            self.record(url, dest, Outcome::NotAllowed, vec![]);
            return Err("JavaScript is off for this site".into());
        }
        self.page_script_requests += 1;
        if self.page_script_requests > MAX_SCRIPT_REQUESTS {
            self.record(url, dest, Outcome::OverLimit, vec![]);
            return Err("too many requests from this page's scripts".into());
        }
        let mut req = req;
        req.method = req.method.to_ascii_uppercase();
        if !matches!(
            req.method.as_str(),
            "GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "PATCH" | "OPTIONS"
        ) {
            self.record(url, dest, Outcome::NotAllowed, vec![]);
            return Err(format!("method {} isn't allowed", req.method));
        }
        if matches!(req.method.as_str(), "GET" | "HEAD") {
            req.body = None;
        }
        if req.body.as_ref().is_some_and(|b| b.len() > MAX_SCRIPT_BODY) {
            self.record(url, dest, Outcome::OverLimit, vec![]);
            return Err("request body too large".into());
        }
        req.headers = allowed_script_headers(req.headers);
        let Some((target, _)) = net::canonical(url) else {
            self.record(url, dest, Outcome::NotAllowed, vec![]);
            return Err(format!("not a valid address: {url}"));
        };
        let target: String = target.into();
        self.script_added(page_url, &target, dest)?;
        let cross = !same_origin(page_url, &target);
        match req.mode {
            Mode::SameOrigin if cross => {
                self.record(&target, dest, Outcome::NotAllowed, vec![]);
                return Err(format!("{target} isn't this page's origin (mode: same-origin)"));
            }
            Mode::NoCors if !is_cors_simple(&req) => {
                self.record(&target, dest, Outcome::NotAllowed, vec![]);
                return Err("mode: no-cors allows only GET, HEAD and POST with simple headers".into());
            }
            _ => {}
        }
        // After the page has loaded, each script request gets its own time.
        let now = std::time::Instant::now();
        if now >= self.deadline {
            self.deadline = now + SCRIPT_REQUEST_TIME;
        }
        if cross && !is_cors_simple(&req) {
            self.preflight(page_url, &target, &req)?;
        }
        let creds = req.credentials;
        match self.fetch_with(&target, Some(page_url), dest, Some(&req), true) {
            Fetch::OpaqueRedirect => Ok(ScriptResponse {
                status: 0,
                url: target,
                redirected: false,
                headers: vec![],
                body: String::new(),
            }),
            Fetch::Done {
                url: final_url,
                body,
                status,
                headers,
                ..
            } => {
                self.page_bytes += body.len();
                let cross = !same_origin(page_url, &final_url);
                if cross {
                    let ok = cors_allows(&headers, &origin_of(page_url), creds == Credentials::Include);
                    if !ok {
                        if let Some(e) = self.log.last_mut() {
                            e.cors_blocked = true;
                        }
                        return Err(format!(
                            "CORS: {} didn't allow this page to read the response",
                            filter::host_of(&final_url)
                        ));
                    }
                }
                Ok(ScriptResponse {
                    redirected: final_url != target,
                    headers: exposed_headers(headers, cross, creds == Credentials::Include),
                    url: final_url,
                    status,
                    body,
                })
            }
            Fetch::LocalNetwork { url, .. } => Err(format!("blocked local-network request {url}")),
            Fetch::Stopped(why) => Err(why),
            Fetch::NeedsHttp { url, .. } => Err(format!("blocked insecure {url}")),
            Fetch::FilterBlocked { url, .. } => Err(format!("blocked {url}")),
            Fetch::Certificate { host, err } => Err(format!("{host}: {err}")),
        }
    }

    /// May a script request that `from` redirected go on to `next`? (`Err`
    /// with an empty reason: stop here and answer with the redirect, for
    /// `redirect: "manual"`.) `cors` says this hop was cross-origin, so its
    /// redirect response must allow the page (`Access-Control-Allow-Origin`)
    /// before it's followed.
    #[allow(clippy::too_many_arguments)]
    fn script_hop(
        &mut self,
        page_url: &str,
        from: &str,
        next: &str,
        req: &ScriptRequest,
        cors: bool,
        origin: &str,
        headers: &[(String, String)],
    ) -> Result<(), String> {
        match req.redirect {
            Redirect::Error => return Err(format!("{} redirected (redirect: error)", filter::host_of(from))),
            Redirect::Manual => return Err(String::new()),
            Redirect::Follow => {}
        }
        if cors && !cors_allows(headers, origin, req.credentials == Credentials::Include) {
            if let Some(e) = self.log.last_mut() {
                e.cors_blocked = true;
            }
            return Err(format!(
                "CORS: {} redirected without allowing this page",
                filter::host_of(from)
            ));
        }
        if req.mode == Mode::SameOrigin && !same_origin(page_url, next) {
            return Err(format!("{next} isn't this page's origin (mode: same-origin)"));
        }
        if url::Url::parse(next).is_ok_and(|u| !u.username().is_empty() || u.password().is_some()) {
            return Err("a script request was redirected to an address with a password".into());
        }
        Ok(())
    }

    /// Ask the server whether this page may send `req` (an `OPTIONS`
    /// request without cookies, through the same checks as any other).
    fn preflight(&mut self, page_url: &str, target: &str, req: &ScriptRequest) -> Result<(), String> {
        let origin = origin_of(page_url);
        let creds = req.credentials == Credentials::Include;
        let mut names: Vec<String> = req
            .headers
            .iter()
            .filter(|(k, v)| !cors_safelisted(k, v))
            .map(|(k, _)| k.to_ascii_lowercase())
            .collect();
        names.sort();
        names.dedup();
        let key = format!("{origin} {target} {creds} {} {}", req.method, names.join(","));
        let now = std::time::Instant::now();
        if self.preflights.get(&key).is_some_and(|until| *until > now) {
            return Ok(());
        }
        let mut headers = vec![("Access-Control-Request-Method".to_string(), req.method.clone())];
        if !names.is_empty() {
            headers.push(("Access-Control-Request-Headers".to_string(), names.join(",")));
        }
        let ask = ScriptRequest {
            method: "OPTIONS".into(),
            headers,
            body: None,
            credentials: Credentials::Omit,
            // A preflight that redirects fails, without following it.
            redirect: Redirect::Error,
            ..ScriptRequest::default()
        };
        let refused = |why: String| format!("CORS preflight: {} {why}", filter::host_of(target));
        let (status, got) = match self.fetch_with(target, Some(page_url), Dest::Xhr, Some(&ask), true) {
            Fetch::Done {
                url, status, headers, ..
            } if url == target => (status, headers),
            Fetch::Done { .. } => return Err(refused("redirected the preflight".into())),
            Fetch::Stopped(why) => return Err(why),
            _ => return Err(refused("couldn't be asked".into())),
        };
        let header = |name: &str| {
            got.iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| v.trim().to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let list = |name: &str| -> Vec<String> {
            header(name)
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let acao = header("access-control-allow-origin");
        let methods = list("access-control-allow-methods");
        let allowed_headers: Vec<String> = list("access-control-allow-headers")
            .into_iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        let why = if !(200..300).contains(&status) {
            Some(format!("answered HTTP {status}"))
        } else if !(acao == origin || (acao == "*" && !creds)) {
            Some("didn't allow this page".into())
        } else if creds && header("access-control-allow-credentials") != "true" {
            Some("didn't allow cookies".into())
        } else if !(matches!(req.method.as_str(), "GET" | "HEAD" | "POST")
            || methods.contains(&req.method)
            || (!creds && methods.iter().any(|m| m == "*")))
        {
            Some(format!("didn't allow {}", req.method))
        } else {
            names
                .iter()
                .find(|h| {
                    !allowed_headers.contains(h)
                        && !(!creds && *h != "authorization" && allowed_headers.iter().any(|a| a == "*"))
                })
                .map(|h| format!("didn't allow the {h} header"))
        };
        if let Some(why) = why {
            if let Some(e) = self.log.last_mut() {
                e.cors_blocked = true;
            }
            return Err(refused(why));
        }
        let age = header("access-control-max-age")
            .parse::<u64>()
            .unwrap_or(5)
            .min(7200);
        self.preflights
            .insert(key, now + std::time::Duration::from_secs(age));
        Ok(())
    }

    /// `document.cookie` for a page's scripts: its cookies, except HttpOnly
    /// ones.
    pub fn script_cookies(&mut self, page_url: &str) -> String {
        if !self.scripts_allowed(page_url) {
            return String::new();
        }
        self.cookies.script_string(page_url)
    }

    /// A page's script setting `document.cookie = value`.
    pub fn set_script_cookie(&mut self, page_url: &str, value: &str) -> cookies::Stored {
        if !self.scripts_allowed(page_url) {
            return cookies::Stored::default();
        }
        let stored = self.cookies.script_set(page_url, value);
        if let Err(e) = self.cookies.save() {
            eprintln!("brickle: saving cookies: {e}");
        }
        stored
    }

    pub fn allow_action(&mut self, action: &Action) -> String {
        match action {
            Action::AllowHttp { host, url } => {
                self.http_allowed.insert(host.clone());
                url.clone()
            }
            Action::AllowBlocked { host, url } => {
                self.visit_allowed.insert(host.clone());
                url.clone()
            }
            Action::AllowLocal { host, url } => {
                self.local_allowed.insert(host.clone());
                url.clone()
            }
        }
    }

    fn record(&mut self, url: &str, dest: Dest, outcome: Outcome, stripped: Vec<String>) -> &mut LogEntry {
        self.log.push(LogEntry {
            url: url.to_string(),
            dest,
            outcome,
            stripped,
            upgraded: false,
            insecure: false,
            allowed_by: None,
            cookies_sent: vec![],
            cookies_set: vec![],
            cookies_refused: vec![],
            cors_blocked: false,
        });
        self.log.last_mut().unwrap()
    }

    /// `script`: the method, headers, body and credentials of a page
    /// script's request (`dest` is then `Xhr`). `added`: a page's script
    /// asked for this (a request, or a file it put on the page), so every
    /// redirect hop must pass [`Network::script_added`] too, and a script
    /// request's hops get the checks its first request did (mode, CORS,
    /// preflight).
    fn fetch_with(
        &mut self,
        url: &str,
        initiator: Option<&str>,
        dest: Dest,
        script: Option<&ScriptRequest>,
        added: bool,
    ) -> Fetch {
        let fdest = filter_dest(dest);
        let mut method = script.map(|s| s.method.clone()).unwrap_or_else(|| "GET".into());
        let mut body = script.and_then(|s| s.body.clone());
        let mut script_headers: Vec<(String, String)> = script.map(|s| s.headers.clone()).unwrap_or_default();
        let page = if dest == Dest::Document { None } else { initiator };
        // After a redirect from one origin to another (neither the page's),
        // the request's origin is hidden: `Origin: null` (as in browsers).
        let mut tainted = false;
        // Once a script request has been cross-origin, it stays a CORS one.
        let mut was_cross = false;
        let mut url = url.to_string();
        let mut from = initiator.map(str::to_string);
        for _ in 0..=MAX_REDIRECTS {
            // 0. One parse, by the one URL parser; every check below sees
            //    its serialization. Credentials are dropped.
            let Some((parsed, had_credentials)) = net::canonical(&url) else {
                self.record(&url, dest, Outcome::NotAllowed, vec![]);
                return Fetch::Stopped(format!("Not a valid address: {url}"));
            };
            url = parsed.into();

            // 1. Strip tracking parameters.
            let extra = self.filters.removeparam(&url, page, fdest);
            let (clean, mut stripped) = urlclean::strip_params(&url, &|k| {
                urlclean::is_tracking_param(k) || extra.contains(&k.to_ascii_lowercase())
            });
            url = clean;
            if had_credentials {
                stripped.push("user:password@".into());
            }

            // 2. Scheme policy.
            if !net::may_load(from.as_deref(), &url) {
                self.record(&url, dest, Outcome::NotAllowed, stripped);
                return Fetch::Stopped(if net::scheme(&url) == "file" {
                    "Web pages aren't allowed to open files on your computer.".into()
                } else {
                    "Brickle can only open http, https and local file addresses.".into()
                });
            }
            if net::scheme(&url) == "file" {
                self.record(&url, dest, Outcome::Local, stripped);
                let read = if dest == Dest::Image {
                    net::read_file_bytes(&url).map(|b| (String::new(), b))
                } else {
                    net::read_file(&url).map(|s| (s, vec![]))
                };
                return match read {
                    Ok((body, bytes)) => Fetch::Done {
                        url,
                        body,
                        bytes,
                        status: 200,
                        headers: vec![],
                    },
                    Err(e) => Fetch::Stopped(e),
                };
            }

            // 3 + 4. Your rules, then the filter lists.
            let host = filter::host_of(&url);
            let allowed_by = match self.decide(&url, page, dest) {
                Decision::Denied(rule) => {
                    self.record(&url, dest, Outcome::Denied { rule: rule.clone() }, stripped);
                    return Fetch::Stopped(format!(
                        "You blocked {host} (rule {}). Open the request panel (F12) to unblock it.",
                        rule
                    ));
                }
                Decision::Blocked(rule) => {
                    self.record(&url, dest, Outcome::Blocked { rule: rule.clone() }, stripped);
                    return Fetch::FilterBlocked { host, url, rule };
                }
                Decision::Allowed(by) => by,
            };

            // 5. HTTPS-only.
            let mut target = url.clone();
            let mut upgraded = false;
            if net::scheme(&url) == "http" && !net::is_local_host(&host) && !self.http_allowed.contains(&host)
            {
                if let Ok(mut u) = url::Url::parse(&url) {
                    if u.set_scheme("https").is_ok() {
                        target = u.into();
                        upgraded = true;
                    }
                }
            }
            let insecure = net::scheme(&target) == "http" && !net::is_local_host(&host);
            if insecure && page.is_some_and(|p| net::scheme(p) == "https") {
                self.record(&url, dest, Outcome::MixedContent, stripped);
                return Fetch::Stopped(format!("blocked insecure {url} on a secure page"));
            }

            // A public page may not reach local-network addresses, unless
            // you said so for this host (navigations only).
            let public_only = from.as_deref().is_some_and(is_public_page)
                && !(dest == Dest::Document && self.local_allowed.contains(&host));
            let top = match (dest, page) {
                (Dest::Document, _) | (_, None) => target.clone(),
                (_, Some(p)) => p.to_string(),
            };
            let ctx = cookies::Context {
                top: &top,
                initiator: from.as_deref(),
                navigation: dest == Dest::Document,
                // This hop's method: a POST answered by 303 continues as a GET.
                safe_method: matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS" | "TRACE"),
            };
            // Script requests carry cookies per their `credentials`.
            let cookies_ok = match script.map(|s| s.credentials) {
                None | Some(Credentials::Include) => true,
                Some(Credentials::Omit) => false,
                Some(Credentials::SameOrigin) => page.is_some_and(|p| same_origin(p, &target)),
            };
            let (cookie, sent) = if cookies_ok {
                self.cookies.header_for(&target, ctx).unzip()
            } else {
                (None, None)
            };
            // A form POST's Origin is the page it was submitted from.
            let origin_page = if dest == Dest::Document { initiator } else { page };
            let page_origin = match origin_page {
                Some(p) if !tainted => origin_of(p),
                _ => "null".into(),
            };
            let cors =
                dest != Dest::Document && (was_cross || page.is_some_and(|p| !same_origin(p, &target)));
            was_cross |= cors;
            let outgoing = script.map(|_| net::Outgoing {
                method: &method,
                headers: &script_headers,
                body: body.as_deref(),
                // Firefox sends Origin on cross-origin requests and on
                // same-origin ones that aren't GET/HEAD.
                origin: (cors || !matches!(method.as_str(), "GET" | "HEAD")).then_some(page_origin.as_str()),
                cors,
            });
            let result = net::http_request_until(
                outgoing,
                &target,
                dest,
                from.as_deref(),
                public_only,
                cookie.as_deref(),
                self.deadline,
                &self.cancel,
            );
            let stored = match &result {
                Ok(_) if !cookies_ok => cookies::Stored::default(),
                Ok(resp) => {
                    let redirect = (300..400).contains(&resp.status);
                    self.cookies.store(&target, ctx, &resp.set_cookies, redirect)
                }
                Err(_) => cookies::Stored::default(),
            };
            let entry_url = target.clone();
            let sent = sent.unwrap_or_default();
            let note = |e: &mut LogEntry| {
                e.cookies_sent = sent.clone();
                e.cookies_set = stored.set.clone();
                e.cookies_refused = stored.refused.clone();
            };
            match result {
                Ok(resp) if (300..400).contains(&resp.status) && resp.location.is_some() => {
                    let next = net::resolve(&target, resp.location.as_deref().unwrap_or(""));
                    // What a page's script asked for gets each hop checked
                    // before it's sent, as its first request was.
                    let refused = match (script, page) {
                        (Some(s), Some(p)) if dest != Dest::Document => {
                            self.script_hop(p, &target, &next, s, cors, &page_origin, &resp.headers)
                        }
                        _ => Ok(()),
                    };
                    let refused = refused.and_then(|()| match page {
                        Some(p) if added && dest != Dest::Document => self.script_added(p, &next, dest),
                        _ => Ok(()),
                    });
                    let e = self.record(
                        &entry_url,
                        dest,
                        Outcome::Redirected { to: next.clone() },
                        stripped,
                    );
                    note(e);
                    e.upgraded = upgraded;
                    e.insecure = insecure;
                    e.allowed_by = allowed_by;
                    match refused {
                        Err(why) if why.is_empty() => return Fetch::OpaqueRedirect,
                        Err(why) => return Fetch::Stopped(why),
                        Ok(()) => {}
                    }
                    // 303, and 301/302 after a POST, continue as a GET.
                    if resp.status == 303 || (matches!(resp.status, 301 | 302) && method == "POST") {
                        method = "GET".into();
                        body = None;
                        // Without the body, its headers go too (as in Fetch).
                        script_headers.retain(|(k, _)| {
                            !matches!(
                                k.to_ascii_lowercase().as_str(),
                                "content-type" | "content-language" | "content-encoding" | "content-location"
                            )
                        });
                    }
                    if !same_origin(&target, &next) {
                        // Credentials a script set for one origin never go
                        // to another.
                        script_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
                        tainted |= page.is_some_and(|p| !same_origin(p, &target));
                    }
                    // The next hop of a request that needs a preflight gets
                    // its own, before anything is sent there.
                    if let (Some(s), Some(p)) = (script, page) {
                        let now = ScriptRequest {
                            method: method.clone(),
                            headers: script_headers.clone(),
                            body: body.clone(),
                            ..s.clone()
                        };
                        if dest != Dest::Document && !same_origin(p, &next) && !is_cors_simple(&now) {
                            if let Err(why) = self.preflight(p, &next, &now) {
                                return Fetch::Stopped(why);
                            }
                        }
                    }
                    from = Some(target);
                    url = next;
                }
                Ok(resp) => {
                    let e = self.record(
                        &entry_url,
                        dest,
                        Outcome::Loaded { status: resp.status },
                        stripped,
                    );
                    note(e);
                    e.upgraded = upgraded;
                    e.insecure = insecure;
                    e.allowed_by = allowed_by;
                    if dest == Dest::Document {
                        self.document_posted = method == "POST";
                    }
                    return Fetch::Done {
                        url: target,
                        body: resp.body,
                        bytes: resp.bytes,
                        status: resp.status,
                        headers: resp.headers,
                    };
                }
                Err(NetError::LocalNetwork) => {
                    self.record(&entry_url, dest, Outcome::LocalNetwork, stripped);
                    return if dest == Dest::Document {
                        Fetch::LocalNetwork { host, url }
                    } else {
                        Fetch::Stopped(format!("blocked local-network request {url}"))
                    };
                }
                Err(NetError::Certificate(err)) => {
                    // A certificate that fails verification may mean someone
                    // is intercepting the connection: never offer plain HTTP.
                    let e = self.record(&entry_url, dest, Outcome::BadCertificate(err.clone()), stripped);
                    e.upgraded = upgraded;
                    return Fetch::Certificate { host, err };
                }
                Err(err @ (NetError::Cancelled | NetError::TimedOut)) => {
                    let e = self.record(&entry_url, dest, Outcome::Failed(err.to_string()), stripped);
                    e.upgraded = upgraded;
                    return Fetch::Stopped(match err {
                        NetError::Cancelled => "Stopped.".into(),
                        _ => format!("{target}: the page took too long to load"),
                    });
                }
                Err(err) if upgraded => {
                    // No HTTPS: a subresource of an HTTP page you already
                    // allowed may fall back; anything else asks first.
                    if dest != Dest::Document
                        && page.is_some_and(|p| net::scheme(p) == "http")
                        && self.http_allowed.contains(&filter::host_of(page.unwrap_or("")))
                    {
                        self.http_allowed.insert(host.clone());
                        continue;
                    }
                    let e = self.record(&entry_url, dest, Outcome::NeedsHttpConsent, stripped);
                    e.upgraded = true;
                    let _ = err;
                    return Fetch::NeedsHttp { host, url };
                }
                Err(err) => {
                    self.record(&entry_url, dest, Outcome::Failed(err.to_string()), stripped);
                    return Fetch::Stopped(format!("{target}: {err}"));
                }
            }
        }
        self.record(&url, dest, Outcome::Failed("too many redirects".into()), vec![]);
        Fetch::Stopped("too many redirects".into())
    }

    /// Apply your rules, then the filter lists, to one request.
    pub fn decide(&self, url: &str, page: Option<&str>, dest: Dest) -> Decision {
        let req = Req::new(url, page, filter_dest(dest));
        if let Some(rule) = self.user.blocks(&req) {
            return Decision::Denied(rule);
        }
        if let Some(rule) = self.user.allows(&req) {
            return Decision::Allowed(Some(rule));
        }
        if dest == Dest::Document && self.visit_allowed.contains(&filter::host_of(url)) {
            return Decision::Allowed(None);
        }
        match self.filters.check(&req) {
            Verdict::Block(rule) => Decision::Blocked(rule),
            Verdict::Allow(rule) => Decision::Allowed(Some(rule)),
            Verdict::None => Decision::Allowed(None),
        }
    }

    /// The rule you'd add or remove by clicking this request in the panel,
    /// and a description of what clicking does.
    pub fn toggle_preview(&self, e: &LogEntry) -> (String, &'static str) {
        let host = e.host();
        match &e.outcome {
            Outcome::Denied { rule } if self.user_rules.contains(rule) => (rule.clone(), "unblock"),
            Outcome::Blocked { .. } => (format!("@@||{host}^"), "allow"),
            Outcome::CrossSite { allow } => (allow.clone(), "allow"),
            _ if e.allowed_by.as_ref().is_some_and(|r| self.user_rules.contains(r)) => {
                (e.allowed_by.clone().unwrap_or_default(), "stop allowing")
            }
            _ => (format!("||{host}^"), "block"),
        }
    }

    /// Apply a panel click: add or remove the rule and save your rules file.
    pub fn toggle(&mut self, e: &LogEntry) -> Result<String, String> {
        let (rule, verb) = self.toggle_preview(e);
        if e.host().is_empty() {
            return Err("local files can't be blocked".into());
        }
        if let Some(i) = self.user_rules.iter().position(|r| *r == rule) {
            self.user_rules.remove(i);
        } else {
            self.user_rules.push(rule.clone());
        }
        self.rebuild_user();
        self.save_rules()?;
        Ok(format!("{verb}: {rule}"))
    }

    fn save_rules(&self) -> Result<(), String> {
        let Some(path) = &self.rules_path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut text = String::from(
            "! Brickle: your own rules, in filter-list syntax. They override the filter lists.\n\
             ! ||example.com^  blocks a host;  @@||example.com^  allows it.\n",
        );
        for r in &self.user_rules {
            text.push_str(r);
            text.push('\n');
        }
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn filter_dest(dest: Dest) -> filter::Dest {
    match dest {
        Dest::Document => filter::Dest::Document,
        Dest::Script => filter::Dest::Script,
        Dest::Xhr => filter::Dest::Xhr,
        Dest::Image => filter::Dest::Image,
        _ => filter::Dest::Stylesheet,
    }
}

/// Most requests one page's scripts may make.
pub const MAX_SCRIPT_REQUESTS: usize = 200;
/// Largest body a script may send.
pub const MAX_SCRIPT_BODY: usize = 1024 * 1024;
/// Time for one script request made after the page loaded.
const SCRIPT_REQUEST_TIME: std::time::Duration = std::time::Duration::from_secs(20);

/// What a script's request returned, as the page may see it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptResponse {
    pub status: u16,
    /// The final URL, after redirects.
    pub url: String,
    pub redirected: bool,
    /// Response headers the page may read (never `Set-Cookie`).
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Headers a script may set on a request; the rest are dropped. Cookies,
/// origin, referrer, host and the like are the browser's to decide, and the
/// fingerprint-relevant ones stay as Firefox sends them.
fn allowed_script_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    const FORBIDDEN: &[&str] = &[
        "accept-charset",
        "accept-encoding",
        "connection",
        "content-length",
        "cookie",
        "cookie2",
        "date",
        "dnt",
        "expect",
        "host",
        "keep-alive",
        "origin",
        "referer",
        "set-cookie",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "via",
        "user-agent",
        "accept-language",
        "priority",
        "x-http-method",
        "x-http-method-override",
        "x-method-override",
    ];
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in headers {
        let name = k.trim().to_ascii_lowercase();
        let token = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
        if !token
            || FORBIDDEN.contains(&name.as_str())
            || name.starts_with("sec-")
            || name.starts_with("proxy-")
            || name.starts_with("access-control-")
            || v.len() > 4096
            || v.chars().any(|c| c == '\r' || c == '\n' || c == '\0')
            || out.len() >= 32
        {
            continue;
        }
        out.retain(|(n, _)| *n != name);
        out.push((name, v.trim().to_string()));
    }
    out
}

/// A cross-origin request that needs no preflight (Fetch spec "CORS-safelisted").
/// Does a cross-origin response let the page with `origin` read it?
fn cors_allows(headers: &[(String, String)], origin: &str, creds: bool) -> bool {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default()
    };
    let acao = header("access-control-allow-origin");
    if creds {
        acao == origin && header("access-control-allow-credentials") == "true"
    } else {
        acao == "*" || acao == origin
    }
}

fn is_cors_simple(req: &ScriptRequest) -> bool {
    matches!(req.method.as_str(), "GET" | "HEAD" | "POST")
        && req.headers.iter().all(|(k, v)| cors_safelisted(k, v))
}

/// A request header a page may send anywhere without a preflight.
fn cors_safelisted(name: &str, value: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "accept" | "accept-language" | "content-language" => value.len() <= 128,
        "content-type" => {
            let mime = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            matches!(
                mime.as_str(),
                "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
            )
        }
        _ => false,
    }
}

/// Response headers a page may read: for cross-origin responses only the
/// CORS-safelisted ones and those the server exposes.
fn exposed_headers(headers: Vec<(String, String)>, cross: bool, creds: bool) -> Vec<(String, String)> {
    const SAFE: &[&str] = &[
        "cache-control",
        "content-language",
        "content-length",
        "content-type",
        "expires",
        "last-modified",
        "pragma",
    ];
    let expose: Vec<String> = headers
        .iter()
        .filter(|(k, _)| k == "access-control-expose-headers")
        .flat_map(|(_, v)| {
            v.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .collect();
    let all = !creds && expose.iter().any(|e| e == "*");
    headers
        .into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "set-cookie" | "set-cookie2"))
        .filter(|(k, _)| !cross || all || SAFE.contains(&k.as_str()) || expose.contains(k))
        .collect()
}

/// `scheme://host:port` of a URL ("null" if it has none).
fn origin_of(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|_| "null".into())
}

fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin() && a.origin().is_tuple(),
        _ => false,
    }
}

/// A page served from the internet (not a local file or local device).
fn is_public_page(url: &str) -> bool {
    matches!(net::scheme(url).as_str(), "http" | "https") && !net::is_local_host(&filter::host_of(url))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A warning page with a single "continue" link. The `brickle:proceed` link
/// only works from an interstitial the browser itself generated.
/// A warning page with no way to continue.
fn interstitial_page(title: &str, body: &str) -> String {
    format!(
        "<title>{t}</title><body style='font-family: sans-serif; margin: 0; background: #fdf6e3'>\
         <div style='max-width: 640px; margin: 60px auto; padding: 0 24px; color: #333'>\
         <h1 style='color: #9a3412'>{t}</h1><p style='line-height: 1.5'>{body}</p></div></body>",
        t = esc(title)
    )
}

fn interstitial(title: &str, body: &str, proceed: &str) -> String {
    format!(
        "<title>{t}</title><body style='font-family: sans-serif; margin: 0; background: #fdf6e3'>\
         <div style='max-width: 640px; margin: 60px auto; padding: 0 24px; color: #333'>\
         <h1 style='color: #9a3412'>{t}</h1><p style='line-height: 1.5'>{body}</p>\
         <p style='margin-top: 32px'><a href='brickle:back' style='background: #1d4ed8; color: white; \
         padding: 8px 16px; text-decoration: none'>Go back</a> \
         &nbsp; <a href='brickle:proceed' style='color: #555'>{proceed}</a></p></div></body>",
        t = esc(title)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_requests_never_leave_the_browser() {
        let mut n = Network::with_filters(Filters::parse("||tracker.example^\n"));
        let r = n.subresource(
            "https://news.example/",
            "https://tracker.example/t.css?utm_source=x",
            Dest::Stylesheet,
        );
        assert!(r.is_err());
        assert_eq!(net::requests_sent_to("https://tracker.example/"), 0);
        let log = n.take_log();
        assert_eq!(log.len(), 1);
        assert!(matches!(log[0].outcome, Outcome::Blocked { .. }));
        assert_eq!(log[0].stripped, vec!["utm_source"]);
    }

    #[test]
    fn user_rules_override_filters() {
        let mut n = Network::with_filters(Filters::parse("||cdn.example^\n"));
        let css = |n: &Network| {
            n.decide(
                "https://cdn.example/x.css",
                Some("https://a.example/"),
                Dest::Stylesheet,
            )
        };
        assert_eq!(css(&n), Decision::Blocked("||cdn.example^".into()));
        n.set_user_rules(vec!["@@||cdn.example^".into()]);
        assert_eq!(css(&n), Decision::Allowed(Some("@@||cdn.example^".into())));
        // Your block always wins, even over your own allow.
        n.set_user_rules(vec!["@@||cdn.example^".into(), "||cdn.example^".into()]);
        assert_eq!(css(&n), Decision::Denied("||cdn.example^".into()));
        assert!(n
            .subresource(
                "https://a.example/",
                "https://cdn.example/x.css",
                Dest::Stylesheet
            )
            .is_err());
        assert!(matches!(n.take_log()[0].outcome, Outcome::Denied { .. }));
    }

    #[test]
    fn filter_blocked_document_shows_interstitial() {
        let mut n = Network::with_filters(Filters::parse("||ads.example^\n"));
        match n.navigate("https://ads.example/landing?fbclid=1", None) {
            Nav::Interstitial { url, action, .. } => {
                assert_eq!(url, "https://ads.example/landing");
                assert!(matches!(action, Action::AllowBlocked { .. }));
            }
            Nav::Page { .. } => panic!("should be blocked"),
        }
    }

    #[test]
    fn web_pages_cannot_load_local_files() {
        let mut n = Network::empty();
        assert!(n
            .subresource("https://evil.example/", "file:///etc/passwd", Dest::Stylesheet)
            .is_err());
        assert_eq!(n.take_log()[0].outcome, Outcome::NotAllowed);
        match n.navigate("file:///etc/passwd", Some("https://evil.example/")) {
            Nav::Page { html, .. } => assert!(html.contains("aren't allowed")),
            Nav::Interstitial { .. } => panic!(),
        }
    }

    #[test]
    fn websites_cannot_reach_the_local_network() {
        let mut n = Network::empty();
        for target in [
            "http://192.168.1.1/reboot.css",
            "http://localhost:8080/x.css",
            "http://[::1]/x.css",
        ] {
            assert!(n
                .subresource("https://evil.example/", target, Dest::Stylesheet)
                .is_err());
            assert_eq!(
                n.take_log().last().unwrap().outcome,
                Outcome::LocalNetwork,
                "{target}"
            );
        }
        for target in ["http://192.168.1.1/", "http://localhost/", "http://[::1]/"] {
            assert_eq!(net::requests_sent_to(target), 0, "nothing was sent to {target}");
        }
        match n.navigate("http://192.168.1.1/admin", Some("https://evil.example/")) {
            Nav::Interstitial {
                action: Action::AllowLocal { host, .. },
                ..
            } => assert_eq!(host, "192.168.1.1"),
            _ => panic!("expected the local-network warning"),
        }
    }

    #[test]
    fn page_time_budget() {
        let mut n = Network::empty();
        n.deadline = std::time::Instant::now() - std::time::Duration::from_secs(1);
        assert!(n
            .subresource(
                "https://late.example/",
                "https://late.example/late.css",
                Dest::Stylesheet
            )
            .unwrap_err()
            .contains("too long"));
        assert_eq!(n.take_log()[0].outcome, Outcome::OverLimit);
        assert_eq!(net::requests_sent_to("https://late.example/"), 0);
    }

    #[test]
    fn images_have_their_own_limits_and_rules() {
        let dir = std::env::temp_dir().join(format!("brickle-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        let img = format!("file://{}", file.display());
        let page = format!("file://{}/page.html", dir.display());
        let mut n = Network::empty();
        // A local page's images are read as bytes, untouched.
        assert_eq!(
            n.image(&page, &img, false).unwrap(),
            b"\x89PNG\r\n\x1a\nnot really"
        );
        // A website's are not.
        assert!(n.image("https://evil.example/", &img, false).is_err());
        assert_eq!(n.take_log().last().unwrap().outcome, Outcome::NotAllowed);
        // Past the per-page count, nothing more is fetched.
        n.page_images = MAX_IMAGES;
        assert!(n
            .image("https://budget.example/", "https://budget.example/x.png", false)
            .unwrap_err()
            .contains("too many images"));
        assert_eq!(n.take_log()[0].outcome, Outcome::OverLimit);
        // A new page starts again; its images get their own byte budget.
        n.restart_page_budget();
        n.image_bytes = IMAGE_BYTE_BUDGET + 1;
        assert!(n
            .image("https://budget.example/", "https://budget.example/x.png", false)
            .unwrap_err()
            .contains("too large"));
        assert_eq!(net::requests_sent_to("https://budget.example/"), 0);
        // Images can't come from the local network either.
        n.restart_page_budget();
        assert!(n
            .image("https://evil.example/", "http://192.168.1.1/cam.jpg", false)
            .is_err());
        assert_eq!(n.take_log().last().unwrap().outcome, Outcome::LocalNetwork);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panel_toggle_rules() {
        let mut n = Network::empty();
        let e = LogEntry {
            url: "https://cdn.example/x.css".into(),
            dest: Dest::Stylesheet,
            outcome: Outcome::Loaded { status: 200 },
            stripped: vec![],
            upgraded: false,
            insecure: false,
            allowed_by: None,
            cookies_sent: vec![],
            cookies_set: vec![],
            cookies_refused: vec![],
            cors_blocked: false,
        };
        assert_eq!(n.toggle_preview(&e), ("||cdn.example^".into(), "block"));
        n.toggle(&e).unwrap();
        assert!(n
            .subresource(
                "https://a.example/",
                "https://cdn.example/x.css",
                Dest::Stylesheet
            )
            .is_err());
        let denied = n.take_log().remove(0);
        assert_eq!(n.toggle_preview(&denied).1, "unblock");
        n.toggle(&denied).unwrap();
        assert!(n.user_rules.is_empty());
    }
}
