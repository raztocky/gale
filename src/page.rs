//! A loaded page: DOM + stylesheets, and the render pipeline.

use crate::css::{self, Color, Stylesheet};
use crate::dom::Node;
use crate::layout::{self, BoxKind, LayoutBox, Rect};
use crate::paint::{self, DisplayList};
use crate::style::{self, StyleContext};
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
}

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
        }
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
                        let s = css::parse(&text);
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
            sheets: all,
            viewport: (width, viewport_h),
        };
        // Form controls get their value, label or placeholder to draw.
        let mut tree = self.dom.clone();
        forms::decorate(&mut tree, self.focus);
        let styled = style::style_tree(&tree, &ctx);
        let root = layout::layout_tree(&styled, width);

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

        let (list, atlas) = paint::rasterize(paint::build_display_list(&root));
        let mut links = Vec::new();
        collect_links(&root, &mut links);
        let mut hits = Vec::new();
        collect_hits(&root, &mut hits);
        *self.hits.borrow_mut() = hits;
        let mb = root.dims.margin_box();
        Rendered {
            list,
            atlas,
            height: mb.y + mb.h,
            background,
            links,
        }
    }
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

/// Every box with a DOM node, in paint order (later boxes are on top).
fn collect_hits(b: &LayoutBox, out: &mut Vec<(Rect, u32)>) {
    if b.uid != 0 {
        out.push((b.dims.border_box(), b.uid));
    }
    if let BoxKind::Anonymous { frags, .. } = &b.kind {
        for f in frags {
            let (a, d) = font::vmetrics(font::index(&f.style), f.style.font_size);
            out.push((
                Rect {
                    x: f.x,
                    y: f.baseline - a,
                    w: f.w,
                    h: a + d,
                },
                f.uid,
            ));
        }
    }
    for c in &b.children {
        collect_hits(c, out);
    }
}

fn collect_links(b: &LayoutBox, out: &mut Vec<(Rect, String)>) {
    if let (Some(l), BoxKind::Block(_)) = (&b.link, &b.kind) {
        out.push((b.dims.border_box(), l.clone()));
    }
    if let BoxKind::Anonymous { frags, .. } = &b.kind {
        for f in frags {
            if let Some(l) = &f.link {
                let (a, d) = font::vmetrics(font::index(&f.style), f.style.font_size);
                out.push((
                    Rect {
                        x: f.x,
                        y: f.baseline - a,
                        w: f.w,
                        h: a + d,
                    },
                    l.clone(),
                ));
            }
        }
    }
    for c in &b.children {
        collect_links(c, out);
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
