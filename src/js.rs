//! JavaScript, run by Boa (a pure-Rust interpreter, no JIT) inside the
//! sandboxed renderer.
//!
//! What scripts can reach is deliberately small:
//! - the page's own DOM, through the primitives below, wrapped into the
//!   familiar `document`/`Element` API by `prelude.js`;
//! - timers and events, run on a virtual clock in bounded batches;
//! - nothing else: no network access (`fetch` and `XMLHttpRequest` fail),
//!   no cookies, no storage that outlives the page. The renderer itself has
//!   no network or files either; asking the browser is the only way out,
//!   and the browser decides.
//!
//! Scripts also can't run forever: loops and recursion are capped by Boa,
//! batches of work have a time budget, and the browser kills a renderer
//! that misses its deadline (then shows the page without JavaScript).
//!
//! The DOM lives in an arena (`Dom`) while scripts run; the renderer turns
//! it back into a `dom::Node` tree for styling and layout. Node ids are
//! arena indices and become `Node::uid`, so a click can be traced back to
//! its element.

use crate::css;
use crate::dom::{self, AttrMap, ElementData, Node, NodeType};
use crate::layout::Rect;
use crate::net::{Credentials, ScriptRequest};
use crate::network::ScriptResponse;
use boa_engine::context::HostHooks;
use boa_engine::object::builtins::JsArray;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue,
    NativeFunction, Source,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Most nodes a page (parser + scripts) may have.
pub const MAX_NODES: usize = 500_000;
/// Longest string a script may put into the DOM in one go.
const MAX_STRING: usize = 16 * 1024 * 1024;
/// Console lines kept, and their length.
const MAX_CONSOLE: usize = 200;
const MAX_CONSOLE_LINE: usize = 500;
/// Iterations any single loop may run before Boa throws.
const LOOP_LIMIT: u64 = 20_000_000;
/// Deepest JS call stack.
const RECURSION_LIMIT: usize = 400;
/// Wall-clock budget for running scripts while a page loads, and for
/// handling one click. Checked between scripts and timer callbacks; the
/// browser's deadline is the hard stop.
pub const LOAD_BUDGET: Duration = Duration::from_secs(6);
pub const EVENT_BUDGET: Duration = Duration::from_secs(2);
/// Virtual time timers may advance per batch, and callbacks per batch.
const TIMER_WINDOW_MS: f64 = 2000.0;
const MAX_CALLBACKS: usize = 2000;
/// Scripts a page may load from the network (asked of the browser).
pub const MAX_SCRIPTS: usize = 32;
/// Script requests that may wait for an answer at once.
const MAX_QUEUED_REQUESTS: usize = 64;

const PRELUDE: &str = include_str!("prelude.js");

// ---------------------------------------------------------------------------
// The DOM arena
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ANode {
    parent: Option<u32>,
    children: Vec<u32>,
    data: NodeType,
}

/// Elements are real elements (not the document or a fragment, whose tags
/// start with `#`).
impl crate::selectors::Tree for Dom {
    fn element(&self, id: u32) -> Option<&ElementData> {
        Dom::element(self, id).filter(|e| !e.tag.starts_with('#'))
    }
    fn parent_node(&self, id: u32) -> Option<u32> {
        self.nodes.get(id as usize)?.parent
    }
    fn children(&self, id: u32) -> &[u32] {
        self.nodes.get(id as usize).map_or(&[], |n| &n.children)
    }
    fn has_text(&self, id: u32) -> bool {
        self.nodes
            .get(id as usize)
            .is_some_and(|n| matches!(&n.data, NodeType::Text(t) if !t.is_empty()))
    }
}

/// A mutable DOM. Node 0 is the document.
#[derive(Debug, Default)]
pub struct Dom {
    nodes: Vec<ANode>,
    /// Bumped on every change, so the renderer knows when to re-layout.
    pub version: u64,
}

const DOCUMENT: u32 = 0;
const FRAGMENT_TAG: &str = "#fragment";

impl Dom {
    pub fn from_tree(root: &Node) -> Dom {
        let mut d = Dom {
            nodes: vec![ANode {
                parent: None,
                children: vec![],
                data: NodeType::Element(ElementData {
                    tag: "#document".into(),
                    attrs: AttrMap::new(),
                }),
            }],
            version: 0,
        };
        let id = d.import(root);
        d.append(DOCUMENT, id);
        d
    }

    /// Copy a parsed tree into the arena (detached). Returns its id.
    fn import(&mut self, n: &Node) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(ANode {
            parent: None,
            children: vec![],
            data: n.node_type.clone(),
        });
        for c in &n.children {
            if self.nodes.len() >= MAX_NODES {
                break;
            }
            let cid = self.import(c);
            self.append(id, cid);
        }
        id
    }

    fn append(&mut self, parent: u32, child: u32) {
        self.nodes[child as usize].parent = Some(parent);
        self.nodes[parent as usize].children.push(child);
    }

    fn get(&self, id: u32) -> Option<&ANode> {
        self.nodes.get(id as usize)
    }

    /// The parent of a node (None for the document and detached nodes).
    pub fn parent(&self, id: u32) -> Option<u32> {
        self.get(id)?.parent
    }

    /// Set an attribute (lower-case name) on an element.
    pub fn set_attr(&mut self, id: u32, name: &str, value: &str) {
        if let Some(e) = self.element_mut(id) {
            e.attrs.insert(name.to_string(), value.to_string());
            self.version += 1;
        }
    }

    pub fn remove_attr(&mut self, id: u32, name: &str) {
        if let Some(e) = self.element_mut(id) {
            if e.attrs.remove(name).is_some() {
                self.version += 1;
            }
        }
    }

    pub fn element(&self, id: u32) -> Option<&ElementData> {
        match &self.get(id)?.data {
            NodeType::Element(e) => Some(e),
            _ => None,
        }
    }

    fn element_mut(&mut self, id: u32) -> Option<&mut ElementData> {
        match &mut self.nodes.get_mut(id as usize)?.data {
            NodeType::Element(e) => Some(e),
            _ => None,
        }
    }

    fn new_node(&mut self, data: NodeType) -> Result<u32, &'static str> {
        if self.nodes.len() >= MAX_NODES {
            return Err("too many DOM nodes");
        }
        self.nodes.push(ANode {
            parent: None,
            children: vec![],
            data,
        });
        Ok(self.nodes.len() as u32 - 1)
    }

    fn is_ancestor_or_self(&self, anc: u32, mut n: u32) -> bool {
        loop {
            if n == anc {
                return true;
            }
            match self.get(n).and_then(|x| x.parent) {
                Some(p) => n = p,
                None => return false,
            }
        }
    }

    pub fn connected(&self, id: u32) -> bool {
        self.is_ancestor_or_self(DOCUMENT, id)
    }

    fn detach(&mut self, id: u32) {
        if let Some(p) = self.nodes[id as usize].parent.take() {
            self.nodes[p as usize].children.retain(|&c| c != id);
            self.version += 1;
        }
    }

    /// Insert `child` into `parent` before `before` (or at the end).
    /// Fragments insert their children.
    fn insert(&mut self, parent: u32, child: u32, before: Option<u32>) -> Result<(), &'static str> {
        if self.get(parent).is_none() || self.get(child).is_none() {
            return Err("no such node");
        }
        if matches!(self.nodes[parent as usize].data, NodeType::Text(_)) {
            return Err("text nodes can't have children");
        }
        if child == DOCUMENT || self.is_ancestor_or_self(child, parent) {
            return Err("can't insert a node into itself");
        }
        if let Some(b) = before {
            if self.get(b).and_then(|n| n.parent) != Some(parent) {
                return Err("reference node isn't a child of this node");
            }
        }
        let moving: Vec<u32> = if self.element(child).is_some_and(|e| e.tag == FRAGMENT_TAG) {
            self.nodes[child as usize].children.clone()
        } else {
            vec![child]
        };
        for &m in &moving {
            if Some(m) == before {
                continue;
            }
            self.detach(m);
            let pos = match before {
                Some(b) => self.nodes[parent as usize]
                    .children
                    .iter()
                    .position(|&c| c == b)
                    .unwrap_or(self.nodes[parent as usize].children.len()),
                None => self.nodes[parent as usize].children.len(),
            };
            self.nodes[m as usize].parent = Some(parent);
            self.nodes[parent as usize].children.insert(pos, m);
        }
        self.version += 1;
        Ok(())
    }

    /// A node's text: a comment's own text, or the text nodes inside.
    pub fn text_content(&self, id: u32, out: &mut String) {
        match &self.nodes[id as usize].data {
            NodeType::Comment(c) => out.push_str(c),
            _ => self.inner_text(id, out),
        }
    }

    fn inner_text(&self, id: u32, out: &mut String) {
        match &self.nodes[id as usize].data {
            NodeType::Text(t) => out.push_str(t),
            NodeType::Comment(_) => {}
            NodeType::Element(_) => {
                for &c in &self.nodes[id as usize].children {
                    self.inner_text(c, out);
                }
            }
        }
    }

    pub fn set_text_content(&mut self, id: u32, s: String) -> Result<(), &'static str> {
        if let NodeType::Text(t) | NodeType::Comment(t) = &mut self.nodes[id as usize].data {
            *t = s;
            self.version += 1;
            return Ok(());
        }
        for c in self.nodes[id as usize].children.clone() {
            self.detach(c);
        }
        if !s.is_empty() {
            let t = self.new_node(NodeType::Text(s))?;
            self.insert(id, t, None)?;
        }
        self.version += 1;
        Ok(())
    }

    fn clone_node(&mut self, id: u32, deep: bool) -> Result<u32, &'static str> {
        let data = self.nodes[id as usize].data.clone();
        let new = self.new_node(data)?;
        if deep {
            for c in self.nodes[id as usize].children.clone() {
                let cc = self.clone_node(c, true)?;
                self.append(new, cc);
            }
        }
        Ok(new)
    }

    /// Parse HTML as the children of `parent` (replacing them).
    fn set_inner_html(&mut self, parent: u32, html: &str) -> Result<(), &'static str> {
        if self.element(parent).is_none() {
            return Err("not an element");
        }
        for c in self.nodes[parent as usize].children.clone() {
            self.detach(c);
        }
        for n in parse_fragment(html) {
            if self.nodes.len() >= MAX_NODES {
                return Err("too many DOM nodes");
            }
            let id = self.import(&n);
            self.append(parent, id);
        }
        self.version += 1;
        Ok(())
    }

    fn serialize(&self, id: u32, out: &mut String) {
        match &self.nodes[id as usize].data {
            NodeType::Text(t) => {
                let raw = self.nodes[id as usize]
                    .parent
                    .and_then(|p| self.element(p))
                    .is_some_and(|e| matches!(e.tag.as_str(), "script" | "style"));
                if raw {
                    out.push_str(t);
                } else {
                    out.push_str(&t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"));
                }
            }
            NodeType::Comment(c) => {
                out.push_str("<!--");
                out.push_str(&c.replace("-->", "--&gt;"));
                out.push_str("-->");
            }
            NodeType::Element(e) if e.tag.starts_with('#') => self.serialize_children(id, out),
            NodeType::Element(e) => {
                out.push('<');
                out.push_str(&e.tag);
                let mut attrs: Vec<_> = e.attrs.iter().collect();
                attrs.sort();
                for (k, v) in attrs {
                    out.push_str(&format!(
                        " {k}=\"{}\"",
                        v.replace('&', "&amp;").replace('"', "&quot;")
                    ));
                }
                out.push('>');
                const VOID: &[&str] = &[
                    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source",
                    "track", "wbr",
                ];
                if !VOID.contains(&e.tag.as_str()) {
                    self.serialize_children(id, out);
                    out.push_str(&format!("</{}>", e.tag));
                }
            }
        }
    }

    fn serialize_children(&self, id: u32, out: &mut String) {
        for &c in &self.nodes[id as usize].children {
            self.serialize(c, out);
        }
    }

    fn matches(&self, id: u32, sels: &[css::Selector]) -> bool {
        sels.iter().any(|s| crate::selectors::matches(self, id, s))
    }

    /// Descendants of `root` in tree order.
    pub fn descendants(&self, root: u32, out: &mut Vec<u32>) {
        let mut stack: Vec<u32> = self.nodes[root as usize].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n as usize].children.iter().rev());
        }
    }

    /// The document as a `Node` tree for styling and layout; each node's
    /// `uid` is its id here.
    pub fn to_tree(&self) -> Node {
        let root = self.nodes[0]
            .children
            .iter()
            .copied()
            .find(|&c| self.element(c).is_some())
            .map(|c| self.export(c, 0))
            .unwrap_or_else(|| dom::elem("html".into(), AttrMap::new(), vec![]));
        root
    }

    fn export(&self, id: u32, depth: usize) -> Node {
        let n = &self.nodes[id as usize];
        let children = if depth < crate::html::MAX_DEPTH {
            n.children.iter().map(|&c| self.export(c, depth + 1)).collect()
        } else {
            vec![]
        };
        Node {
            children,
            node_type: n.data.clone(),
            uid: id,
        }
    }

    /// The nearest element at or above `id`.
    pub fn element_at_or_above(&self, mut id: u32) -> Option<u32> {
        loop {
            match &self.get(id)?.data {
                NodeType::Element(e) if !e.tag.starts_with('#') => return Some(id),
                _ => id = self.get(id)?.parent?,
            }
        }
    }
}

/// Parse an HTML fragment (for `innerHTML`): the parsed head and body
/// children, in order.
fn parse_fragment(html: &str) -> Vec<Node> {
    let doc = crate::html::parse(&format!("<body>{html}"));
    let mut out = Vec::new();
    for part in doc.children {
        if matches!(part.tag(), Some("head") | Some("body")) {
            out.extend(part.children);
        } else {
            out.push(part);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Per-thread state reached by the native functions
// ---------------------------------------------------------------------------

#[derive(Default)]
struct State {
    dom: Dom,
    url: String,
    console: Vec<String>,
    /// Details of the browser scripts read ([`crate::explain::FINGERPRINT`]).
    read: std::collections::BTreeSet<&'static str>,
    /// A navigation a script asked for (`location.href = ...`).
    navigate: Option<String>,
    /// Script elements inserted by scripts, waiting to be loaded.
    pending_scripts: Vec<u32>,
    /// Boxes from the last layout, for `getBoundingClientRect`.
    boxes: Vec<(u32, Rect)>,
    /// Boxes you can scroll inside: offset x, y, how far each can scroll,
    /// and padding-box width and height (`Page::scroll_info`).
    scrollers: Vec<(u32, [f32; 6])>,
    /// `scrollTop`/`scrollLeft`/`scrollTo()` asked for: (node, x, y), a
    /// coordinate left as it was when `None`.
    scroll_sets: Vec<(u32, Option<f32>, Option<f32>)>,
    /// Requests scripts made, waiting to be sent to the browser.
    requests: Vec<(u32, String, ScriptRequest)>,
    next_request: u32,
    /// `document.cookie` values scripts set, waiting for the browser.
    cookie_sets: Vec<String>,
    /// `document.cookie` as last reported by the browser (plus local sets).
    cookies: String,
    /// `form.submit()`: (form, submitter).
    submit: Option<(u32, Option<u32>)>,
    warned_selectors: bool,
    /// The page's Content Security Policy.
    csp: crate::csp::Csp,
    /// An inline handler the policy allowed, about to be compiled (once).
    approved_handler: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    /// When the running scripts' time is up (see [`Runtime::set_deadline`]).
    static DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    /// Whether the console says so yet.
    static STOP_NOTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Most promise callbacks run in one go (a promise chain that never ends
/// stops here even with no deadline).
const MAX_JOBS: usize = 1_000_000;

/// Promise callbacks (microtasks), run in order with the deadline checked
/// between them: a framework starting up through a long promise chain
/// stops when its time is up instead of running past the browser's hard
/// limit. Brickle runs timers itself, so Boa's timer jobs are never used.
#[derive(Default)]
struct Jobs {
    promise: RefCell<std::collections::VecDeque<boa_engine::job::PromiseJob>>,
    generic: RefCell<std::collections::VecDeque<boa_engine::job::GenericJob>>,
}

impl boa_engine::job::JobExecutor for Jobs {
    fn enqueue_job(self: Rc<Self>, job: boa_engine::job::Job, _: &mut Context) {
        match job {
            boa_engine::job::Job::PromiseJob(p) => self.promise.borrow_mut().push_back(p),
            boa_engine::job::Job::GenericJob(g) => self.generic.borrow_mut().push_back(g),
            _ => {}
        }
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        for _ in 0..MAX_JOBS {
            if check_deadline().is_err() {
                break;
            }
            let next = self.promise.borrow_mut().pop_front();
            if let Some(job) = next {
                let _ = job.call(context);
                continue;
            }
            let next = self.generic.borrow_mut().pop_front();
            match next {
                Some(job) => {
                    let _ = job.call(context);
                }
                None => return Ok(()),
            }
        }
        // Out of time (or callbacks): the rest never run.
        self.promise.borrow_mut().clear();
        self.generic.borrow_mut().clear();
        Ok(())
    }
}

/// Every native call checks the deadline: a long task (a framework
/// starting up) is stopped once its time is up, with an error the page
/// can't catch, instead of running past the browser's hard limit and
/// losing the whole renderer.
fn check_deadline() -> JsResult<()> {
    match DEADLINE.with(std::cell::Cell::get) {
        Some(t) if Instant::now() >= t => {
            if !STOP_NOTED.with(|n| n.replace(true)) {
                with(|s| push_console(s, "[brickle] a script ran out of time and was stopped".into()));
            }
            Err(boa_engine::error::RuntimeLimitError::LoopIteration.into())
        }
        _ => Ok(()),
    }
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn type_error(msg: &str) -> JsError {
    JsNativeError::typ().with_message(msg.to_string()).into()
}

fn dom_error(msg: &str) -> JsError {
    JsNativeError::error()
        .with_message(format!("DOMException: {msg}"))
        .into()
}

fn arg_id(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<u32> {
    let v = args.get_or_undefined(i).to_number(ctx)?;
    let ok = v.is_finite() && v >= 0.0 && v.fract() == 0.0 && with(|s| (v as usize) < s.dom.nodes.len());
    if ok {
        Ok(v as u32)
    } else {
        Err(type_error("not a node"))
    }
}

fn arg_str(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    let s = args.get_or_undefined(i).to_string(ctx)?.to_std_string_escaped();
    if s.len() > MAX_STRING {
        return Err(JsNativeError::range().with_message("string too long").into());
    }
    Ok(s)
}

fn ids(list: impl IntoIterator<Item = u32>, ctx: &mut Context) -> JsValue {
    JsArray::from_iter(list.into_iter().map(JsValue::from), ctx).into()
}

fn opt_str(s: Option<String>) -> JsValue {
    match s {
        Some(s) => js_string!(s).into(),
        None => JsValue::null(),
    }
}

fn parse_selectors(text: &str) -> Option<Vec<css::Selector>> {
    // All or nothing: a script asking for a selector we can't match gets
    // told, rather than a partial answer.
    crate::selectors::reset_budget();
    css::split_selector_list(text)
        .into_iter()
        .map(css::parse_selector)
        .collect()
}

/// Selectors Brickle doesn't support yet return no matches, with a console
/// note (once), rather than throwing and stopping the script.
fn selectors_or_warn(text: &str) -> Option<Vec<css::Selector>> {
    let s = parse_selectors(text);
    if s.is_none() {
        with(|st| {
            if !st.warned_selectors {
                st.warned_selectors = true;
                push_console(st, format!("[brickle] unsupported selector, no matches: {text}"));
            }
        });
    }
    s
}

fn push_console(st: &mut State, line: String) {
    if st.console.len() < MAX_CONSOLE {
        let mut l = line;
        if l.len() > MAX_CONSOLE_LINE {
            let mut cut = MAX_CONSOLE_LINE;
            while !l.is_char_boundary(cut) {
                cut -= 1;
            }
            l.truncate(cut);
            l.push('…');
        }
        st.console.push(l);
    }
}

// ---------------------------------------------------------------------------
// Native functions (the `dom` object handed to prelude.js)
// ---------------------------------------------------------------------------

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn n_node_type(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    Ok(with(|s| match &s.dom.nodes[id as usize].data {
        NodeType::Text(_) => 3,
        NodeType::Comment(_) => 8,
        NodeType::Element(e) if e.tag == "#document" => 9,
        NodeType::Element(e) if e.tag == FRAGMENT_TAG => 11,
        NodeType::Element(_) => 1,
    })
    .into())
}

fn n_tag(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    Ok(js_string!(with(|s| s
        .dom
        .element(id)
        .map(|e| e.tag.clone())
        .unwrap_or_default()))
    .into())
}

fn n_parent(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    Ok(with(|s| s.dom.nodes[id as usize].parent.map(|p| p as i32).unwrap_or(-1)).into())
}

fn n_children(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let list = with(|s| s.dom.nodes[id as usize].children.clone());
    Ok(ids(list, c))
}

fn n_text_content(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let mut out = String::new();
    with(|s| s.dom.text_content(id, &mut out));
    Ok(js_string!(out).into())
}

fn n_set_text_content(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let text = arg_str(a, 1, c)?;
    with(|s| s.dom.set_text_content(id, text)).map_err(dom_error)?;
    Ok(JsValue::undefined())
}

fn n_get_attr(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let name = arg_str(a, 1, c)?.to_ascii_lowercase();
    Ok(opt_str(with(|s| {
        s.dom.element(id).and_then(|e| e.attrs.get(&name).cloned())
    })))
}

fn n_set_attr(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let name = arg_str(a, 1, c)?.to_ascii_lowercase();
    let value = arg_str(a, 2, c)?;
    if name.is_empty() || name.chars().any(|ch| ch.is_whitespace() || "\"'>/=".contains(ch)) {
        return Err(dom_error("invalid attribute name"));
    }
    with(|s| {
        if let Some(e) = s.dom.element_mut(id) {
            e.attrs.insert(name, value);
            s.dom.version += 1;
        }
    });
    Ok(JsValue::undefined())
}

fn n_remove_attr(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let name = arg_str(a, 1, c)?.to_ascii_lowercase();
    with(|s| {
        if let Some(e) = s.dom.element_mut(id) {
            if e.attrs.remove(&name).is_some() {
                s.dom.version += 1;
            }
        }
    });
    Ok(JsValue::undefined())
}

fn n_attr_names(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let mut names = with(|s| {
        s.dom
            .element(id)
            .map(|e| e.attrs.keys().cloned().collect::<Vec<_>>())
    })
    .unwrap_or_default();
    names.sort();
    Ok(JsArray::from_iter(names.into_iter().map(|n| JsValue::from(js_string!(n))), c).into())
}

fn n_create_element(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let tag = arg_str(a, 0, c)?.to_ascii_lowercase();
    let valid = tag == FRAGMENT_TAG
        || (!tag.is_empty()
            && tag.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic())
            && tag
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == ':'));
    if !valid {
        return Err(dom_error("invalid element name"));
    }
    let id = with(|s| {
        s.dom.new_node(NodeType::Element(ElementData {
            tag,
            attrs: AttrMap::new(),
        }))
    })
    .map_err(dom_error)?;
    Ok(id.into())
}

fn n_create_text(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let text = arg_str(a, 0, c)?;
    let id = with(|s| s.dom.new_node(NodeType::Text(text))).map_err(dom_error)?;
    Ok(id.into())
}

fn n_create_comment(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let text = arg_str(a, 0, c)?;
    let id = with(|s| s.dom.new_node(NodeType::Comment(text))).map_err(dom_error)?;
    Ok(id.into())
}

fn n_insert(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let parent = arg_id(a, 0, c)?;
    let child = arg_id(a, 1, c)?;
    let before = if a.get_or_undefined(2).is_null_or_undefined() {
        None
    } else {
        Some(arg_id(a, 2, c)?)
    };
    with(|s| {
        s.dom.insert(parent, child, before)?;
        // Script elements added to the document get loaded, as in browsers.
        if s.dom.connected(child) {
            let mut new = vec![child];
            s.dom.descendants(child, &mut new);
            for n in new {
                if s.dom.element(n).is_some_and(|e| e.tag == "script") {
                    s.pending_scripts.push(n);
                }
            }
        }
        Ok::<_, &'static str>(())
    })
    .map_err(dom_error)?;
    Ok(JsValue::undefined())
}

fn n_remove(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    with(|s| s.dom.detach(id));
    Ok(JsValue::undefined())
}

fn n_clone(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let deep = a.get_or_undefined(1).to_boolean();
    Ok(with(|s| s.dom.clone_node(id, deep)).map_err(dom_error)?.into())
}

fn n_inner_html(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let mut out = String::new();
    with(|s| s.dom.serialize_children(id, &mut out));
    Ok(js_string!(out).into())
}

fn n_outer_html(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let mut out = String::new();
    with(|s| s.dom.serialize(id, &mut out));
    Ok(js_string!(out).into())
}

fn n_set_inner_html(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let html = arg_str(a, 1, c)?;
    with(|s| s.dom.set_inner_html(id, &html)).map_err(dom_error)?;
    Ok(JsValue::undefined())
}

fn n_query(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let text = arg_str(a, 1, c)?;
    let all = a.get_or_undefined(2).to_boolean();
    let Some(sels) = selectors_or_warn(&text) else {
        return Ok(ids([], c));
    };
    let found = with(|s| {
        let mut d = Vec::new();
        s.dom.descendants(id, &mut d);
        let mut hits = Vec::new();
        for n in d {
            if s.dom.matches(n, &sels) {
                hits.push(n);
                if !all {
                    break;
                }
            }
        }
        hits
    });
    Ok(ids(found, c))
}

fn n_matches(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let text = arg_str(a, 1, c)?;
    let Some(sels) = selectors_or_warn(&text) else {
        return Ok(false.into());
    };
    Ok(with(|s| s.dom.matches(id, &sels)).into())
}

fn n_by_id(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let want = arg_str(a, 0, c)?;
    let found = with(|s| {
        let mut d = Vec::new();
        s.dom.descendants(DOCUMENT, &mut d);
        d.into_iter()
            .find(|&n| s.dom.element(n).and_then(|e| e.id()) == Some(want.as_str()))
    });
    Ok(found.map(|n| n as i32).unwrap_or(-1).into())
}

fn n_connected(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    Ok(with(|s| s.dom.connected(id)).into())
}

fn n_rect(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let r = with(|s| s.boxes.iter().find(|(u, _)| *u == id).map(|(_, r)| *r)).unwrap_or(Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    });
    Ok(JsArray::from_iter([r.x, r.y, r.w, r.h].map(|v| JsValue::from(v as f64)), c).into())
}

/// A box's scroll state: `[x, y, maxX, maxY, width, height]`, or null if
/// it doesn't scroll.
fn n_scroll(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    Ok(
        match with(|s| s.scrollers.iter().find(|(u, _)| *u == id).map(|(_, v)| *v)) {
            Some(v) => JsArray::from_iter(v.map(|v| JsValue::from(v as f64)), c).into(),
            None => JsValue::null(),
        },
    )
}

/// Scroll a box to (x, y); NaN leaves that coordinate as it is.
fn n_set_scroll(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(a, 0, c)?;
    let coord = |i: usize, c: &mut Context| -> JsResult<Option<f32>> {
        let v = a.get_or_undefined(i).to_number(c)?;
        Ok(v.is_finite().then_some(v.clamp(0.0, 1e7) as f32))
    };
    let (x, y) = (coord(1, c)?, coord(2, c)?);
    with(|s| {
        // Later ones replace earlier ones for the same box.
        if let Some(e) = s.scroll_sets.iter_mut().find(|e| e.0 == id) {
            e.1 = x.or(e.1);
            e.2 = y.or(e.2);
        } else if s.scroll_sets.len() < MAX_SCROLL_SETS {
            s.scroll_sets.push((id, x, y));
        }
        // Reading it back gives the new offset, as far as it can go.
        if let Some((_, v)) = s.scrollers.iter_mut().find(|(u, _)| *u == id) {
            if let Some(x) = x {
                v[0] = x.min(v[2]).round();
            }
            if let Some(y) = y {
                v[1] = y.min(v[3]).round();
            }
        }
    });
    Ok(JsValue::undefined())
}

/// Most boxes scrolled by scripts between two looks by the page.
const MAX_SCROLL_SETS: usize = 4096;

fn n_log(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let level = arg_str(a, 0, c)?;
    let msg = arg_str(a, 1, c)?;
    with(|s| push_console(s, format!("[{level}] {msg}")));
    Ok(JsValue::undefined())
}

/// A script read a detail of the browser that fingerprinting scripts
/// collect (only known names are kept).
fn n_read(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(a, 0, c)?;
    if let Some((n, _)) = crate::explain::FINGERPRINT.iter().find(|(n, _)| *n == name) {
        with(|s| s.read.insert(n));
    }
    Ok(JsValue::undefined())
}

fn n_navigate(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let target = arg_str(a, 0, c)?;
    with(|s| {
        let url = crate::net::resolve(&s.url, &target);
        // Only ordinary web addresses; the browser re-checks everything.
        if matches!(crate::net::scheme(&url).as_str(), "http" | "https") {
            s.navigate = Some(url);
        } else {
            push_console(s, format!("[brickle] blocked script navigation to {target}"));
        }
    });
    Ok(JsValue::undefined())
}

fn n_set_url(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    // history.pushState: same origin only, no navigation.
    let target = arg_str(a, 0, c)?;
    let ok = with(|s| {
        let new = crate::net::resolve(&s.url, &target);
        let same = url::Url::parse(&new).ok().map(|u| u.origin())
            == url::Url::parse(&s.url).ok().map(|u| u.origin());
        if same {
            s.url = new;
        }
        same
    });
    if !ok {
        return Err(dom_error("SecurityError: pushState to another origin"));
    }
    Ok(JsValue::undefined())
}

fn n_url(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(js_string!(with(|s| s.url.clone())).into())
}

/// May this inline handler run? If so, the next compile of exactly this
/// code is let through even without `'unsafe-eval'`.
fn n_allow_handler(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let code = arg_str(a, 0, c)?;
    Ok(with(|s| {
        if s.csp.handler(&code) {
            s.approved_handler = Some(code);
            true
        } else {
            push_console(
                s,
                "[csp] refused an inline event handler ('unsafe-inline' isn't allowed)".into(),
            );
            false
        }
    })
    .into())
}

fn n_resolve(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let href = arg_str(a, 0, c)?;
    Ok(js_string!(with(|s| crate::net::resolve(&s.url, &href))).into())
}

fn n_request(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let method = arg_str(a, 0, c)?;
    let url = arg_str(a, 1, c)?;
    let mut headers = Vec::new();
    if let Some(list) = a.get_or_undefined(2).as_object() {
        let n = list.get(js_string!("length"), c)?.to_length(c)?.min(64);
        let mut i = 0;
        while i + 1 < n {
            let k = list.get(i, c)?.to_string(c)?.to_std_string_escaped();
            let v = list.get(i + 1, c)?.to_string(c)?.to_std_string_escaped();
            headers.push((k, v));
            i += 2;
        }
    }
    let body = if a.get_or_undefined(3).is_null_or_undefined() {
        None
    } else {
        Some(arg_str(a, 3, c)?)
    };
    let credentials = match arg_str(a, 4, c)?.as_str() {
        "omit" => Credentials::Omit,
        "include" => Credentials::Include,
        _ => Credentials::SameOrigin,
    };
    let mode = match arg_str(a, 5, c)?.as_str() {
        "same-origin" => crate::net::Mode::SameOrigin,
        "no-cors" => crate::net::Mode::NoCors,
        _ => crate::net::Mode::Cors,
    };
    let redirect = match arg_str(a, 6, c)?.as_str() {
        "error" => crate::net::Redirect::Error,
        "manual" => crate::net::Redirect::Manual,
        _ => crate::net::Redirect::Follow,
    };
    let refused = with(|s| {
        let url = crate::net::resolve(&s.url, &url);
        (!s.csp.connect(&url)).then(|| {
            let msg = format!("Content Security Policy: connect-src doesn't allow {url}");
            push_console(s, format!("[csp] {msg}"));
            msg
        })
    });
    if let Some(msg) = refused {
        return Err(type_error(&msg));
    }
    let id = with(|s| {
        if s.requests.len() >= MAX_QUEUED_REQUESTS {
            return None;
        }
        s.next_request += 1;
        let url = crate::net::resolve(&s.url, &url);
        s.requests.push((
            s.next_request,
            url,
            ScriptRequest {
                method,
                headers,
                body,
                credentials,
                mode,
                redirect,
                csp: s.csp.connect_sources(),
            },
        ));
        Some(s.next_request)
    });
    match id {
        Some(id) => Ok(id.into()),
        None => Err(JsNativeError::range()
            .with_message("too many requests waiting")
            .into()),
    }
}

fn n_cookie(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(js_string!(with(|s| s.cookies.clone())).into())
}

fn n_set_cookie(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let v = arg_str(a, 0, c)?;
    if v.len() > 4096 {
        return Ok(JsValue::undefined());
    }
    with(|s| {
        if s.cookie_sets.len() < 100 {
            // Show the change at once; the browser's answer replaces this.
            if let Some((name, value)) = v.split(';').next().and_then(|p| p.split_once('=')) {
                let (name, value) = (name.trim(), value.trim());
                let mut parts: Vec<String> = s
                    .cookies
                    .split("; ")
                    .filter(|p| !p.is_empty() && p.split('=').next() != Some(name))
                    .map(str::to_string)
                    .collect();
                if !value.is_empty() && !v.to_ascii_lowercase().contains("max-age=0") {
                    parts.push(format!("{name}={value}"));
                }
                s.cookies = parts.join("; ");
            }
            s.cookie_sets.push(v);
        }
    });
    Ok(JsValue::undefined())
}

fn n_submit(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let form = arg_id(a, 0, c)?;
    let submitter = if a.get_or_undefined(1).is_null_or_undefined() {
        None
    } else {
        Some(arg_id(a, 1, c)?)
    };
    with(|s| s.submit = Some((form, submitter)));
    Ok(JsValue::undefined())
}

const NATIVES: &[(&str, Native, usize)] = &[
    ("submit", n_submit, 2),
    ("request", n_request, 5),
    ("cookie", n_cookie, 0),
    ("setCookie", n_set_cookie, 1),
    ("nodeType", n_node_type, 1),
    ("tag", n_tag, 1),
    ("parent", n_parent, 1),
    ("children", n_children, 1),
    ("textContent", n_text_content, 1),
    ("setTextContent", n_set_text_content, 2),
    ("getAttr", n_get_attr, 2),
    ("setAttr", n_set_attr, 3),
    ("removeAttr", n_remove_attr, 2),
    ("attrNames", n_attr_names, 1),
    ("createElement", n_create_element, 1),
    ("createText", n_create_text, 1),
    ("createComment", n_create_comment, 1),
    ("insert", n_insert, 3),
    ("remove", n_remove, 1),
    ("clone", n_clone, 2),
    ("innerHTML", n_inner_html, 1),
    ("outerHTML", n_outer_html, 1),
    ("setInnerHTML", n_set_inner_html, 2),
    ("query", n_query, 3),
    ("matches", n_matches, 2),
    ("byId", n_by_id, 1),
    ("connected", n_connected, 1),
    ("rect", n_rect, 1),
    ("log", n_log, 2),
    ("scroll", n_scroll, 1),
    ("setScroll", n_set_scroll, 3),
    ("read", n_read, 1),
    ("navigate", n_navigate, 1),
    ("setUrl", n_set_url, 1),
    ("url", n_url, 0),
    ("resolve", n_resolve, 1),
    ("allowHandler", n_allow_handler, 1),
];

// ---------------------------------------------------------------------------
// The runtime
// ---------------------------------------------------------------------------

/// Time as scripts see it: UTC, like Firefox's fingerprinting protection
/// (and LibreWolf), so the time zone doesn't identify you.
#[derive(Debug)]
struct Hooks;

impl HostHooks for Hooks {
    fn local_timezone_offset_seconds(&self, _unix_time_seconds: i64) -> i32 {
        0
    }
    /// `eval`, `new Function` and timers given strings need the page's
    /// policy to allow `'unsafe-eval'`, except an inline handler the policy
    /// allowed just now (`allowHandler`), compiled once.
    fn ensure_can_compile_strings(
        &self,
        _realm: boa_engine::realm::Realm,
        _parameters: &[JsString],
        body: &JsString,
        _direct: bool,
        _context: &mut Context,
    ) -> JsResult<()> {
        with(|s| {
            if s.csp.eval() {
                return Ok(());
            }
            if s.approved_handler
                .take()
                .is_some_and(|h| h == body.to_std_string_escaped())
            {
                return Ok(());
            }
            push_console(
                s,
                "[csp] refused to evaluate a string as code ('unsafe-eval' isn't allowed)".into(),
            );
            Err(JsNativeError::eval()
                .with_message("Content Security Policy: 'unsafe-eval' isn't allowed")
                .into())
        })
    }
    fn max_buffer_size(&self, _context: &mut Context) -> u64 {
        256 * 1024 * 1024
    }
}

/// Functions from prelude.js that the renderer calls.
struct Internals {
    run_script: JsObject,
    step_timers: JsObject,
    dispatch: JsObject,
    fire_load: JsObject,
    fire_event: JsObject,
    set_viewport: JsObject,
    deliver: JsObject,
}

/// What a click did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClickResult {
    /// A handler called `preventDefault()`: don't follow the link.
    pub prevented: bool,
    /// The page changed and needs a new layout.
    pub changed: bool,
}

pub struct Runtime {
    // Declared before `ctx` so these handles are dropped first.
    internals: Internals,
    ctx: Context,
    // Declared last, so it runs after the context is gone.
    _collect: CollectOnDrop,
}

/// Frees JavaScript objects the page left in reference cycles, which Boa
/// otherwise only collects on a later allocation (so a long-lived thread,
/// like the fuzzer's, would keep them).
struct CollectOnDrop;

impl Drop for CollectOnDrop {
    fn drop(&mut self) {
        boa_engine::gc::force_collect();
    }
}

/// Viewport size as scripts see it: rounded down to steps of 200x100 px,
/// like Firefox's letterboxing, so the window size identifies you less.
pub fn rounded_viewport(w: f32, h: f32) -> (f64, f64) {
    let r = |v: f32, step: f32| ((v / step).floor() * step).max(step) as f64;
    (r(w, 200.0), r(h, 100.0))
}

impl Runtime {
    /// Set up scripting for a page. Only one runtime per thread.
    pub fn new(tree: &Node, url: &str, viewport: (f32, f32)) -> Result<Runtime, String> {
        with(|s| {
            *s = State {
                dom: Dom::from_tree(tree),
                url: url.to_string(),
                ..State::default()
            }
        });
        let mut ctx = Context::builder()
            .host_hooks(Rc::new(Hooks))
            .job_executor(Rc::new(Jobs::default()))
            .build()
            .map_err(|e| format!("can't start JavaScript: {e}"))?;
        {
            let limits = ctx.runtime_limits_mut();
            limits.set_loop_iteration_limit(LOOP_LIMIT);
            limits.set_recursion_limit(RECURSION_LIMIT);
        }
        let mut obj = ObjectInitializer::new(&mut ctx);
        for (name, f, len) in NATIVES {
            let f: Native = *f;
            let checked = NativeFunction::from_copy_closure(move |this, args, ctx| {
                check_deadline()?;
                f(this, args, ctx)
            });
            obj.function(checked, js_string!(*name), *len);
        }
        let (vw, vh) = rounded_viewport(viewport.0, viewport.1);
        obj.property(
            js_string!("ua"),
            js_string!(crate::net::user_agent()),
            Attribute::READONLY,
        );
        let natives = obj.build();
        let setup = ctx
            .eval(Source::from_bytes(PRELUDE))
            .map_err(|e| format!("prelude: {e}"))?;
        let setup = setup.as_object().ok_or("prelude didn't return a function")?;
        let internals = setup
            .call(&JsValue::undefined(), &[natives.into()], &mut ctx)
            .map_err(|e| format!("prelude: {e}"))?;
        let internals = internals
            .as_object()
            .ok_or("prelude didn't return its internals")?;
        let get = |name: &str, ctx: &mut Context| -> Result<JsObject, String> {
            internals
                .get(js_string!(name), ctx)
                .ok()
                .and_then(|v| v.as_object())
                .ok_or(format!("prelude is missing {name}"))
        };
        let internals = Internals {
            run_script: get("runScript", &mut ctx)?,
            step_timers: get("stepTimers", &mut ctx)?,
            dispatch: get("dispatch", &mut ctx)?,
            fire_load: get("fireLoad", &mut ctx)?,
            fire_event: get("fireEvent", &mut ctx)?,
            set_viewport: get("setViewport", &mut ctx)?,
            deliver: get("deliver", &mut ctx)?,
        };
        let mut rt = Runtime {
            internals,
            ctx,
            _collect: CollectOnDrop,
        };
        rt.call(&rt.internals.set_viewport.clone(), &[vw.into(), vh.into()]);
        Ok(rt)
    }

    /// Call an internal function; exceptions go to the console.
    fn call(&mut self, f: &JsObject, args: &[JsValue]) -> Option<JsValue> {
        let r = f.call(&JsValue::undefined(), args, &mut self.ctx);
        let _ = self.ctx.run_jobs();
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                let msg = e.to_string();
                with(|s| push_console(s, format!("[error] {msg}")));
                None
            }
        }
    }

    /// Run one classic script. `script` is its element (for
    /// `document.currentScript`).
    pub fn run_script(&mut self, code: &str, script: u32, name: &str) {
        let f = self.internals.run_script.clone();
        self.call(&f, &[JsValue::undefined(), script.into()]);
        // Evaluated as a classic script, so top-level declarations are
        // shared between scripts as in browsers.
        let r = self.ctx.eval(Source::from_bytes(code));
        let _ = self.ctx.run_jobs();
        if let Err(e) = r {
            let msg = e.to_string();
            with(|s| push_console(s, format!("[error] {name}: {msg}")));
        }
        self.call(&f, &[JsValue::undefined(), (-1).into()]);
    }

    /// Stop scripts that are still running at `until` (None: no limit
    /// beyond the loop and recursion limits).
    pub fn set_deadline(&mut self, until: Option<Instant>) {
        DEADLINE.with(|d| d.set(until));
        STOP_NOTED.with(|n| n.set(false));
    }

    /// Run due timers, up to the batch limits or `until`.
    pub fn run_timers(&mut self, until: Instant) {
        let f = self.internals.step_timers.clone();
        for i in 0..MAX_CALLBACKS {
            if Instant::now() >= until {
                with(|s| push_console(s, "[brickle] scripts ran out of time".into()));
                break;
            }
            let window = if i == 0 { TIMER_WINDOW_MS } else { -1.0 };
            match self.call(&f, &[window.into()]) {
                Some(v) if v.to_boolean() => {}
                _ => break,
            }
        }
    }

    /// `DOMContentLoaded` and `load`.
    pub fn fire_load(&mut self) {
        let f = self.internals.fire_load.clone();
        self.call(&f, &[]);
    }

    /// Dispatch a click on node `uid` (or the element containing it).
    pub fn click(&mut self, uid: u32, until: Instant) -> ClickResult {
        let before = self.version();
        let target = with(|s| s.dom.element_at_or_above(uid));
        let Some(target) = target else {
            return ClickResult::default();
        };
        let f = self.internals.dispatch.clone();
        let prevented = self
            .call(&f, &[target.into(), js_string!("click").into()])
            .is_some_and(|v| v.to_boolean());
        self.run_timers(until);
        ClickResult {
            prevented,
            changed: self.version() != before,
        }
    }

    /// Requests scripts made since last asked: (id, absolute URL, request).
    pub fn take_requests(&mut self) -> Vec<(u32, String, ScriptRequest)> {
        with(|s| std::mem::take(&mut s.requests))
    }

    /// `document.cookie` assignments since last asked.
    pub fn take_cookie_sets(&mut self) -> Vec<String> {
        with(|s| std::mem::take(&mut s.cookie_sets))
    }

    /// The browser's current `document.cookie` for the page.
    pub fn set_cookies(&mut self, cookies: String) {
        with(|s| s.cookies = cookies);
    }

    /// Hand a request's answer to the script that made it.
    pub fn deliver(&mut self, id: u32, result: Result<ScriptResponse, String>) {
        let f = self.internals.deliver.clone();
        let args: Vec<JsValue> = match result {
            Ok(r) => {
                let mut flat = Vec::new();
                for (k, v) in r.headers {
                    flat.push(JsValue::from(js_string!(k)));
                    flat.push(JsValue::from(js_string!(v)));
                }
                let headers = JsArray::from_iter(flat, &mut self.ctx);
                vec![
                    id.into(),
                    JsValue::null(),
                    (r.status as u32).into(),
                    js_string!(r.url).into(),
                    r.redirected.into(),
                    headers.into(),
                    js_string!(r.body).into(),
                ]
            }
            Err(e) => vec![id.into(), js_string!(e).into()],
        };
        self.call(&f, &args);
    }

    /// Use the page's DOM directly (forms: typed text, checkboxes).
    pub fn with_dom<R>(&mut self, f: impl FnOnce(&mut Dom) -> R) -> R {
        with(|s| f(&mut s.dom))
    }

    /// Dispatch a plain event (`input`, `change`, `submit`, `focus`) on a
    /// node. Returns whether a handler called `preventDefault()`.
    pub fn fire(&mut self, uid: u32, event: &str, bubbles: bool, cancelable: bool) -> bool {
        let f = self.internals.fire_event.clone();
        self.call(
            &f,
            &[
                uid.into(),
                js_string!(event).into(),
                bubbles.into(),
                cancelable.into(),
            ],
        )
        .is_some_and(|v| v.to_boolean())
    }

    /// A form a script asked to submit (`form.submit()`), and its submitter.
    pub fn take_submit(&mut self) -> Option<(u32, Option<u32>)> {
        with(|s| s.submit.take())
    }

    pub fn version(&self) -> u64 {
        with(|s| s.dom.version)
    }

    pub fn tree(&self) -> Node {
        with(|s| s.dom.to_tree())
    }

    pub fn url(&self) -> String {
        with(|s| s.url.clone())
    }

    pub fn set_boxes(&mut self, boxes: Vec<(u32, Rect)>) {
        with(|s| s.boxes = boxes);
    }

    pub fn set_scrollers(&mut self, scrollers: Vec<(u32, [f32; 6])>) {
        with(|s| s.scrollers = scrollers);
    }

    /// The scrolling scripts asked for since last asked.
    pub fn take_scroll_sets(&mut self) -> Vec<(u32, Option<f32>, Option<f32>)> {
        with(|s| std::mem::take(&mut s.scroll_sets))
    }

    pub fn take_console(&mut self) -> Vec<String> {
        with(|s| std::mem::take(&mut s.console))
    }

    /// The details of the browser scripts read since last asked.
    pub fn take_read(&mut self) -> Vec<String> {
        with(|s| {
            std::mem::take(&mut s.read)
                .into_iter()
                .map(String::from)
                .collect()
        })
    }

    pub fn take_navigate(&mut self) -> Option<String> {
        with(|s| s.navigate.take())
    }

    /// Script elements scripts inserted since last asked: (id, src, inline code).
    pub fn take_pending_scripts(&mut self) -> Vec<(u32, Option<String>, String)> {
        with(|s| {
            let pending = std::mem::take(&mut s.pending_scripts);
            pending
                .into_iter()
                .filter_map(|id| {
                    let e = s.dom.element(id)?;
                    if !is_classic_script(e) {
                        return None;
                    }
                    let src = e.attrs.get("src").map(|src| crate::net::resolve(&s.url, src));
                    let mut code = String::new();
                    s.dom.text_content(id, &mut code);
                    Some((id, src, code))
                })
                .collect()
        })
    }

    /// Hold the page's scripts to this policy.
    pub fn set_csp(&mut self, csp: crate::csp::Csp) {
        with(|s| s.csp = csp);
    }

    /// May script element `id` run (from `src`, else its `code`)? Refusals
    /// go to the console. `parser_inserted`: it came with the document.
    pub fn script_allowed(&self, id: u32, src: Option<&str>, code: &str, parser_inserted: bool) -> bool {
        with(|s| {
            let nonce = s.dom.element(id).and_then(|e| e.attrs.get("nonce").cloned());
            let ok = s.csp.script_element(nonce.as_deref(), src, code, parser_inserted);
            if !ok {
                let what = src.map_or("an inline script".to_string(), |u| u.to_string());
                push_console(s, format!("[csp] refused to run {what}"));
            }
            ok
        })
    }

    /// Where external script element `id`, which the policy allowed, may
    /// be redirected ([`crate::csp::Csp::script_sources`]).
    pub fn script_sources(&self, id: u32, parser_inserted: bool) -> Vec<Vec<String>> {
        with(|s| {
            let nonce = s.dom.element(id).and_then(|e| e.attrs.get("nonce").cloned());
            s.csp.script_sources(nonce.as_deref(), parser_inserted)
        })
    }

    /// Does the code fetched for script element `id` match its `integrity`
    /// attribute (if any)? A mismatch goes to the console.
    pub fn integrity_ok(&self, id: u32, url: &str, code: &str) -> bool {
        with(|s| {
            let want = s.dom.element(id).and_then(|e| e.attrs.get("integrity").cloned());
            let ok = want.is_none_or(|w| crate::csp::integrity_matches(&w, code));
            if !ok {
                push_console(
                    s,
                    format!("[integrity] {url} doesn't match its integrity hash; not run"),
                );
            }
            ok
        })
    }

    /// Script elements in the document, in order: (id, src, inline code).
    pub fn document_scripts(&self) -> Vec<(u32, Option<String>, String)> {
        with(|s| {
            let mut all = Vec::new();
            s.dom.descendants(DOCUMENT, &mut all);
            all.into_iter()
                .filter_map(|id| {
                    let e = s.dom.element(id)?;
                    if e.tag != "script" || !is_classic_script(e) {
                        return None;
                    }
                    let src = e.attrs.get("src").map(|src| crate::net::resolve(&s.url, src));
                    let mut code = String::new();
                    s.dom.text_content(id, &mut code);
                    Some((id, src, code))
                })
                .collect()
        })
    }
}

/// Classic JavaScript: no `type`, or a JavaScript MIME type. Modules aren't
/// supported yet, so `nomodule` fallbacks run, as in older browsers.
fn is_classic_script(e: &ElementData) -> bool {
    match e.attrs.get("type").map(|t| t.trim().to_ascii_lowercase()) {
        None => true,
        Some(t) => matches!(
            t.as_str(),
            "" | "text/javascript"
                | "application/javascript"
                | "application/x-javascript"
                | "text/ecmascript"
        ),
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        with(|s| *s = State::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(html: &str, script: &str) -> (Runtime, String) {
        let tree = crate::html::parse(html);
        let mut rt = Runtime::new(&tree, "https://example.com/dir/page.html", (1000.0, 700.0)).unwrap();
        rt.run_script(script, 0, "test");
        rt.fire_load();
        rt.run_timers(Instant::now() + Duration::from_secs(2));
        let mut out = String::new();
        rt.tree().dump(0, &mut out);
        (rt, out)
    }

    /// A page's Promise polyfill built on queueMicrotask (as Yahoo's is)
    /// mustn't loop through Brickle's queueMicrotask forever.
    #[test]
    fn queue_microtask_survives_a_promise_polyfill() {
        let (_, out) = run(
            "<p id=p></p>",
            "const Native = Promise;\
             globalThis.Promise = function (f) { const p = new Native(f); return p; };\
             Promise.resolve = (v) => { queueMicrotask(() => {}); return Native.resolve(v); };\
             queueMicrotask(() => { document.getElementById('p').textContent = 'ran'; });",
        );
        assert!(out.contains("\"ran\""), "{out}");
    }

    /// Waiting for time to pass (as schedulers do) finishes: each look at
    /// the clock moves it on a little.
    #[test]
    fn performance_now_moves_on() {
        let (_, out) = run(
            "<p id=p></p>",
            "const end = performance.now() + 50; let n = 0;\
             while (performance.now() < end) n++;\
             document.getElementById('p').textContent = 'waited ' + (n > 0);",
        );
        assert!(out.contains("waited true"), "{out}");
    }

    /// A promise chain that never ends stops at the deadline, keeping what
    /// the scripts did before it.
    #[test]
    fn endless_promise_chains_stop_at_the_deadline() {
        let tree = crate::html::parse("<p id=p></p>");
        let mut rt = Runtime::new(&tree, "https://example.com/", (1000.0, 700.0)).unwrap();
        let start = Instant::now();
        rt.set_deadline(Some(start + Duration::from_millis(300)));
        rt.run_script(
            "document.getElementById('p').textContent = 'before';\
             (function f() { Promise.resolve().then(f); })();",
            0,
            "test",
        );
        rt.set_deadline(None);
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
        let mut out = String::new();
        rt.tree().dump(0, &mut out);
        assert!(out.contains("before"), "{out}");
        assert!(rt.take_console().iter().any(|l| l.contains("ran out of time")));
    }

    #[test]
    fn base64() {
        let (mut rt, _) = run(
            "",
            r#"
            console.log(btoa(''), btoa('f'), btoa('fo'), btoa('foo'), btoa('\xff\x00'));
            console.log(atob('Zm9v'), atob(' Zm 8= '), atob('Zg'), atob(btoa('h\xe9llo')) === 'h\xe9llo');
            for (const f of [() => btoa('\u0100'), () => atob('Z'), () => atob('Zm9v!')]) {
                try { f(); console.log('no error'); } catch (e) { console.log(e.name); }
            }
            "#,
        );
        assert_eq!(
            rt.take_console(),
            vec![
                "[log]  Zg== Zm8= Zm9v /wA=",
                "[log] foo fo f true",
                "[log] InvalidCharacterError",
                "[log] InvalidCharacterError",
                "[log] InvalidCharacterError",
            ]
        );
    }

    #[test]
    fn dom_basics() {
        let (mut rt, out) = run(
            "<p id=a class='x y'>hi</p><ul></ul>",
            r#"
            const p = document.getElementById('a');
            p.textContent = 'changed';
            p.classList.add('z'); p.classList.remove('x');
            const ul = document.querySelector('ul');
            for (const t of ['one', 'two']) {
                const li = document.createElement('li');
                li.appendChild(document.createTextNode(t));
                ul.appendChild(li);
            }
            document.body.insertAdjacentHTML('beforeend', '<b>bold</b>');
            console.log(document.querySelectorAll('li').length, p === document.getElementById('a'),
                        p.parentNode === document.body, ul.children[1].textContent);
            "#,
        );
        assert!(out.contains("\"changed\""), "{out}");
        assert!(out.contains("class=\"y z\""), "{out}");
        assert!(
            out.contains("\"one\"") && out.contains("\"two\"") && out.contains("\"bold\""),
            "{out}"
        );
        assert_eq!(rt.take_console(), vec!["[log] 2 true true two"]);
    }

    #[test]
    fn events_timers_and_load() {
        let (mut rt, out) = run(
            "<div id=d></div><a id=l href=/x onclick='return false'>x</a>",
            r#"
            const d = document.getElementById('d');
            document.addEventListener('DOMContentLoaded', () => d.textContent += 'ready ');
            window.addEventListener('load', () => d.textContent += 'load ');
            setTimeout(() => d.textContent += 'timer ', 100);
            Promise.resolve().then(() => d.textContent += 'micro ');
            let n = 0; const iv = setInterval(() => { if (++n == 3) { clearInterval(iv); d.textContent += 'x3'; } }, 10);
            d.addEventListener('click', (e) => { d.dataset.clicked = 'yes'; e.preventDefault(); });
            "#,
        );
        assert!(out.contains("\"micro ready load x3timer\""), "{out}");
        let d = with(|s| {
            let mut all = vec![];
            s.dom.descendants(DOCUMENT, &mut all);
            all.into_iter()
                .find(|&n| s.dom.element(n).and_then(|e| e.id()) == Some("d"))
                .unwrap()
        });
        let r = rt.click(d, Instant::now() + Duration::from_secs(1));
        assert!(r.prevented && r.changed);
        assert!(rt.tree().text_content().contains("x3"));
        let mut out = String::new();
        rt.tree().dump(0, &mut out);
        assert!(out.contains("data-clicked=\"yes\""), "{out}");
        // Inline handler returning false prevents the default action.
        let l = with(|s| {
            let mut all = vec![];
            s.dom.descendants(DOCUMENT, &mut all);
            all.into_iter()
                .find(|&n| s.dom.element(n).and_then(|e| e.id()) == Some("l"))
                .unwrap()
        });
        assert!(rt.click(l, Instant::now() + Duration::from_secs(1)).prevented);
    }

    #[test]
    fn runaway_scripts_are_stopped() {
        let (mut rt, out) = run(
            "<p id=p>start</p>",
            "document.getElementById('p').textContent = 'before'; while (true) {}",
        );
        assert!(out.contains("before"), "{out}");
        assert!(rt.take_console().iter().any(|l| l.contains("error")));
        // Catching the limit doesn't help: it can't be caught.
        let (mut rt, _) = run(
            "",
            "for (;;) { try { for (;;) {} } catch (e) {} } console.log('escaped');",
        );
        let c = rt.take_console();
        assert!(
            c.iter().any(|l| l.contains("RuntimeLimit")) && !c.iter().any(|l| l.contains("escaped")),
            "{c:?}"
        );
        let (mut rt, _) = run("", "function f() { f(); } f();");
        assert!(rt.take_console().iter().any(|l| l.contains("error")));
        // A timer that keeps rescheduling itself stops at the batch limit.
        let start = Instant::now();
        run("", "function t() { setTimeout(t, 0); } t();");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    /// A stand-in browser: records requests and answers them.
    struct FakeHost {
        seen: Vec<(String, ScriptRequest)>,
        cookies: String,
    }

    impl crate::page::Host for FakeHost {
        fn fetch(&mut self, _: &str, _: crate::net::Dest) -> Result<String, String> {
            Err("no files".into())
        }
        fn request(&mut self, url: &str, req: ScriptRequest) -> Result<ScriptResponse, String> {
            self.seen.push((url.to_string(), req));
            if url.contains("blocked") {
                return Err("blocked by a filter".into());
            }
            Ok(ScriptResponse {
                status: 200,
                url: url.to_string(),
                redirected: false,
                headers: vec![("content-type".into(), "application/json".into())],
                body: "{\"n\": 42}".into(),
            })
        }
        fn set_cookie(&mut self, v: &str) {
            let pair = v.split(';').next().unwrap_or("").trim();
            self.cookies = if self.cookies.is_empty() {
                pair.to_string()
            } else {
                format!("{}; {pair}", self.cookies)
            };
        }
        fn cookies(&mut self) -> String {
            self.cookies.clone()
        }
    }

    #[test]
    fn script_requests_go_through_the_browser() {
        let tree = crate::html::parse("<p id=out></p>");
        let mut rt = Runtime::new(&tree, "https://example.com/app/", (1000.0, 700.0)).unwrap();
        let mut host = FakeHost {
            seen: vec![],
            cookies: "theme=dark".into(),
        };
        rt.set_cookies(host.cookies.clone());
        rt.run_script(
            r#"
            console.log(document.cookie, navigator.cookieEnabled);
            document.cookie = 'lang=en; Path=/';
            fetch('api/data?x=1', { method: 'POST', headers: { 'X-Thing': 'a' }, body: JSON.stringify({a: 1}),
                                    credentials: 'include' })
              .then(r => r.json()).then(j => console.log('fetch', j.n));
            fetch('https://blocked.example/t').catch(e => console.log('rejected', e instanceof TypeError));
            const x = new XMLHttpRequest();
            x.onload = () => console.log('xhr', x.status, x.getResponseHeader('Content-Type'), JSON.parse(x.responseText).n);
            x.open('GET', '/other'); x.send();
            const f = new FormData(); f.append('q', 'cats');
            fetch('/form', { method: 'POST', body: f });
            const c = new AbortController(); fetch('/aborted', { signal: c.signal }).catch(e => console.log('aborted'));
            c.abort();
            fetch(new Request('/opts', { mode: 'same-origin', redirect: 'error' }));
            fetch('/manual', { mode: 'no-cors', redirect: 'manual' });
            "#,
            0,
            "test",
        );
        crate::page::pump(&mut rt, &mut host, Instant::now() + Duration::from_secs(2));
        rt.run_script("console.log(document.cookie)", 0, "after");
        let c = rt.take_console();
        assert_eq!(c[0], "[log] theme=dark true");
        for want in [
            "[log] fetch 42",
            "[log] rejected true",
            "[log] xhr 200 application/json 42",
            "[log] aborted",
        ] {
            assert!(c.contains(&want.to_string()), "{want}: {c:?}");
        }
        assert_eq!(c.last().unwrap(), "[log] theme=dark; lang=en", "{c:?}");
        let (url, req) = &host.seen[0];
        assert_eq!(url, "https://example.com/app/api/data?x=1");
        assert_eq!(req.method, "POST");
        assert_eq!(req.body.as_deref(), Some("{\"a\":1}"));
        assert_eq!(req.credentials, Credentials::Include);
        assert!(
            req.headers.contains(&("x-thing".into(), "a".into())),
            "{:?}",
            req.headers
        );
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.starts_with("text/plain")));
        assert_eq!(
            (req.mode, req.redirect),
            (crate::net::Mode::Cors, crate::net::Redirect::Follow)
        );
        let opts = |end: &str| {
            host.seen
                .iter()
                .find(|(u, _)| u.ends_with(end))
                .unwrap()
                .1
                .clone()
        };
        let o = opts("/opts");
        assert_eq!(
            (o.mode, o.redirect),
            (crate::net::Mode::SameOrigin, crate::net::Redirect::Error)
        );
        let o = opts("/manual");
        assert_eq!(
            (o.mode, o.redirect),
            (crate::net::Mode::NoCors, crate::net::Redirect::Manual)
        );
        let form = host.seen.iter().find(|(u, _)| u.ends_with("/form")).unwrap();
        let body = form.1.body.as_deref().unwrap();
        assert!(body.contains("name=\"q\"") && body.contains("cats"), "{body}");
        assert!(form
            .1
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.starts_with("multipart/form-data; boundary=")));
    }

    #[test]
    fn storage_lasts_only_for_the_page() {
        let (mut rt, _) = run(
            "",
            r#"
            localStorage.setItem('a', '1'); console.log(localStorage.getItem('a'));
            console.log(new Date(0).getTimezoneOffset(), innerWidth, innerHeight, screen.width);
            navigator.sendBeacon('/beacon', 'x');
            "#,
        );
        let c = rt.take_console();
        assert!(c.contains(&"[log] 1".to_string()), "{c:?}");
        assert!(c.contains(&"[log] 0 1000 700 1920".to_string()), "{c:?}");
        assert!(rt.take_requests().is_empty(), "sendBeacon sends nothing");
    }

    #[test]
    fn reading_browser_details_is_noted() {
        let (mut rt, _) = run(
            "",
            r#"
            const fp = [navigator.platform, navigator.languages, navigator.hardwareConcurrency,
                screen.width, screen.height, new Date().getTimezoneOffset(), devicePixelRatio];
            console.log(navigator.cookieEnabled, typeof navigator.sendBeacon, fp.length);
            "#,
        );
        assert!(rt.take_console().contains(&"[log] true function 7".to_string()));
        let read = rt.take_read();
        for name in [
            "navigator.platform",
            "navigator.hardwareConcurrency",
            "screen.width",
            "timezone",
            "devicePixelRatio",
        ] {
            assert!(read.iter().any(|r| r == name), "{name} in {read:?}");
        }
        assert!(
            !read.iter().any(|r| r.contains("cookieEnabled")),
            "only watched details: {read:?}"
        );
        assert!(rt.take_read().is_empty(), "taken once");
        // Scripts can't change what's reported.
        let (mut rt, _) = run(
            "",
            "try { navigator.platform = 'x'; } catch (e) {} console.log(navigator.platform);",
        );
        assert!(rt.take_console().contains(&"[log] Linux x86_64".to_string()));
    }

    #[test]
    fn navigation_requests_are_checked() {
        let (mut rt, _) = run("", "location.href = 'other.html';");
        assert_eq!(
            rt.take_navigate().as_deref(),
            Some("https://example.com/dir/other.html")
        );
        let (mut rt, _) = run(
            "",
            "location.href = 'javascript:alert(1)'; location.assign('file:///etc/passwd');",
        );
        assert_eq!(rt.take_navigate(), None);
        let (mut rt, _) = run(
            "",
            "history.pushState({}, '', '/new'); console.log(location.pathname);",
        );
        assert_eq!(rt.take_console(), vec!["[log] /new"]);
    }

    #[test]
    fn dropping_a_page_with_cycles_is_safe() {
        // Objects that only reference each other, and a listener closing
        // over its element; the runtime's drop collects them (the fuzzer's
        // leak checker is what verifies they're freed).
        for _ in 0..3 {
            run(
                "<div id=d></div>",
                "const a = {}; const b = {a}; a.b = b; const d = document.getElementById('d');\
                 d.addEventListener('click', () => d.x = a);",
            );
        }
        boa_engine::gc::force_collect();
    }

    #[test]
    fn hostile_dom_operations_fail_safely() {
        let (mut rt, _) = run(
            "<div id=a><div id=b></div></div>",
            r#"
            const a = document.getElementById('a'), b = document.getElementById('b');
            try { b.appendChild(a); } catch (e) { console.log('cycle refused'); }
            try { document.createElement('<script>'); } catch (e) { console.log('bad name'); }
            try { a.setAttribute('on click', 'x'); } catch (e) { console.log('bad attr'); }
            a.innerHTML = '<img src=x onerror=alert(1)><script>console.log("ran")</script>';
            "#,
        );
        let c = rt.take_console();
        assert_eq!(
            c,
            vec!["[log] cycle refused", "[log] bad name", "[log] bad attr"],
            "innerHTML scripts don't run"
        );
    }
}
