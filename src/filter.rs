//! A content blocker that reads EasyList / uBlock Origin filter syntax.
//!
//! Supported:
//! - Network rules: `||host^`, `|https://…`, plain substrings, `*`, `^`,
//!   `|` end anchors, and `@@` exceptions.
//! - Options: `third-party`/`3p`, `first-party`/`1p`, `domain=`/`from=`,
//!   `document`, `stylesheet`/`css`, `script`, `xhr`, `image`, `all`, `important`, `match-case`,
//!   `removeparam=`, `generichide`/`elemhide` (exceptions), `badfilter`.
//! - Cosmetic rules: `##selector`, `example.com##selector`, `#@#` exceptions.
//! - Preprocessor: `!#if` / `!#else` / `!#endif` (evaluated as Firefox).
//!
//! Rules the engine can't apply faithfully (regex rules, `$redirect`,
//! `$csp`, scriptlets, procedural cosmetics...) are skipped rather than
//! approximated, so they never over-block. Rules that only target request
//! types Brickle never makes (`$font`, `$media`, `$websocket`...) are dropped too.

use std::collections::{HashMap, HashSet};

/// What kind of request is being checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    Document,
    Stylesheet,
    Script,
    /// `fetch`/`XMLHttpRequest` from a page's script.
    Xhr,
    Image,
}

const T_DOCUMENT: u8 = 1;
const T_STYLESHEET: u8 = 2;
/// Types Brickle never requests (font, media, ...).
const T_OTHER: u8 = 4;
const T_SCRIPT: u8 = 8;
const T_XHR: u8 = 16;
const T_IMAGE: u8 = 32;
const T_ALL: u8 = T_DOCUMENT | T_STYLESHEET | T_OTHER | T_SCRIPT | T_XHR | T_IMAGE;
/// Types Brickle requests, so rules for them matter.
const T_USED: u8 = T_DOCUMENT | T_STYLESHEET | T_SCRIPT | T_XHR | T_IMAGE;

#[derive(Debug, Clone)]
struct NetRule {
    tokens: Vec<Tok>,
    start: Anchor,
    end_anchor: bool,
    /// None = any party, Some(true) = third-party only, Some(false) = first-party only.
    third_party: Option<bool>,
    /// 0 = no type options given.
    types: u8,
    /// (domain, include) pairs from `domain=`.
    domains: Vec<(String, bool)>,
    important: bool,
    /// A bare `||host^` rule, which also blocks top-level documents
    /// ("strict blocking", as uBlock does).
    pure_host: bool,
    text: String,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Lit(Vec<u8>),
    Star,
    Sep,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Anchor {
    None,
    Start,
    Host,
}

#[derive(Debug, Clone, Default)]
struct RuleSet {
    /// Rules starting `||host^` indexed by host.
    by_host: HashMap<String, Vec<usize>>,
    /// Other rules indexed by one whole word that must appear in a matching
    /// URL (the same trick uBlock uses), so a check only looks at a handful
    /// of candidates instead of every rule.
    by_token: HashMap<Vec<u8>, Vec<usize>>,
    /// Rules with no usable token; always checked.
    others: Vec<usize>,
    rules: Vec<NetRule>,
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'%'
}

/// URL words common enough that indexing on them would be pointless.
const COMMON: &[&[u8]] = &[
    b"http", b"https", b"www", b"com", b"net", b"org", b"js", b"css", b"html",
];

/// The best whole word a rule's URL must contain, if any.
fn rule_token(r: &NetRule) -> Option<Vec<u8>> {
    let mut best: Option<Vec<u8>> = None;
    for (ti, t) in r.tokens.iter().enumerate() {
        let Tok::Lit(lit) = t else { continue };
        // A run touching the literal's edge is only a whole word if the
        // neighbour is a separator or an anchor (not `*` or nothing).
        let left_bounded = match ti.checked_sub(1).map(|i| &r.tokens[i]) {
            Some(Tok::Sep) => true,
            Some(_) => false,
            None => r.start != Anchor::None,
        };
        let right_bounded = match r.tokens.get(ti + 1) {
            Some(Tok::Sep) => true,
            Some(_) => false,
            None => r.end_anchor,
        };
        let mut i = 0;
        while i < lit.len() {
            if !is_word(lit[i]) {
                i += 1;
                continue;
            }
            let st = i;
            while i < lit.len() && is_word(lit[i]) {
                i += 1;
            }
            let whole = (st > 0 || left_bounded) && (i < lit.len() || right_bounded);
            let word = &lit[st..i];
            if whole
                && word.len() >= 2
                && !COMMON.contains(&word)
                && best.as_ref().is_none_or(|b| word.len() > b.len())
            {
                best = Some(word.to_vec());
            }
        }
    }
    best
}

impl RuleSet {
    fn add(&mut self, r: NetRule) {
        let idx = self.rules.len();
        match (&r.start, r.tokens.first(), r.tokens.get(1)) {
            (Anchor::Host, Some(Tok::Lit(h)), Some(Tok::Sep) | None)
                if h.iter()
                    .all(|c| c.is_ascii_alphanumeric() || *c == b'.' || *c == b'-') =>
            {
                self.by_host
                    .entry(String::from_utf8_lossy(h).into_owned())
                    .or_default()
                    .push(idx);
            }
            _ => match rule_token(&r) {
                Some(tok) => self.by_token.entry(tok).or_default().push(idx),
                None => self.others.push(idx),
            },
        }
        self.rules.push(r);
    }

    fn find(&self, req: &Req, important_only: bool) -> Option<&NetRule> {
        let mut host = req.host.as_str();
        loop {
            if let Some(ids) = self.by_host.get(host) {
                for &i in ids {
                    let r = &self.rules[i];
                    if (!important_only || r.important) && r.matches(req) {
                        return Some(r);
                    }
                }
            }
            match host.find('.') {
                Some(i) => host = &host[i + 1..],
                None => break,
            }
        }
        let ok = |r: &&NetRule| (!important_only || r.important) && r.matches(req);
        let u = req.url.as_bytes();
        let mut i = 0;
        while i < u.len() {
            if !is_word(u[i]) {
                i += 1;
                continue;
            }
            let st = i;
            while i < u.len() && is_word(u[i]) {
                i += 1;
            }
            if let Some(ids) = self.by_token.get(&u[st..i]) {
                if let Some(r) = ids.iter().map(|&i| &self.rules[i]).find(ok) {
                    return Some(r);
                }
            }
        }
        self.others.iter().map(|&i| &self.rules[i]).find(ok)
    }
}

/// A request as the matcher sees it.
pub struct Req {
    url: String,
    host: String,
    /// Host of the page making the request (the target itself for documents).
    page_host: String,
    third_party: bool,
    dest: Dest,
}

impl Req {
    pub fn new(url: &str, page_url: Option<&str>, dest: Dest) -> Req {
        let host = host_of(url);
        let page_host = page_url.map(host_of).unwrap_or_else(|| host.clone());
        let third_party = registrable(&host) != registrable(&page_host);
        Req {
            url: url.to_ascii_lowercase(),
            host,
            page_host,
            third_party,
            dest,
        }
    }
}

pub fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
        })
        .unwrap_or_default()
}

/// The registrable domain ("eTLD+1"), e.g. `news.bbc.co.uk` -> `bbc.co.uk`.
pub fn registrable(host: &str) -> String {
    psl::domain_str(host).unwrap_or(host).to_string()
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

impl NetRule {
    fn matches(&self, req: &Req) -> bool {
        let t = match req.dest {
            Dest::Document => T_DOCUMENT,
            Dest::Stylesheet => T_STYLESHEET,
            Dest::Script => T_SCRIPT,
            Dest::Xhr => T_XHR,
            Dest::Image => T_IMAGE,
        };
        let type_ok = if self.types == 0 {
            // Untyped rules apply to subresources; top-level documents are
            // blocked only by explicit `$document` or bare `||host^` rules.
            t != T_DOCUMENT || self.pure_host
        } else {
            self.types & t != 0
        };
        if !type_ok {
            return false;
        }
        if let Some(tp) = self.third_party {
            if tp != req.third_party {
                return false;
            }
        }
        if !self.domains.is_empty() {
            let mut included = None;
            let mut any_include = false;
            for (d, inc) in &self.domains {
                any_include |= *inc;
                if domain_matches(&req.page_host, d) {
                    // The most specific (longest) matching entry wins.
                    match included {
                        Some((len, _)) if len >= d.len() => {}
                        _ => included = Some((d.len(), *inc)),
                    }
                }
            }
            let ok = match included {
                Some((_, inc)) => inc,
                None => !any_include,
            };
            if !ok {
                return false;
            }
        }
        self.matches_url(&req.url)
    }

    fn matches_url(&self, url: &str) -> bool {
        let u = url.as_bytes();
        match self.start {
            Anchor::Start => glob(&self.tokens, u, 0, self.end_anchor),
            Anchor::None => (0..=u.len()).any(|i| glob(&self.tokens, u, i, self.end_anchor)),
            Anchor::Host => {
                // Candidate starts: the beginning of the host and each label in it.
                let Some(scheme_end) = url.find("://") else {
                    return false;
                };
                let host_start = scheme_end + 3;
                let host_end = url[host_start..]
                    .find(['/', '?', '#', ':'])
                    .map(|i| host_start + i)
                    .unwrap_or(url.len());
                std::iter::once(host_start)
                    .chain((host_start..host_end).filter(|&i| u[i] == b'.').map(|i| i + 1))
                    .any(|i| glob(&self.tokens, u, i, self.end_anchor))
            }
        }
    }
}

fn is_sep(c: u8) -> bool {
    !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'%'))
}

/// Match tokens against `text[start..]`. Classic wildcard matching with
/// single-star backtracking: O(len(text) * len(pattern)) worst case.
fn glob(toks: &[Tok], text: &[u8], start: usize, end_anchor: bool) -> bool {
    // Expand literals into single-byte units so `*` backtracking is simple.
    #[derive(Clone, Copy)]
    enum U {
        B(u8),
        Star,
        Sep,
    }
    let units: Vec<U> = toks
        .iter()
        .flat_map(|t| match t {
            Tok::Lit(b) => b.iter().map(|&c| U::B(c)).collect::<Vec<_>>(),
            Tok::Star => vec![U::Star],
            Tok::Sep => vec![U::Sep],
        })
        .collect();
    let (mut p, mut t) = (0usize, start);
    let mut star: Option<(usize, usize)> = None;
    loop {
        if p == units.len() {
            if !end_anchor || t == text.len() {
                return true;
            }
        } else {
            match units[p] {
                U::Star => {
                    star = Some((p, t));
                    p += 1;
                    continue;
                }
                U::B(c) if t < text.len() && text[t] == c => {
                    p += 1;
                    t += 1;
                    continue;
                }
                U::Sep if t < text.len() && is_sep(text[t]) => {
                    p += 1;
                    t += 1;
                    continue;
                }
                // `^` also matches the end of the address.
                U::Sep if t == text.len() => {
                    p += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, st)) if st < text.len() => {
                star = Some((sp, st + 1));
                p = sp + 1;
                t = st + 1;
            }
            _ => return false,
        }
    }
}

/// Why a request was blocked or allowed.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// No rule matched.
    None,
    Block(String),
    /// An `@@` exception overrode a block.
    Allow(String),
}

#[derive(Default)]
pub struct Filters {
    block: RuleSet,
    allow: RuleSet,
    /// Generic `##selector` rules.
    hide_generic: Vec<String>,
    /// Domain-specific `domain##selector` rules: domain -> selectors.
    hide_specific: HashMap<String, Vec<String>>,
    /// `domain##selector` with a `~domain` exclusion: selector -> excluded domains.
    hide_excluded: HashMap<String, Vec<String>>,
    /// `domain#@#selector` exceptions (empty domain = everywhere).
    hide_exceptions: HashMap<String, HashSet<String>>,
    /// Hosts with `$generichide` / `$elemhide` exceptions.
    generichide: Vec<NetRule>,
    elemhide: Vec<NetRule>,
    /// `$removeparam=name` rules: (optional URL pattern, parameter).
    removeparam: Vec<(Option<NetRule>, String)>,
    pub stats: Stats,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub network: usize,
    pub cosmetic: usize,
    pub skipped: usize,
}

impl Filters {
    pub fn parse(text: &str) -> Filters {
        let mut f = Filters::default();
        f.add_list(text);
        f
    }

    pub fn add_list(&mut self, text: &str) {
        let mut badfilters = HashSet::new();
        // Stack of booleans for `!#if`; a line is active if all are true.
        let mut cond: Vec<bool> = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        for raw in &lines {
            let line = raw.trim();
            if let Some(expr) = line.strip_prefix("!#if ") {
                cond.push(eval_condition(expr.trim()));
                continue;
            }
            if line.starts_with("!#else") {
                if let Some(c) = cond.last_mut() {
                    *c = !*c;
                }
                continue;
            }
            if line.starts_with("!#endif") {
                cond.pop();
                continue;
            }
            if cond.iter().any(|c| !c) {
                continue;
            }
            if let Some(rest) = line
                .strip_suffix("$badfilter")
                .or_else(|| line.strip_suffix(",badfilter"))
            {
                badfilters.insert(rest.to_string());
            }
        }
        cond.clear();
        for raw in lines {
            let line = raw.trim();
            if let Some(expr) = line.strip_prefix("!#if ") {
                cond.push(eval_condition(expr.trim()));
                continue;
            }
            if line.starts_with("!#else") {
                if let Some(c) = cond.last_mut() {
                    *c = !*c;
                }
                continue;
            }
            if line.starts_with("!#endif") {
                cond.pop();
                continue;
            }
            if cond.iter().any(|c| !c)
                || line.is_empty()
                || line.starts_with('!')
                || line.starts_with('[')
                || line.starts_with("# ")
                || line == "#"
                || badfilters.contains(line)
            {
                continue;
            }
            if !self.add_rule(line) {
                self.stats.skipped += 1;
            }
        }
    }

    /// Add one rule. Returns false if it was skipped as unsupported.
    pub fn add_rule(&mut self, line: &str) -> bool {
        // Cosmetic rules.
        for (sep, exception) in [("#@#", true), ("##", false)] {
            if let Some(i) = line.find(sep) {
                let (domains, sel) = (&line[..i], line[i + sep.len()..].trim());
                return self.add_cosmetic(domains, sel, exception);
            }
        }
        if line.contains("#?#") || line.contains("#$#") || line.contains("#%#") {
            return false;
        }
        match parse_network(line) {
            Some(Parsed::Rule { rule, allow }) => {
                self.stats.network += 1;
                if allow {
                    self.allow.add(rule)
                } else {
                    self.block.add(rule)
                }
                true
            }
            Some(Parsed::RemoveParam(pattern, name)) => {
                self.removeparam.push((pattern, name));
                true
            }
            Some(Parsed::GenericHide(r)) => {
                self.generichide.push(r);
                true
            }
            Some(Parsed::ElemHide(r)) => {
                self.elemhide.push(r);
                true
            }
            Some(Parsed::Irrelevant) => true,
            None => false,
        }
    }

    fn add_cosmetic(&mut self, domains: &str, sel: &str, exception: bool) -> bool {
        // Scriptlets, HTML filters and procedural operators aren't supported.
        let unsupported = [
            "+js(",
            ":has(",
            ":has-text(",
            ":upward(",
            ":xpath(",
            ":style(",
            ":remove(",
            ":matches-",
            ":min-text-length(",
            ":watch-attr(",
            ":others(",
            ":-abp-",
            ":contains(",
            ":not(",
        ];
        if sel.is_empty() || sel.starts_with('^') || unsupported.iter().any(|u| sel.contains(u)) {
            return false;
        }
        let doms: Vec<&str> = domains
            .split(',')
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .collect();
        if doms.iter().any(|d| d.ends_with(".*") || d.contains('/')) {
            return false; // entity and regex domains
        }
        self.stats.cosmetic += 1;
        if exception {
            let targets = if doms.is_empty() { vec![""] } else { doms };
            for d in targets {
                self.hide_exceptions
                    .entry(d.to_ascii_lowercase())
                    .or_default()
                    .insert(sel.to_string());
            }
            return true;
        }
        let (inc, exc): (Vec<&str>, Vec<&str>) = doms.iter().partition(|d| !d.starts_with('~'));
        if inc.is_empty() {
            self.hide_generic.push(sel.to_string());
            if !exc.is_empty() {
                self.hide_excluded
                    .entry(sel.to_string())
                    .or_default()
                    .extend(exc.iter().map(|d| d[1..].to_ascii_lowercase()));
            }
        } else {
            for d in inc {
                self.hide_specific
                    .entry(d.to_ascii_lowercase())
                    .or_default()
                    .push(sel.to_string());
            }
        }
        true
    }

    /// Check a request. `@@` exceptions beat blocks, except `$important` blocks.
    pub fn check(&self, req: &Req) -> Verdict {
        if let Some(r) = self.block.find(req, true) {
            return Verdict::Block(r.text.clone());
        }
        match self.block.find(req, false) {
            None => Verdict::None,
            Some(b) => match self.allow.find(req, false) {
                Some(a) => Verdict::Allow(a.text.clone()),
                None => Verdict::Block(b.text.clone()),
            },
        }
    }

    /// A block rule matching this request, ignoring exceptions (used for
    /// your own rules, where a block always wins).
    pub fn blocks(&self, req: &Req) -> Option<String> {
        self.block.find(req, false).map(|r| r.text.clone())
    }

    /// Is there an allow rule for this request (used for user rules)?
    pub fn allows(&self, req: &Req) -> Option<String> {
        self.allow.find(req, false).map(|r| r.text.clone())
    }

    /// Query parameters `$removeparam` rules strip from `url`.
    pub fn removeparam(&self, url: &str, page_url: Option<&str>, dest: Dest) -> HashSet<String> {
        let req = Req::new(url, page_url, dest);
        self.removeparam
            .iter()
            .filter(|(pat, _)| pat.as_ref().is_none_or(|p| p.matches(&req)))
            .map(|(_, name)| name.clone())
            .collect()
    }

    /// CSS that hides the elements cosmetic filters target on `page_url`.
    pub fn cosmetic_css(&self, page_url: &str) -> String {
        let host = host_of(page_url);
        if host.is_empty() {
            return String::new();
        }
        let doc = Req::new(page_url, None, Dest::Document);
        if self.elemhide.iter().any(|r| r.matches(&doc)) {
            return String::new();
        }
        let generic_off = self.generichide.iter().any(|r| r.matches(&doc));

        let mut excepted: HashSet<&str> = HashSet::new();
        for (d, sels) in &self.hide_exceptions {
            if d.is_empty() || domain_matches(&host, d) {
                excepted.extend(sels.iter().map(String::as_str));
            }
        }
        let mut sels: Vec<&str> = Vec::new();
        if !generic_off {
            for s in &self.hide_generic {
                let excluded = self
                    .hide_excluded
                    .get(s)
                    .is_some_and(|ds| ds.iter().any(|d| domain_matches(&host, d)));
                if !excluded && !excepted.contains(s.as_str()) {
                    sels.push(s);
                }
            }
        }
        let mut h = host.as_str();
        loop {
            if let Some(list) = self.hide_specific.get(h) {
                sels.extend(list.iter().map(String::as_str).filter(|s| !excepted.contains(s)));
            }
            match h.find('.') {
                Some(i) => h = &h[i + 1..],
                None => break,
            }
        }
        // Chunk into rules; the CSS parser drops selectors it can't handle
        // one at a time, so one odd selector doesn't disable the rest.
        let mut css = String::new();
        for chunk in sels.chunks(500) {
            css.push_str(&chunk.join(",\n"));
            css.push_str(" { display: none !important }\n");
        }
        css
    }
}

/// `!#if` conditions, evaluated as uBlock does on desktop Firefox.
pub fn eval_condition(expr: &str) -> bool {
    expr.split("||").any(|alt| {
        alt.split("&&").all(|term| {
            let term = term.trim().trim_matches(['(', ')']);
            let (neg, name) = match term.strip_prefix('!') {
                Some(n) => (true, n.trim()),
                None => (false, term),
            };
            let value =
                matches!(name, "env_firefox" | "env_gecko" | "env_desktop" | "false") && name != "false";
            value != neg
        })
    })
}

enum Parsed {
    Rule {
        rule: NetRule,
        allow: bool,
    },
    RemoveParam(Option<NetRule>, String),
    GenericHide(NetRule),
    ElemHide(NetRule),
    /// Valid, but only targets requests Brickle never makes.
    Irrelevant,
}

fn parse_network(line: &str) -> Option<Parsed> {
    let (allow, body) = match line.strip_prefix("@@") {
        Some(b) => (true, b),
        None => (false, line),
    };
    // Regex rules `/.../` aren't supported.
    if body.starts_with('/') && body.len() > 1 && body[..body.find('$').unwrap_or(body.len())].ends_with('/')
    {
        return None;
    }
    let (pattern, opts) = match body.rfind('$') {
        Some(i) if !body[i + 1..].contains('/') || body[i + 1..].contains('=') => {
            (&body[..i], Some(&body[i + 1..]))
        }
        _ => (body, None),
    };

    let mut types = 0u8;
    let mut neg_types = 0u8;
    let mut third_party = None;
    let mut domains = Vec::new();
    let mut important = false;
    let mut removeparam = None;
    let mut generichide = false;
    let mut elemhide = false;
    if let Some(opts) = opts {
        for opt in opts.split(',') {
            let opt = opt.trim();
            let (neg, name) = match opt.strip_prefix('~') {
                Some(n) => (true, n),
                None => (false, opt),
            };
            let (key, value) = match name.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (name, None),
            };
            let type_bit = match key {
                "document" | "doc" => Some(T_DOCUMENT),
                "stylesheet" | "css" => Some(T_STYLESHEET),
                "script" => Some(T_SCRIPT),
                "xmlhttprequest" | "xhr" => Some(T_XHR),
                "image" => Some(T_IMAGE),
                "font" | "media" | "object" | "subdocument" | "frame" | "ping" | "websocket" | "other"
                | "beacon" | "webrtc" | "object-subrequest" => Some(T_OTHER),
                "all" => Some(T_ALL),
                _ => None,
            };
            if let Some(bit) = type_bit {
                if neg {
                    neg_types |= bit
                } else {
                    types |= bit
                }
                continue;
            }
            match key {
                "third-party" | "3p" => third_party = Some(!neg),
                "first-party" | "1p" => third_party = Some(neg),
                "domain" | "from" => {
                    for d in value?.split('|') {
                        let (inc, d) = match d.strip_prefix('~') {
                            Some(d) => (false, d),
                            None => (true, d),
                        };
                        if d.ends_with(".*") || d.starts_with('/') {
                            return None;
                        }
                        domains.push((d.to_ascii_lowercase(), inc));
                    }
                }
                "important" => important = true,
                "match-case" | "popup" | "popunder" | "strict1p" | "strict3p" | "ehide" | "shide"
                | "specifichide" => {
                    if key == "ehide" {
                        elemhide = true;
                    }
                    if matches!(key, "popup" | "popunder") {
                        return Some(Parsed::Irrelevant);
                    }
                }
                "removeparam" | "queryprune" => {
                    let v = value.unwrap_or("");
                    if v.is_empty() || v.starts_with('/') || v.starts_with('~') || v.contains('|') {
                        return None;
                    }
                    removeparam = Some(v.to_string());
                }
                "generichide" | "ghide" => generichide = true,
                "elemhide" => elemhide = true,
                _ => return None, // redirect, csp, header, permissions, replace, ...
            }
        }
    }
    if neg_types != 0 {
        types = T_ALL & !neg_types & !T_DOCUMENT | (types & T_DOCUMENT);
    }
    if types != 0 && types & T_USED == 0 && !generichide && !elemhide {
        return Some(Parsed::Irrelevant);
    }

    let (start, rest) = if let Some(r) = pattern.strip_prefix("||") {
        (Anchor::Host, r)
    } else if let Some(r) = pattern.strip_prefix('|') {
        (Anchor::Start, r)
    } else {
        (Anchor::None, pattern)
    };
    let (rest, end_anchor) = match rest.strip_suffix('|') {
        Some(r) => (r, true),
        None => (rest, false),
    };
    if rest.matches('*').count() > 8
        || (rest.is_empty() && removeparam.is_none() && !generichide && !elemhide)
    {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    let mut tokens = Vec::new();
    let mut lit = Vec::new();
    for &c in lower.as_bytes() {
        match c {
            b'*' | b'^' => {
                if !lit.is_empty() {
                    tokens.push(Tok::Lit(std::mem::take(&mut lit)));
                }
                tokens.push(if c == b'*' { Tok::Star } else { Tok::Sep });
            }
            _ => lit.push(c),
        }
    }
    if !lit.is_empty() {
        tokens.push(Tok::Lit(lit));
    }
    let pure_host = start == Anchor::Host
        && matches!(tokens.as_slice(), [Tok::Lit(_), Tok::Sep] | [Tok::Lit(_)])
        && opts.is_none_or(|o| {
            o.split(',')
                .all(|x| matches!(x, "important" | "third-party" | "3p"))
        });
    let rule = NetRule {
        tokens,
        start,
        end_anchor,
        third_party,
        types,
        domains,
        important,
        pure_host,
        text: line.to_string(),
    };
    if let Some(name) = removeparam {
        if allow {
            return None;
        }
        // removeparam applies to navigations as well as subresources.
        let mut rule = rule;
        if rule.types == 0 {
            rule.types = T_USED;
        }
        let pattern = if rest.is_empty() && start == Anchor::None {
            None
        } else {
            Some(rule)
        };
        return Some(Parsed::RemoveParam(pattern, name.to_ascii_lowercase()));
    }
    if allow && (generichide || elemhide) {
        // These apply to the page itself, including its `domain=` list.
        let mut rule = rule;
        rule.types = T_DOCUMENT;
        return Some(if generichide {
            Parsed::GenericHide(rule)
        } else {
            Parsed::ElemHide(rule)
        });
    }
    if generichide || elemhide {
        return None;
    }
    Some(Parsed::Rule { rule, allow })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css(page: &str) -> Req {
        Req::new("https://cdn.tracker.example/x.css", Some(page), Dest::Stylesheet)
    }

    #[test]
    fn host_anchor_and_separator() {
        let f = Filters::parse("||tracker.example^\n");
        assert!(matches!(f.check(&css("https://news.site/")), Verdict::Block(_)));
        let ok = Req::new(
            "https://nottracker.example/x.css",
            Some("https://news.site/"),
            Dest::Stylesheet,
        );
        assert_eq!(f.check(&ok), Verdict::None);
        let ok = Req::new(
            "https://tracker.example.org/x.css",
            Some("https://news.site/"),
            Dest::Stylesheet,
        );
        assert_eq!(f.check(&ok), Verdict::None);
    }

    #[test]
    fn wildcards_and_anchors() {
        let f = Filters::parse("/ads/*.css\n|https://exact.example/a.css|\n");
        let r = Req::new(
            "https://x.example/static/ads/big.css?v=1",
            Some("https://x.example/"),
            Dest::Stylesheet,
        );
        assert!(matches!(f.check(&r), Verdict::Block(_)));
        let r = Req::new(
            "https://exact.example/a.css",
            Some("https://x.example/"),
            Dest::Stylesheet,
        );
        assert!(matches!(f.check(&r), Verdict::Block(_)));
        let r = Req::new(
            "https://exact.example/a.css?x",
            Some("https://x.example/"),
            Dest::Stylesheet,
        );
        assert_eq!(f.check(&r), Verdict::None);
    }

    #[test]
    fn exceptions_party_and_domains() {
        let f = Filters::parse(
            "||tracker.example^$third-party\n@@||tracker.example/ok.css\n||cdn.example^$domain=a.example|~b.a.example\n",
        );
        // First-party use isn't blocked by a third-party rule.
        let r = Req::new(
            "https://tracker.example/x.css",
            Some("https://www.tracker.example/"),
            Dest::Stylesheet,
        );
        assert_eq!(f.check(&r), Verdict::None);
        let r = Req::new(
            "https://tracker.example/ok.css",
            Some("https://news.site/"),
            Dest::Stylesheet,
        );
        assert!(matches!(f.check(&r), Verdict::Allow(_)));
        let on = |page: &str| {
            f.check(&Req::new(
                "https://cdn.example/s.css",
                Some(page),
                Dest::Stylesheet,
            ))
        };
        assert!(matches!(on("https://a.example/"), Verdict::Block(_)));
        assert!(matches!(on("https://x.a.example/"), Verdict::Block(_)));
        assert_eq!(on("https://b.a.example/"), Verdict::None);
        assert_eq!(on("https://other.example/"), Verdict::None);
    }

    #[test]
    fn important_beats_exception() {
        let f = Filters::parse("||t.example^$important\n@@||t.example^\n");
        assert!(matches!(
            f.check(&Req::new(
                "https://t.example/a.css",
                Some("https://s.example/"),
                Dest::Stylesheet
            )),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn documents_only_blocked_by_strict_rules() {
        let f = Filters::parse("||bad.example^\n/banner/*\n||ads.example^$document\n");
        let doc = |u: &str| f.check(&Req::new(u, None, Dest::Document));
        assert!(matches!(doc("https://bad.example/"), Verdict::Block(_)));
        assert!(matches!(doc("https://ads.example/x"), Verdict::Block(_)));
        assert_eq!(doc("https://ok.example/banner/1"), Verdict::None);
    }

    #[test]
    fn image_rules_apply_to_images_only() {
        let f =
            Filters::parse("||pixel.example^$image\n||cdn.example/ads/\n@@||cdn.example/ads/ok.png$image\n");
        let page = Some("https://site.example/");
        let check = |u: &str, d: Dest| f.check(&Req::new(u, page, d));
        assert!(matches!(
            check("https://pixel.example/t.gif", Dest::Image),
            Verdict::Block(_)
        ));
        assert_eq!(
            check("https://pixel.example/t.css", Dest::Stylesheet),
            Verdict::None
        );
        assert!(matches!(
            check("https://cdn.example/ads/a.png", Dest::Image),
            Verdict::Block(_)
        ));
        assert!(matches!(
            check("https://cdn.example/ads/ok.png", Dest::Image),
            Verdict::Allow(_)
        ));
    }

    #[test]
    fn skips_unsupported_and_irrelevant() {
        let f = Filters::parse(
            "/^https?:\\/\\/[a-z]+\\.example\\//\n||x.example^$redirect=noop.js\n||img.example^$font\nexample.com##+js(nobab)\n##.ok\n",
        );
        assert_eq!(f.stats.network, 0);
        assert_eq!(f.stats.cosmetic, 1);
        assert_eq!(f.stats.skipped, 3);
        let r = Req::new(
            "https://img.example/a.css",
            Some("https://s.example/"),
            Dest::Stylesheet,
        );
        assert_eq!(f.check(&r), Verdict::None);
    }

    #[test]
    fn preprocessor() {
        let f = Filters::parse(
            "!#if env_chromium\n||chrome.example^\n!#else\n||ff.example^\n!#endif\n!#if !env_firefox\n||no.example^\n!#endif\n",
        );
        let blocked = |h: &str| {
            matches!(
                f.check(&Req::new(
                    &format!("https://{h}/a.css"),
                    Some("https://s.example/"),
                    Dest::Stylesheet
                )),
                Verdict::Block(_)
            )
        };
        assert!(!blocked("chrome.example"));
        assert!(blocked("ff.example"));
        assert!(!blocked("no.example"));
    }

    #[test]
    fn badfilter_cancels_rule() {
        let f = Filters::parse("||t.example^\n||t.example^$badfilter\n");
        let r = Req::new(
            "https://t.example/a.css",
            Some("https://s.example/"),
            Dest::Stylesheet,
        );
        assert_eq!(f.check(&r), Verdict::None);
    }

    #[test]
    fn cosmetic_rules() {
        let f = Filters::parse(
            "##.ad-banner\n##.sponsor\nnews.example##.promo\nnews.example#@#.sponsor\n~shop.example##.popup\n",
        );
        let c = f.cosmetic_css("https://www.news.example/story");
        assert!(c.contains(".ad-banner") && c.contains(".promo") && c.contains(".popup"));
        assert!(!c.contains(".sponsor"));
        let c = f.cosmetic_css("https://shop.example/");
        assert!(c.contains(".sponsor") && !c.contains(".popup") && !c.contains(".promo"));

        let f = Filters::parse("##.ad\n@@||quiet.example^$generichide\n");
        assert_eq!(f.cosmetic_css("https://quiet.example/"), "");
    }

    #[test]
    fn removeparam() {
        let f = Filters::parse("$removeparam=ref_id\n||shop.example^$removeparam=aff\n");
        let p = f.removeparam("https://shop.example/?aff=1&ref_id=2", None, Dest::Document);
        assert!(p.contains("aff") && p.contains("ref_id"));
        let p = f.removeparam("https://other.example/?aff=1&ref_id=2", None, Dest::Document);
        assert!(!p.contains("aff") && p.contains("ref_id"));
    }

    #[test]
    fn glob_is_not_exponential() {
        let f = Filters::parse("*a*a*a*a*a*a*a*b\n");
        let url = format!("https://x.example/{}", "a".repeat(5000));
        let r = Req::new(&url, Some("https://y.example/"), Dest::Stylesheet);
        let t = std::time::Instant::now();
        assert_eq!(f.check(&r), Verdict::None);
        assert!(t.elapsed().as_secs() < 5);
    }
}
