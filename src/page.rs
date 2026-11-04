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
use std::collections::HashMap;
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
    /// Boxes from the last layout and the node each is for, in paint
    /// order, for finding what a click hit.
    hits: RefCell<Vec<(Rect, u32)>>,
    /// The page's editable DOM when it has no scripts (with scripts, the
    /// script runtime holds it). Typing and form controls change it.
    arena: Option<Dom>,
    /// The form field that has the keyboard.
    pub focus: Option<u32>,
    /// Images by URL: decoded, or `None` if they couldn't be loaded.
    images: HashMap<String, Option<Decoded>>,
    /// Bytes of decoded pixels kept in `images`.
    image_bytes: usize,
    /// Your text size (see `StyleContext::text_scale`).
    pub text_scale: f32,
    /// Size of the tab bar's text in px: [`Rendered::title`] is drawn at
    /// it (0 = not wanted).
    pub title_px: f32,
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
    /// A field that takes typing has the focus.
    pub focus: bool,
}

/// A key that does something in a form field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Backspace,
    Enter,
    Tab,
    Escape,
}

/// What scripts left behind while a page loaded.
#[derive(Debug, Default)]
pub struct ScriptOutput {
    pub console: Vec<String>,
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
}

/// Fetches a subresource (absolute URL) for the page. In the sandboxed
/// renderer this asks the browser process, which applies the security policy.
pub type Fetcher<'f> = dyn FnMut(&str, Dest) -> Result<String, String> + 'f;

/// The browser's side of a page while its scripts run: everything that
/// needs the network or the cookie jar is asked of it (over IPC in the
/// sandboxed renderer), and it applies the policy.
pub trait Host {
    /// A stylesheet or script file for the page.
    fn fetch(&mut self, url: &str, dest: Dest) -> Result<String, String>;
    /// A script's `fetch`/`XMLHttpRequest`.
    fn request(&mut self, url: &str, req: ScriptRequest) -> Result<ScriptResponse, String>;
    /// An image's bytes.
    fn image(&mut self, url: &str) -> Result<Vec<u8>, String> {
        Err(format!("images are off, skipped {url}"))
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
        page.set_tree(arena.to_tree(), fetch);
        page.arena = Some(arena);
        page
    }

    /// Parse a document and run its scripts (only ever in the sandboxed
    /// renderer). Returns the page and the script runtime, which keeps the
    /// page alive for events.
    pub fn load_with_scripts(
        url: &str,
        src: &str,
        blocker_css: &str,
        viewport: (f32, f32),
        host: &mut dyn Host,
    ) -> (Page, Option<Runtime>, ScriptOutput) {
        let tree = html::parse(src);
        let mut rt = match Runtime::new(&tree, url, viewport) {
            Ok(rt) => rt,
            Err(e) => {
                let mut page = Page::empty(url, blocker_css, false);
                page.set_tree(tree, &mut |u, d| host.fetch(u, d));
                let out = ScriptOutput {
                    console: vec![format!("[brickle] {e}")],
                    navigate: None,
                };
                return (page, None, out);
            }
        };
        rt.set_cookies(host.cookies());
        let until = Instant::now() + js::LOAD_BUDGET;
        let mut loader = ScriptLoader { fetched: 0, until };
        for (id, src, code) in rt.document_scripts() {
            if Instant::now() >= until {
                break;
            }
            loader.run(&mut rt, id, src, code, host);
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
        page.set_tree(rt.tree(), &mut |u, d| host.fetch(u, d));
        let out = ScriptOutput {
            console: rt.take_console(),
            navigate: rt.take_navigate(),
        };
        (page, Some(rt), out)
    }

    /// A page for an already-built tree (no subresources; for tests and
    /// fuzzing).
    pub fn from_tree(url: &str, tree: Node) -> Page {
        let mut page = Page::empty(url, "", true);
        page.set_tree(tree, &mut |_, _| Err("subresources disabled".into()));
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
            hits: RefCell::new(Vec::new()),
            arena: None,
            focus: None,
            images: HashMap::new(),
            image_bytes: 0,
            text_scale: 1.0,
            title_px: 0.0,
        }
    }

    /// Fetch and decode the document's images not tried yet. Decoding
    /// happens here, in the (sandboxed) renderer; `fetch` asks the browser.
    pub fn load_images(&mut self, fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>) {
        let mut imgs = Vec::new();
        self.dom.walk(&mut imgs, &|n| n.tag() == Some("img"));
        let mut urls: Vec<String> = imgs
            .iter()
            .filter_map(|n| n.element().and_then(|e| image_source(&self.url, e)))
            .collect();
        // CSS background images, after the page's own images.
        urls.extend(self.background_urls());
        for url in urls {
            if self.images.contains_key(&url) || self.images.len() >= crate::network::MAX_IMAGES {
                continue;
            }
            let bytes = match data_url(&url) {
                Some(b) => b,
                None if url.starts_with("data:") => Err("unsupported data: address".into()),
                None => fetch(&url),
            };
            let decoded = bytes.ok().and_then(|b| {
                // A decoder bug must not take the page down with it.
                std::panic::catch_unwind(|| image::decode(&b)).ok()?.ok()
            });
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
    fn set_tree(&mut self, dom: Node, fetch: &mut Fetcher) {
        let mut sheets = Vec::new();
        let mut found = Vec::new();
        let scripting = self.scripting;
        collect_style_elements(&dom, scripting, &mut found);
        for n in found {
            let Some(e) = n.element() else { continue };
            if e.tag == "style" {
                sheets.push(css::parse(&n.text_content()));
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
                    if let Some(s) = self.sheet_cache.get(&abs) {
                        sheets.push(s.clone());
                    } else if let Ok(text) = fetch(&abs, Dest::Stylesheet) {
                        // Failures are shown in the request log.
                        let mut s = css::parse(&text);
                        css::resolve_urls(&mut s, &abs);
                        self.sheet_cache.insert(abs, s.clone());
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
        self.sheets = sheets;
        self.dom = dom;
    }

    /// After scripts changed the DOM (in response to an event): take the new
    /// tree. Stylesheets not fetched while loading aren't fetched now.
    pub fn refresh(&mut self, rt: &Runtime) {
        self.set_tree(rt.tree(), &mut |_, _| Err("page already loaded".into()));
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
        self.set_tree(tree, &mut |_, _| Err("page already loaded".into()));
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
        let Some(uid) = self.hit(x, y) else {
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
                Some(Activation::Submit { form, submitter }) => {
                    out.submit = self.submit(rt.as_deref_mut(), form, Some(submitter));
                }
                _ => {}
            }
        }
        self.finish(rt, before, was_focus, &mut out);
        out
    }

    /// You typed text into the focused field.
    pub fn type_text(&mut self, mut rt: Option<&mut Runtime>, text: &str) -> Interaction {
        let mut out = Interaction::default();
        let Some(f) = self.focus else { return out };
        let before = self.dom_version(rt.as_deref_mut());
        let typed = self.with_dom(rt.as_deref_mut(), |d| forms::type_text(d, f, text)) == Some(true);
        if typed {
            if let Some(rt) = rt.as_deref_mut() {
                rt.fire(f, "input", true, false);
            }
        }
        self.finish(rt, before, Some(f), &mut out);
        out
    }

    /// You pressed a key with a field focused.
    pub fn key(&mut self, mut rt: Option<&mut Runtime>, key: Key) -> Interaction {
        let mut out = Interaction::default();
        let before = self.dom_version(rt.as_deref_mut());
        let was = self.focus;
        match (key, self.focus) {
            (Key::Backspace, Some(f)) => {
                if self.with_dom(rt.as_deref_mut(), |d| forms::backspace(d, f)) == Some(true) {
                    if let Some(rt) = rt.as_deref_mut() {
                        rt.fire(f, "input", true, false);
                    }
                }
            }
            (Key::Enter, Some(f)) => {
                let textarea = self
                    .with_dom(rt.as_deref_mut(), |d| {
                        d.element(f).is_some_and(|e| e.tag == "textarea")
                    })
                    .unwrap_or(false);
                if textarea {
                    self.with_dom(rt.as_deref_mut(), |d| forms::type_text(d, f, "\n"));
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
            (Key::Tab, _) => {
                self.focus = self
                    .with_dom(rt.as_deref_mut(), |d| forms::next_field(d, was))
                    .flatten();
            }
            (Key::Escape, _) => self.focus = None,
            _ => {}
        }
        self.finish(rt, before, was, &mut out);
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
        self.hits.borrow().iter().map(|(r, u)| (*u, *r)).collect()
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
        forms::decorate(&mut tree, self.focus);
        let images = self.images_by_node(&tree);
        let styled = style::style_tree(&tree, &ctx);
        let root = layout::layout_tree_with_images(&styled, (width, viewport_h), &images);

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
            &Seen::new(&order, page, clips),
            background,
            &mut texts,
            &mut 0,
        );
        // Tall enough to scroll to everything drawn, including positioned
        // boxes and text overflowing its box.
        let mb = root.dims.margin_box();
        let height = order.iter().fold(mb.y + mb.h, |h, b| h.max(content_bottom(b)));
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
            let mut x = 0.0;
            let mut offsets = Vec::with_capacity(n + 1);
            offsets.push(0.0);
            let mut buf = [0u8; 4];
            for c in f.text.chars() {
                x += font::measure(fi, f.style.font_size, c.encode_utf8(&mut buf));
                offsets.push(x);
            }
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

/// The bytes of a `data:image/...;base64,` address (decoded here, no request).
fn data_url(url: &str) -> Option<Result<Vec<u8>, String>> {
    let rest = url
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("data:"))
        .map(|_| &url[5..])?;
    let (meta, payload) = rest.split_once(',')?;
    let meta = meta.to_ascii_lowercase();
    if !meta.starts_with("image/") || meta.contains("svg") {
        return Some(Err("not an image".into()));
    }
    if !meta.ends_with(";base64") {
        return Some(Err("only base64 image data is supported".into()));
    }
    Some(base64(payload).ok_or_else(|| "bad base64".to_string()))
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

/// Runs scripts while a page loads, fetching external ones from the browser.
struct ScriptLoader {
    fetched: usize,
    until: Instant,
}

impl ScriptLoader {
    fn run(&mut self, rt: &mut Runtime, id: u32, src: Option<String>, code: String, host: &mut dyn Host) {
        let (code, name) = match src {
            Some(url) if self.fetched < js::MAX_SCRIPTS => {
                self.fetched += 1;
                match host.fetch(&url, Dest::Script) {
                    Ok(code) => (code, url),
                    Err(_) => return, // shown in the request log
                }
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
                self.run(rt, id, src, code, host);
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
    if b.uid != 0 {
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
            Page::load_with_scripts("https://example.com/", html, "", (800.0, 600.0), &mut host);
        let _ = page.render(800.0, 600.0);
        (page, rt.expect("scripts run"))
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
        page.load_images(&mut |u| {
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
            "<img id=a src='data:image/png;base64,{b64}'><img src='data:image/svg+xml,<svg/>'>\
             <img src='data:text/html;base64,PGI+'>"
        ));
        assert!(asked.is_empty(), "{asked:?}");
        assert_eq!(size_of(&page, "a"), (3.0, 2.0));
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
