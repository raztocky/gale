//! Form controls: how they're drawn, what clicking and typing do, and what a
//! form submits. All of this runs in the (sandboxed) renderer, on the
//! page's own DOM; a submission is only a request the browser then checks
//! like any navigation.
//!
//! Values live in the DOM, as attributes (`value`, `checked`, an option's
//! `selected`) or a textarea's text, so scripts, rendering and submission
//! all see the same thing.

use crate::dom::{self, AttrMap, ElementData, Node, NodeType};
use crate::js::Dom;

/// Longest value typing can produce in one field.
const MAX_VALUE: usize = 64 * 1024;
/// Largest form body Brickle submits.
pub const MAX_BODY: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// text, search, email, url, tel, number, password, ...
    Text {
        password: bool,
    },
    TextArea,
    Checkbox,
    Radio,
    Select,
    Submit,
    Reset,
    Button,
    Hidden,
    File,
}

impl Control {
    pub fn takes_typing(self) -> bool {
        matches!(self, Control::Text { .. } | Control::TextArea)
    }
}

pub fn control(e: &ElementData) -> Option<Control> {
    let ty = e.attrs.get("type").map(|t| t.trim().to_ascii_lowercase());
    Some(match e.tag.as_str() {
        "input" => match ty.as_deref().unwrap_or("text") {
            "checkbox" => Control::Checkbox,
            "radio" => Control::Radio,
            "submit" | "image" => Control::Submit,
            "reset" => Control::Reset,
            "button" => Control::Button,
            "hidden" => Control::Hidden,
            "file" => Control::File,
            "password" => Control::Text { password: true },
            _ => Control::Text { password: false },
        },
        "textarea" => Control::TextArea,
        "select" => Control::Select,
        "button" => match ty.as_deref().unwrap_or("submit") {
            "reset" => Control::Reset,
            "button" => Control::Button,
            _ => Control::Submit,
        },
        _ => return None,
    })
}

fn kind(dom: &Dom, id: u32) -> Option<Control> {
    dom.element(id).and_then(control)
}

fn disabled(dom: &Dom, id: u32) -> bool {
    dom.element(id).is_some_and(|e| e.attrs.contains_key("disabled"))
}

/// The control a click on node `uid` is for: the control itself or its
/// ancestor, or the control a `<label>` is for.
pub fn control_at(dom: &Dom, uid: u32) -> Option<u32> {
    let mut cur = Some(uid);
    while let Some(id) = cur {
        if let Some(e) = dom.element(id) {
            if control(e).is_some() {
                return (!disabled(dom, id)).then_some(id);
            }
            if e.tag == "label" {
                if let Some(target) = e.attrs.get("for") {
                    return by_id(dom, target).filter(|&t| kind(dom, t).is_some() && !disabled(dom, t));
                }
                let mut d = Vec::new();
                dom.descendants(id, &mut d);
                return d
                    .into_iter()
                    .find(|&n| kind(dom, n).is_some() && !disabled(dom, n));
            }
            if e.tag == "a" {
                return None;
            }
        }
        cur = dom.parent(id);
    }
    None
}

fn by_id(dom: &Dom, want: &str) -> Option<u32> {
    let mut d = Vec::new();
    dom.descendants(0, &mut d);
    d.into_iter()
        .find(|&n| dom.element(n).and_then(|e| e.id()) == Some(want))
}

/// The form a control belongs to.
pub fn form_of(dom: &Dom, id: u32) -> Option<u32> {
    if let Some(f) = dom.element(id).and_then(|e| e.attrs.get("form")) {
        return by_id(dom, f).filter(|&n| dom.element(n).is_some_and(|e| e.tag == "form"));
    }
    let mut cur = dom.parent(id);
    while let Some(p) = cur {
        if dom.element(p).is_some_and(|e| e.tag == "form") {
            return Some(p);
        }
        cur = dom.parent(p);
    }
    None
}

/// Controls of a form, in document order.
fn controls(dom: &Dom, form: u32) -> Vec<u32> {
    let mut all = Vec::new();
    dom.descendants(0, &mut all);
    all.into_iter()
        .filter(|&n| kind(dom, n).is_some() && form_of(dom, n) == Some(form))
        .collect()
}

fn options(dom: &Dom, select: u32) -> Vec<u32> {
    let mut d = Vec::new();
    dom.descendants(select, &mut d);
    d.into_iter()
        .filter(|&n| dom.element(n).is_some_and(|e| e.tag == "option"))
        .collect()
}

fn selected_option(dom: &Dom, select: u32) -> Option<u32> {
    let opts = options(dom, select);
    opts.iter()
        .copied()
        .find(|&o| dom.element(o).is_some_and(|e| e.attrs.contains_key("selected")))
        .or_else(|| opts.first().copied())
}

fn text_of(dom: &Dom, id: u32) -> String {
    let mut s = String::new();
    dom.text_content(id, &mut s);
    s
}

fn option_value(dom: &Dom, o: u32) -> String {
    dom.element(o)
        .and_then(|e| e.attrs.get("value").cloned())
        .unwrap_or_else(|| text_of(dom, o).split_whitespace().collect::<Vec<_>>().join(" "))
}

/// A field's current value.
pub fn value(dom: &Dom, id: u32) -> String {
    match kind(dom, id) {
        Some(Control::TextArea) => text_of(dom, id),
        Some(Control::Select) => selected_option(dom, id)
            .map(|o| option_value(dom, o))
            .unwrap_or_default(),
        _ => dom
            .element(id)
            .and_then(|e| e.attrs.get("value").cloned())
            .unwrap_or_default(),
    }
}

fn set_value(dom: &mut Dom, id: u32, v: &str) {
    let mut v = v.to_string();
    let max = dom
        .element(id)
        .and_then(|e| e.attrs.get("maxlength"))
        .and_then(|m| m.trim().parse::<usize>().ok())
        .unwrap_or(MAX_VALUE)
        .min(MAX_VALUE);
    if v.chars().count() > max {
        v = v.chars().take(max).collect();
    }
    if kind(dom, id) == Some(Control::TextArea) {
        let _ = dom.set_text_content(id, v);
    } else {
        // A single-line field has no line breaks.
        let v: String = v.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        dom.set_attr(id, "value", &v);
    }
}

/// What a click on a control does by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// A field that takes typing now has the focus.
    Focus(u32),
    /// A checkbox, radio button or select changed.
    Changed(u32),
    /// Submit `form` (with this submit button).
    Submit { form: u32, submitter: u32 },
    /// Reset `form` to its original values (not supported: nothing).
    Nothing,
}

/// Do a control's default click action.
pub fn activate(dom: &mut Dom, id: u32) -> Activation {
    match kind(dom, id) {
        Some(c) if c.takes_typing() => {
            if dom.element(id).is_some_and(|e| e.attrs.contains_key("readonly")) {
                Activation::Nothing
            } else {
                Activation::Focus(id)
            }
        }
        Some(Control::Checkbox) => {
            if dom.element(id).is_some_and(|e| e.attrs.contains_key("checked")) {
                dom.remove_attr(id, "checked");
            } else {
                dom.set_attr(id, "checked", "");
            }
            Activation::Changed(id)
        }
        Some(Control::Radio) => {
            let name = dom.element(id).and_then(|e| e.attrs.get("name").cloned());
            if let Some(name) = name {
                let form = form_of(dom, id);
                let mut all = Vec::new();
                dom.descendants(0, &mut all);
                for other in all {
                    let same = other != id
                        && kind(dom, other) == Some(Control::Radio)
                        && dom.element(other).and_then(|e| e.attrs.get("name")) == Some(&name)
                        && form_of(dom, other) == form;
                    if same {
                        dom.remove_attr(other, "checked");
                    }
                }
            }
            dom.set_attr(id, "checked", "");
            Activation::Changed(id)
        }
        Some(Control::Select) => {
            // No drop-down yet: each click picks the next option.
            let opts = options(dom, id);
            if opts.len() > 1 {
                let cur = selected_option(dom, id).and_then(|s| opts.iter().position(|&o| o == s));
                let next = opts[cur.map(|i| (i + 1) % opts.len()).unwrap_or(0)];
                for &o in &opts {
                    dom.remove_attr(o, "selected");
                }
                dom.set_attr(next, "selected", "");
                return Activation::Changed(id);
            }
            Activation::Nothing
        }
        Some(Control::Submit) => match form_of(dom, id) {
            Some(form) => Activation::Submit { form, submitter: id },
            None => Activation::Nothing,
        },
        _ => Activation::Nothing,
    }
}

/// Type text into a field. Returns whether anything changed.
pub fn type_text(dom: &mut Dom, id: u32, text: &str) -> bool {
    if !kind(dom, id).is_some_and(Control::takes_typing) || text.is_empty() {
        return false;
    }
    let v = value(dom, id) + text;
    set_value(dom, id, &v);
    true
}

pub fn backspace(dom: &mut Dom, id: u32) -> bool {
    if !kind(dom, id).is_some_and(Control::takes_typing) {
        return false;
    }
    let mut v = value(dom, id);
    if v.pop().is_none() {
        return false;
    }
    set_value(dom, id, &v);
    true
}

/// Enter in a single-line field submits its form, as if by its first
/// submit button.
pub fn implicit_submit(dom: &Dom, field: u32) -> Option<(u32, Option<u32>)> {
    let form = form_of(dom, field)?;
    let submitter = controls(dom, form)
        .into_iter()
        .find(|&c| kind(dom, c) == Some(Control::Submit) && !disabled(dom, c));
    Some((form, submitter))
}

/// The next field that takes typing after `from` (Tab).
pub fn next_field(dom: &Dom, from: Option<u32>) -> Option<u32> {
    let mut all = Vec::new();
    dom.descendants(0, &mut all);
    let fields: Vec<u32> = all
        .into_iter()
        .filter(|&n| kind(dom, n).is_some_and(Control::takes_typing) && !disabled(dom, n))
        .collect();
    let i = from.and_then(|f| fields.iter().position(|&x| x == f));
    match i {
        Some(i) => fields.get(i + 1).or(fields.first()).copied(),
        None => fields.first().copied(),
    }
}

// ---------------------------------------------------------------------------
// Submission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    /// Where to go (for GET, with the form data as the query).
    pub url: String,
    pub post: bool,
    pub body: Option<String>,
    pub content_type: Option<String>,
}

/// What submitting `form` (with `submitter`) sends, or None if it can't be
/// submitted (no valid action, or too large).
pub fn submission(dom: &Dom, form: u32, submitter: Option<u32>, page_url: &str) -> Option<Submission> {
    let attr = |id: Option<u32>, name: &str| -> Option<String> {
        id.and_then(|i| dom.element(i))
            .and_then(|e| e.attrs.get(name).cloned())
    };
    let action = attr(submitter, "formaction")
        .or_else(|| attr(Some(form), "action"))
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| page_url.to_string());
    let action = crate::net::resolve(page_url, &action);
    if !matches!(crate::net::scheme(&action).as_str(), "http" | "https" | "file") {
        return None;
    }
    let method = attr(submitter, "formmethod")
        .or_else(|| attr(Some(form), "method"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let post = method.trim() == "post";
    let enctype = attr(submitter, "formenctype")
        .or_else(|| attr(Some(form), "enctype"))
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut entries: Vec<(String, String)> = Vec::new();
    for c in controls(dom, form) {
        let Some(e) = dom.element(c) else { continue };
        let Some(name) = e.attrs.get("name").filter(|n| !n.is_empty()).cloned() else {
            continue;
        };
        if e.attrs.contains_key("disabled") {
            continue;
        }
        match kind(dom, c) {
            Some(Control::Checkbox | Control::Radio) => {
                if e.attrs.contains_key("checked") {
                    entries.push((name, e.attrs.get("value").cloned().unwrap_or_else(|| "on".into())));
                }
            }
            Some(Control::Submit) => {
                if Some(c) == submitter {
                    if e.attrs
                        .get("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("image"))
                    {
                        entries.push((format!("{name}.x"), "0".into()));
                        entries.push((format!("{name}.y"), "0".into()));
                    } else {
                        entries.push((name, e.attrs.get("value").cloned().unwrap_or_default()));
                    }
                }
            }
            Some(Control::Reset | Control::Button | Control::File) | None => {}
            Some(_) => entries.push((name, value(dom, c))),
        }
    }

    let (body, content_type) = if enctype == "multipart/form-data" {
        let boundary = format!("----brickle{:016x}", boundary_seed());
        let mut b = String::new();
        for (k, v) in &entries {
            let k = k.replace('"', "%22").replace(['\r', '\n'], " ");
            b.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
            ));
        }
        b.push_str(&format!("--{boundary}--\r\n"));
        (b, format!("multipart/form-data; boundary={boundary}"))
    } else if enctype == "text/plain" {
        let b: String = entries.iter().map(|(k, v)| format!("{k}={v}\r\n")).collect();
        (b, "text/plain".to_string())
    } else {
        let b = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(entries.iter())
            .finish();
        (b, "application/x-www-form-urlencoded".to_string())
    };
    if body.len() > MAX_BODY {
        return None;
    }
    if post {
        return Some(Submission {
            url: action,
            post: true,
            body: Some(body),
            content_type: Some(content_type),
        });
    }
    let mut u = url::Url::parse(&action).ok()?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(entries.iter())
        .finish();
    u.set_query(Some(&query));
    Some(Submission {
        url: u.into(),
        post: false,
        body: None,
        content_type: None,
    })
}

fn boundary_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ 0x9e37_79b9_7f4a_7c15
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Built-in look of form controls (user-agent origin, so pages' CSS can
/// restyle them).
pub const UA_CSS: &str = "
.-brickle-hidden { display: none }
.-brickle-text { width: 160px; padding: 2px 4px; background-color: white; white-space: pre; }
.-brickle-textarea { display: inline-block; width: 300px; min-height: 48px; padding: 2px 4px; background-color: white; white-space: pre; font-family: monospace; }
.-brickle-check { width: 13px; height: 13px; padding: 0; background-color: white; font-size: 11px; line-height: 13px; text-align: center; }
.-brickle-button { padding: 2px 8px; background-color: #e9e9ed; }
.-brickle-select { padding: 1px 6px; background-color: #e9e9ed; }
.-brickle-placeholder { color: #757575 }
.-brickle-focus { border-color: #0060df; border-width: 2px; }
";

fn add_class(n: &mut Node, class: &str) {
    if let NodeType::Element(e) = &mut n.node_type {
        let c = e.attrs.entry("class".into()).or_default();
        if !c.is_empty() {
            c.push(' ');
        }
        c.push_str(class);
    }
}

fn text_child(s: &str) -> Node {
    dom::text(s.to_string())
}

fn placeholder(s: &str) -> Node {
    let mut attrs = AttrMap::new();
    attrs.insert("class".into(), "-brickle-placeholder".into());
    dom::elem("span".into(), attrs, vec![text_child(s)])
}

/// Give form controls something to draw: their value or placeholder, a
/// button's label, the selected option; mark the focused field. Works on
/// a copy of the tree used only for rendering.
pub fn decorate(n: &mut Node, focus: Option<u32>) {
    let focused = focus.is_some_and(|f| f == n.uid && n.uid != 0);
    let Some(e) = n.element().cloned() else { return };
    let Some(kind) = control(&e) else {
        for c in &mut n.children {
            decorate(c, focus);
        }
        return;
    };
    let caret = if focused { "▏" } else { "" };
    let val = e.attrs.get("value").cloned().unwrap_or_default();
    let ph = e.attrs.get("placeholder").cloned().unwrap_or_default();
    match kind {
        Control::Hidden => add_class(n, "-brickle-hidden"),
        Control::Text { password } => {
            add_class(n, "-brickle-text");
            let shown = if password {
                "•".repeat(val.chars().count())
            } else {
                val.clone()
            };
            n.children = if shown.is_empty() && !ph.is_empty() && !focused {
                vec![placeholder(&ph)]
            } else {
                vec![text_child(&format!("{shown}{caret}\u{200b}"))]
            };
        }
        Control::TextArea => {
            add_class(n, "-brickle-textarea");
            let text = n.text_content();
            n.children = if text.is_empty() && !ph.is_empty() && !focused {
                vec![placeholder(&ph)]
            } else {
                vec![text_child(&format!("{text}{caret}\u{200b}"))]
            };
        }
        Control::Checkbox | Control::Radio => {
            add_class(n, "-brickle-check");
            let on = e.attrs.contains_key("checked");
            let mark = match (kind, on) {
                (Control::Checkbox, true) => "✓",
                (Control::Radio, true) => "●",
                _ => "\u{a0}",
            };
            n.children = vec![text_child(mark)];
        }
        Control::Select => {
            add_class(n, "-brickle-select");
            let opts: Vec<&Node> = {
                let mut v = Vec::new();
                n.walk(&mut v, &|c| c.tag() == Some("option"));
                v
            };
            let chosen = opts
                .iter()
                .find(|o| o.element().is_some_and(|e| e.attrs.contains_key("selected")))
                .or(opts.first())
                .map(|o| o.text_content().split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            n.children = vec![text_child(&format!("{chosen} ▾"))];
        }
        Control::Submit | Control::Reset | Control::Button | Control::File if e.tag == "input" => {
            add_class(n, "-brickle-button");
            let label = match kind {
                Control::File => "Browse… (not supported)".to_string(),
                // `value=""` is an empty label (often an icon button).
                _ if !val.is_empty() || e.attrs.contains_key("value") => val,
                Control::Submit => e.attrs.get("alt").cloned().unwrap_or_else(|| "Submit".into()),
                Control::Reset => "Reset".into(),
                _ => String::new(),
            };
            n.children = vec![text_child(&label)];
        }
        _ => {
            // <button>: keep its own content.
            add_class(n, "-brickle-button");
            for c in &mut n.children {
                decorate(c, focus);
            }
        }
    }
    if focused {
        add_class(n, "-brickle-focus");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dom(html: &str) -> Dom {
        Dom::from_tree(&crate::html::parse(html))
    }

    fn find(d: &Dom, id: &str) -> u32 {
        by_id(d, id).unwrap_or_else(|| panic!("no #{id}"))
    }

    #[test]
    fn button_labels() {
        let label = |html: &str| {
            let mut n = crate::html::parse(html);
            decorate(&mut n, None);
            let mut b = Vec::new();
            n.walk(&mut b, &|c| c.tag() == Some("input"));
            b[0].text_content()
        };
        assert_eq!(label("<input type=submit>"), "Submit");
        assert_eq!(label("<input type=submit value=Go>"), "Go");
        // DuckDuckGo's icon button: empty, not its `alt`.
        assert_eq!(label("<input type=submit value='' alt=Search>"), "");
        assert_eq!(label("<input type=reset>"), "Reset");
    }

    #[test]
    fn typing_and_get_submission() {
        let mut d = dom(
            "<form action=/search id=f><input id=q name=q value=old><input name=lang type=hidden value=en>\
             <input type=checkbox name=safe id=safe><input type=submit name=go value=Go id=go>\
             <input type=submit name=other value=Other></form>",
        );
        let (q, go, safe) = (find(&d, "q"), find(&d, "go"), find(&d, "safe"));
        assert_eq!(activate(&mut d, q), Activation::Focus(q));
        backspace(&mut d, q);
        backspace(&mut d, q);
        backspace(&mut d, q);
        type_text(&mut d, q, "cats & dogs");
        assert_eq!(value(&d, q), "cats & dogs");
        assert_eq!(activate(&mut d, safe), Activation::Changed(safe));
        let Activation::Submit { form, submitter } = activate(&mut d, go) else {
            panic!()
        };
        let s = submission(&d, form, Some(submitter), "https://example.com/page?x=1#top").unwrap();
        assert_eq!(
            s.url,
            "https://example.com/search?q=cats+%26+dogs&lang=en&safe=on&go=Go"
        );
        assert!(!s.post && s.body.is_none());
        // Enter submits with the first submit button.
        let (f, sub) = implicit_submit(&d, q).unwrap();
        assert_eq!((f, sub), (form, Some(go)));
    }

    #[test]
    fn post_multipart_and_controls() {
        let mut d = dom(
            "<form method=POST enctype=multipart/form-data action='https://other.example/up'>\
             <textarea name=msg id=msg></textarea>\
             <input type=radio name=size value=s id=s checked><input type=radio name=size value=l id=l>\
             <select name=color id=color><option>Red<option value=g>Green</select>\
             <input name=off disabled value=x><input type=password name=pw id=pw maxlength=4>\
             <label for=l id=lab>Large</label><button id=b>Send</button></form>",
        );
        let (msg, l, color, pw, lab, b) = (
            find(&d, "msg"),
            find(&d, "l"),
            find(&d, "color"),
            find(&d, "pw"),
            find(&d, "lab"),
            find(&d, "b"),
        );
        type_text(&mut d, msg, "line one\nline two");
        type_text(&mut d, pw, "secret");
        assert_eq!(value(&d, pw), "secr", "maxlength");
        // Clicking the label picks its radio button; the other one clears.
        assert_eq!(control_at(&d, lab), Some(l));
        activate(&mut d, l);
        assert_eq!(value(&d, color), "Red");
        activate(&mut d, color);
        assert_eq!(value(&d, color), "g");
        let Activation::Submit { form, submitter } = activate(&mut d, b) else {
            panic!()
        };
        let s = submission(&d, form, Some(submitter), "https://example.com/").unwrap();
        assert!(s.post);
        assert_eq!(s.url, "https://other.example/up");
        let body = s.body.unwrap();
        assert!(s
            .content_type
            .unwrap()
            .starts_with("multipart/form-data; boundary=----brickle"));
        assert!(
            body.contains("name=\"msg\"\r\n\r\nline one\nline two\r\n"),
            "{body}"
        );
        assert!(
            body.contains("name=\"size\"\r\n\r\nl\r\n") && !body.contains("\r\n\r\ns\r\n"),
            "{body}"
        );
        assert!(body.contains("name=\"pw\"\r\n\r\nsecr\r\n"));
        assert!(!body.contains("name=\"off\""), "disabled fields aren't sent");
    }

    #[test]
    fn nothing_odd_gets_submitted() {
        let mut d = dom("<form action='javascript:alert(1)'><input type=submit id=s></form><input id=lone>");
        let s = find(&d, "s");
        let Activation::Submit { form, submitter } = activate(&mut d, s) else {
            panic!()
        };
        assert_eq!(
            submission(&d, form, Some(submitter), "https://example.com/"),
            None
        );
        // A field outside any form focuses but Enter submits nothing.
        let lone = find(&d, "lone");
        assert_eq!(implicit_submit(&d, lone), None);
        // Single-line fields drop line breaks.
        type_text(&mut d, lone, "a\nb");
        assert_eq!(value(&d, lone), "ab");
    }

    #[test]
    fn controls_get_something_to_draw() {
        let mut d =
            dom("<input id=a placeholder='Search'><input type=password value=abc id=p><input type=submit>");
        let p = find(&d, "p");
        let _ = &mut d;
        let mut tree = d.to_tree();
        decorate(&mut tree, Some(p));
        let mut out = String::new();
        tree.dump(0, &mut out);
        assert!(
            out.contains("\"Search\"") && out.contains("-brickle-placeholder"),
            "{out}"
        );
        assert!(
            out.contains("\"•••▏") && !out.contains(" \"abc"),
            "password hidden, caret shown: {out}"
        );
        assert!(out.contains("\"Submit\""), "{out}");
        assert!(out.contains("-brickle-focus"), "{out}");
    }
}
