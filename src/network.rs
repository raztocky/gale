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

use crate::filter::{self, Filters, Req, Verdict};
use crate::net::{self, Dest};
use crate::page::error_page;
use crate::urlclean;
use std::collections::HashSet;
use std::path::PathBuf;

const MAX_REDIRECTS: usize = 5;
/// Most external stylesheets one page may pull in.
pub const MAX_STYLESHEETS: usize = 16;

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
    /// Too many stylesheets on one page.
    OverLimit,
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
}

pub enum Nav {
    Page {
        url: String,
        html: String,
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
    Done { url: String, body: String },
    Stopped(String),
    NeedsHttp { host: String, url: String },
    FilterBlocked { host: String, url: String, rule: String },
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
    log: Vec<LogEntry>,
    page_stylesheets: usize,
    pub lists_loaded: usize,
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
            log: Vec::new(),
            page_stylesheets: 0,
            lists_loaded: 0,
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
        n
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
        self.page_stylesheets = 0;
        match self.fetch(url, from, Dest::Document) {
            Fetch::Done { url, body } => Nav::Page { url, html: body },
            Fetch::Stopped(why) => Nav::Page {
                url: url.to_string(),
                html: error_page(url, &why),
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
    pub fn subresource(&mut self, page_url: &str, url: &str) -> Result<String, String> {
        self.page_stylesheets += 1;
        if self.page_stylesheets > MAX_STYLESHEETS {
            self.record(url, Dest::Stylesheet, Outcome::OverLimit, vec![]);
            return Err(format!("too many stylesheets, skipped {url}"));
        }
        match self.fetch(url, Some(page_url), Dest::Stylesheet) {
            Fetch::Done { body, .. } => Ok(body),
            Fetch::Stopped(why) => Err(why),
            Fetch::NeedsHttp { url, .. } => Err(format!("blocked insecure {url}")),
            Fetch::FilterBlocked { url, .. } => Err(format!("blocked {url}")),
        }
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
        });
        self.log.last_mut().unwrap()
    }

    fn fetch(&mut self, url: &str, initiator: Option<&str>, dest: Dest) -> Fetch {
        let fdest = match dest {
            Dest::Document => filter::Dest::Document,
            _ => filter::Dest::Stylesheet,
        };
        let page = if dest == Dest::Document { None } else { initiator };
        let mut url = url.to_string();
        let mut from = initiator.map(str::to_string);
        for _ in 0..=MAX_REDIRECTS {
            // 1. Strip tracking parameters.
            let extra = self.filters.removeparam(&url, page, fdest);
            let (clean, stripped) = urlclean::strip_params(&url, &|k| {
                urlclean::is_tracking_param(k) || extra.contains(&k.to_ascii_lowercase())
            });
            url = clean;

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
                return match net::read_file(&url) {
                    Ok(body) => Fetch::Done { url, body },
                    Err(e) => Fetch::Stopped(e),
                };
            }

            // 3 + 4. Your rules, then the filter lists.
            let host = filter::host_of(&url);
            let allowed_by = match self.decide(&url, page, dest) {
                Decision::Denied(rule) => {
                    self.record(&url, dest, Outcome::Denied { rule: rule.clone() }, stripped);
                    return Fetch::Stopped(format!(
                        "You blocked {host} (rule <code>{}</code>). Open the request panel (F12) to unblock it.",
                        esc(&rule)
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
                target = format!("https://{}", &url["http://".len()..]);
                upgraded = true;
            }
            let insecure = net::scheme(&target) == "http" && !net::is_local_host(&host);
            if insecure && page.is_some_and(|p| net::scheme(p) == "https") {
                self.record(&url, dest, Outcome::MixedContent, stripped);
                return Fetch::Stopped(format!("blocked insecure {url} on a secure page"));
            }

            let result = net::http_get(&target, dest, from.as_deref());
            let entry_url = target.clone();
            match result {
                Ok(resp) if (300..400).contains(&resp.status) && resp.location.is_some() => {
                    let next = net::resolve(&target, resp.location.as_deref().unwrap_or(""));
                    let e = self.record(
                        &entry_url,
                        dest,
                        Outcome::Redirected { to: next.clone() },
                        stripped,
                    );
                    e.upgraded = upgraded;
                    e.insecure = insecure;
                    e.allowed_by = allowed_by;
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
                    e.upgraded = upgraded;
                    e.insecure = insecure;
                    e.allowed_by = allowed_by;
                    return Fetch::Done {
                        url: target,
                        body: resp.body,
                    };
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
                    self.record(&entry_url, dest, Outcome::Failed(err.clone()), stripped);
                    return Fetch::Stopped(format!("{target}: {err}"));
                }
            }
        }
        self.record(&url, dest, Outcome::Failed("too many redirects".into()), vec![]);
        Fetch::Stopped("too many redirects".into())
    }

    /// Apply your rules, then the filter lists, to one request.
    pub fn decide(&self, url: &str, page: Option<&str>, dest: Dest) -> Decision {
        let fdest = if dest == Dest::Document {
            filter::Dest::Document
        } else {
            filter::Dest::Stylesheet
        };
        let req = Req::new(url, page, fdest);
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

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A warning page with a single "continue" link. The `brickle:proceed` link
/// only works from an interstitial the browser itself generated.
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
        let before = net::requests_sent();
        let r = n.subresource(
            "https://news.example/",
            "https://tracker.example/t.css?utm_source=x",
        );
        assert!(r.is_err());
        assert_eq!(net::requests_sent(), before);
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
            .subresource("https://a.example/", "https://cdn.example/x.css")
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
            .subresource("https://evil.example/", "file:///etc/passwd")
            .is_err());
        assert_eq!(n.take_log()[0].outcome, Outcome::NotAllowed);
        match n.navigate("file:///etc/passwd", Some("https://evil.example/")) {
            Nav::Page { html, .. } => assert!(html.contains("aren't allowed")),
            Nav::Interstitial { .. } => panic!(),
        }
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
        };
        assert_eq!(n.toggle_preview(&e), ("||cdn.example^".into(), "block"));
        n.toggle(&e).unwrap();
        assert!(n
            .subresource("https://a.example/", "https://cdn.example/x.css")
            .is_err());
        let denied = n.take_log().remove(0);
        assert_eq!(n.toggle_preview(&denied).1, "unblock");
        n.toggle(&denied).unwrap();
        assert!(n.user_rules.is_empty());
    }
}
