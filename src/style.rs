//! Selector matching, the cascade, inheritance and computed values.

use crate::css::{self, Color, Combinator, Declaration, SimpleSelector, Stylesheet, Unit, Value};
use crate::dom::{ElementData, Node, NodeType};
use std::collections::HashMap;
use std::rc::Rc;

/// Largest font size we render, in px.
pub const MAX_FONT_SIZE: f32 = 256.0;

pub const UA_CSS: &str = include_str!("ua.css");

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Display {
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Flex,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Len {
    Px(f32),
    Percent(f32),
    Auto,
}

impl Len {
    pub fn resolve(self, container: f32) -> Option<f32> {
        match self {
            Len::Px(v) => Some(v),
            Len::Percent(p) => Some(container * p / 100.0),
            Len::Auto => None,
        }
    }
    pub fn or_zero(self, container: f32) -> f32 {
        self.resolve(container).unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListStyle {
    Disc,
    Circle,
    Square,
    Decimal,
    None,
}

/// Fully computed style for one element. Edges are [top, right, bottom, left].
#[derive(Debug, Clone)]
pub struct ComputedStyle {
    pub display: Display,
    pub color: Color,
    pub background: Color,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub family: Family,
    pub line_height: f32,
    pub text_align: TextAlign,
    pub underline: bool,
    pub strike: bool,
    pub pre: bool,
    pub nowrap: bool,
    pub uppercase: bool,
    pub list_style: ListStyle,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border_width: [f32; 4],
    pub border_color: [Color; 4],
    pub width: Len,
    pub height: Len,
    pub min_height: Len,
    pub max_width: Len,
    pub visible: bool,
    pub flex_row: bool,
    pub gap: f32,
}

impl ComputedStyle {
    pub fn root(viewport: (f32, f32)) -> ComputedStyle {
        let _ = viewport;
        ComputedStyle {
            display: Display::Block,
            color: Color::BLACK,
            background: Color::TRANSPARENT,
            font_size: 16.0,
            bold: false,
            italic: false,
            family: Family::Serif,
            line_height: 16.0 * 1.2,
            text_align: TextAlign::Left,
            underline: false,
            strike: false,
            pre: false,
            nowrap: false,
            uppercase: false,
            list_style: ListStyle::Disc,
            margin: [Len::Px(0.0); 4],
            padding: [Len::Px(0.0); 4],
            border_width: [0.0; 4],
            border_color: [Color::BLACK; 4],
            width: Len::Auto,
            height: Len::Auto,
            min_height: Len::Auto,
            max_width: Len::Auto,
            visible: true,
            flex_row: true,
            gap: 0.0,
        }
    }

    /// A child's starting point: inherited properties copied, others reset.
    fn inherit(&self) -> ComputedStyle {
        let mut s = ComputedStyle::root((0.0, 0.0));
        s.color = self.color;
        s.font_size = self.font_size;
        s.bold = self.bold;
        s.italic = self.italic;
        s.family = self.family;
        s.line_height = self.line_height;
        s.text_align = self.text_align;
        s.underline = self.underline;
        s.strike = self.strike;
        s.pre = self.pre;
        s.nowrap = self.nowrap;
        s.uppercase = self.uppercase;
        s.list_style = self.list_style;
        s.visible = self.visible;
        s.display = Display::Inline;
        s
    }
}

pub struct StyledNode<'a> {
    pub node: &'a Node,
    pub style: Rc<ComputedStyle>,
    pub children: Vec<StyledNode<'a>>,
}

impl<'a> StyledNode<'a> {
    pub fn display(&self) -> Display {
        match self.node.node_type {
            NodeType::Text(_) => Display::Inline,
            NodeType::Element(_) => self.style.display,
        }
    }
}

struct MatchedRule<'s> {
    key: (bool, u8, css::Specificity, usize),
    decl: &'s Declaration,
}

pub struct StyleContext<'s> {
    /// (origin, sheet). Origin 0 = user agent, 1 = author.
    pub sheets: Vec<(u8, &'s Stylesheet)>,
    pub viewport: (f32, f32),
}

pub fn style_tree<'a>(root: &'a Node, ctx: &StyleContext) -> StyledNode<'a> {
    let base = ComputedStyle::root(ctx.viewport);
    let mut ancestors = Vec::new();
    style_node(root, &base, &mut ancestors, ctx, true)
}

fn style_node<'a>(
    node: &'a Node,
    parent: &ComputedStyle,
    ancestors: &mut Vec<&'a ElementData>,
    ctx: &StyleContext,
    is_root: bool,
) -> StyledNode<'a> {
    let style = match &node.node_type {
        NodeType::Text(_) => {
            let mut s = parent.inherit();
            s.display = Display::Inline;
            s
        }
        NodeType::Element(e) => compute(e, parent, ancestors, ctx, is_root),
    };
    let style = Rc::new(style);
    let mut children = Vec::new();
    if let NodeType::Element(e) = &node.node_type {
        ancestors.push(e);
        for c in &node.children {
            children.push(style_node(c, &style, ancestors, ctx, false));
        }
        ancestors.pop();
    }
    StyledNode {
        node,
        style,
        children,
    }
}

fn matches_simple(e: &ElementData, s: &SimpleSelector, is_root: bool) -> bool {
    if s.tag.as_ref().is_some_and(|t| *t != e.tag) {
        return false;
    }
    if s.id.as_ref().is_some_and(|id| Some(id.as_str()) != e.id()) {
        return false;
    }
    if s.root && !is_root {
        return false;
    }
    s.classes.iter().all(|c| e.has_class(c)) && s.attrs.iter().all(|a| e.attrs.contains_key(a))
}

/// Match a right-to-left selector against an element and its ancestor chain.
fn matches(e: &ElementData, ancestors: &[&ElementData], sel: &css::Selector) -> bool {
    fn go(parts: &[(SimpleSelector, Combinator)], e: &ElementData, anc: &[&ElementData]) -> bool {
        let (simple, comb) = &parts[0];
        if !matches_simple(e, simple, anc.is_empty() && e.tag == "html") {
            return false;
        }
        if parts.len() == 1 {
            return true;
        }
        match comb {
            Combinator::Child => anc.split_last().is_some_and(|(p, rest)| go(&parts[1..], p, rest)),
            // With only descendant combinators left, matching the nearest
            // qualifying ancestor is always optimal, so no backtracking is
            // needed. This keeps crafted selectors like `a a a a a b` on deep
            // trees from taking exponential time.
            _ if parts[1..].iter().all(|(_, c)| *c != Combinator::Child) => (0..anc.len())
                .rev()
                .find(|&i| matches_simple(anc[i], &parts[1].0, i == 0 && anc[i].tag == "html"))
                .is_some_and(|i| go(&parts[1..], anc[i], &anc[..i])),
            _ => (0..anc.len()).rev().any(|i| go(&parts[1..], anc[i], &anc[..i])),
        }
    }
    go(&sel.parts, e, ancestors)
}

fn compute(
    e: &ElementData,
    parent: &ComputedStyle,
    ancestors: &[&ElementData],
    ctx: &StyleContext,
    is_root: bool,
) -> ComputedStyle {
    let mut matched: Vec<MatchedRule> = Vec::new();
    let mut order = 0;
    for (origin, sheet) in &ctx.sheets {
        for rule in &sheet.rules {
            let best = rule
                .selectors
                .iter()
                .filter(|s| matches(e, ancestors, s))
                .map(|s| s.specificity())
                .max();
            if let Some(spec) = best {
                for d in &rule.declarations {
                    order += 1;
                    matched.push(MatchedRule {
                        key: (d.important, *origin, spec, order),
                        decl: d,
                    });
                }
            }
        }
    }
    let inline = e
        .attrs
        .get("style")
        .map(|s| css::parse_inline(s))
        .unwrap_or_default();
    for d in &inline {
        order += 1;
        matched.push(MatchedRule {
            key: (d.important, 2, (1, 0, 0), order),
            decl: d,
        });
    }
    // Presentational attributes (<font color>, bgcolor, width...) at low priority.
    let mut presentational = Vec::new();
    for (attr, prop) in [
        ("bgcolor", "background-color"),
        ("color", "color"),
        ("width", "width"),
        ("height", "height"),
    ] {
        if let Some(v) = e.attrs.get(attr) {
            if attr == "color" && e.tag != "font" {
                continue;
            }
            if (attr == "width" || attr == "height")
                && !matches!(e.tag.as_str(), "img" | "table" | "td" | "th" | "hr")
            {
                continue;
            }
            let v = if v.chars().all(|c| c.is_ascii_digit()) {
                format!("{v}px")
            } else {
                v.clone()
            };
            presentational.push(Declaration {
                name: prop.into(),
                values: css::parse_values(&v),
                important: false,
            });
        }
    }
    let bordered = |t: &ElementData| t.attrs.get("border").is_some_and(|b| b != "0");
    if e.tag == "table" && bordered(e) {
        presentational.push(Declaration {
            name: "border".into(),
            values: css::parse_values("1px outset gray"),
            important: false,
        });
    }
    if matches!(e.tag.as_str(), "td" | "th")
        && ancestors
            .iter()
            .rev()
            .find(|a| a.tag == "table")
            .is_some_and(|t| bordered(t))
    {
        presentational.push(Declaration {
            name: "border".into(),
            values: css::parse_values("1px inset gray"),
            important: false,
        });
    }
    if e.attrs.contains_key("hidden") {
        presentational.push(Declaration {
            name: "display".into(),
            values: vec![Value::Keyword("none".into())],
            important: false,
        });
    }
    for d in &presentational {
        matched.push(MatchedRule {
            key: (false, 1, (0, 0, 0), 0),
            decl: d,
        });
    }
    matched.sort_by_key(|m| m.key);

    let mut props: HashMap<String, Vec<Value>> = HashMap::new();
    for m in &matched {
        expand(m.decl, &mut props);
    }

    let mut s = parent.inherit();
    s.display = Display::Inline;
    let ctxv = Ctx {
        parent,
        viewport: ctx.viewport,
    };

    // font-size first: em units in other properties depend on it.
    if let Some(v) = props.get("font-size").and_then(|v| v.first()) {
        s.font_size = match v {
            Value::Keyword(k) => match k.as_str() {
                "xx-small" => 9.0,
                "x-small" => 10.0,
                "small" => 13.0,
                "medium" => 16.0,
                "large" => 18.0,
                "x-large" => 24.0,
                "xx-large" => 32.0,
                "smaller" => parent.font_size / 1.2,
                "larger" => parent.font_size * 1.2,
                _ => parent.font_size,
            },
            Value::Percent(p) => parent.font_size * p / 100.0,
            v => ctxv.length(v, parent.font_size).unwrap_or(parent.font_size),
        };
    }
    // Clamp absurd sizes: huge glyphs would eat memory in the glyph cache.
    s.font_size = s.font_size.clamp(0.0, MAX_FONT_SIZE);
    // Keep line-height proportional when the font size changes, unless set.
    if (s.font_size - parent.font_size).abs() > f32::EPSILON {
        s.line_height = parent.line_height / parent.font_size * s.font_size;
    }
    let fs = s.font_size;

    for (name, vals) in &props {
        let first = vals.first();
        let kw = match first {
            Some(Value::Keyword(k)) => k.as_str(),
            _ => "",
        };
        if kw == "inherit" {
            continue;
        }
        match name.as_str() {
            "display" => {
                s.display = match kw {
                    "block" | "table" | "table-row" | "table-row-group" | "table-header-group"
                    | "table-footer-group" | "grid" | "flow-root" | "table-caption" => Display::Block,
                    "inline" => Display::Inline,
                    "inline-block" | "table-cell" | "inline-table" | "inline-flex" | "inline-grid" => {
                        Display::InlineBlock
                    }
                    "list-item" => Display::ListItem,
                    "flex" => Display::Flex,
                    "none" => Display::None,
                    _ => s.display,
                }
            }
            "color" => {
                if let Some(Value::Color(c)) = first {
                    s.color = *c;
                }
            }
            "background-color" => {
                if let Some(Value::Color(c)) = first {
                    s.background = *c;
                }
            }
            "font-weight" => {
                s.bold = match first {
                    Some(Value::Number(n)) => *n >= 600.0,
                    _ => match kw {
                        "bold" | "bolder" => true,
                        "normal" | "lighter" => false,
                        _ => s.bold,
                    },
                }
            }
            "font-style" => s.italic = kw == "italic" || kw == "oblique",
            "font-family" => {
                for v in vals {
                    let name = match v {
                        Value::Keyword(k) => k.clone(),
                        Value::Str(st) => st.to_ascii_lowercase(),
                        _ => continue,
                    };
                    let fam = if name.contains("mono")
                        || name.contains("courier")
                        || name.contains("consol")
                        || name == "menlo"
                    {
                        Some(Family::Mono)
                    } else if name == "serif"
                        || name.contains("times")
                        || name.contains("georgia")
                        || name.contains("garamond")
                    {
                        Some(Family::Serif)
                    } else if name.contains("sans")
                        || name.contains("arial")
                        || name.contains("helvetica")
                        || name.contains("system-ui")
                        || name.contains("inter")
                        || name.contains("verdana")
                        || name.contains("roboto")
                        || name.contains("segoe")
                        || name.contains("apple")
                    {
                        Some(Family::Sans)
                    } else {
                        None
                    };
                    if let Some(f) = fam {
                        s.family = f;
                        break;
                    }
                }
            }
            "line-height" => {
                s.line_height = match first {
                    Some(Value::Number(n)) => n * fs,
                    Some(Value::Percent(p)) => p / 100.0 * fs,
                    Some(Value::Keyword(k)) if k == "normal" => fs * 1.2,
                    Some(v) => ctxv.length(v, fs).unwrap_or(s.line_height),
                    None => s.line_height,
                }
            }
            "text-align" => {
                s.text_align = match kw {
                    "center" | "-webkit-center" => TextAlign::Center,
                    "right" | "end" => TextAlign::Right,
                    "left" | "start" | "justify" => TextAlign::Left,
                    _ => s.text_align,
                }
            }
            "text-decoration" | "text-decoration-line" => {
                let has = |w: &str| vals.iter().any(|v| matches!(v, Value::Keyword(k) if k == w));
                if has("none") {
                    s.underline = false;
                    s.strike = false;
                }
                if has("underline") {
                    s.underline = true;
                }
                if has("line-through") {
                    s.strike = true;
                }
            }
            "text-transform" => s.uppercase = kw == "uppercase",
            "white-space" => {
                s.pre = matches!(kw, "pre" | "pre-wrap" | "pre-line" | "break-spaces");
                s.nowrap = matches!(kw, "nowrap" | "pre");
            }
            "list-style-type" => {
                s.list_style = match kw {
                    "none" => ListStyle::None,
                    "circle" => ListStyle::Circle,
                    "square" => ListStyle::Square,
                    "decimal"
                    | "decimal-leading-zero"
                    | "lower-alpha"
                    | "upper-alpha"
                    | "lower-roman"
                    | "upper-roman"
                    | "lower-latin"
                    | "upper-latin" => ListStyle::Decimal,
                    _ => ListStyle::Disc,
                }
            }
            "visibility" => s.visible = kw != "hidden" && kw != "collapse",
            "flex-direction" => s.flex_row = !kw.starts_with("column"),
            "gap" | "column-gap" => {
                if let Some(v) = first {
                    s.gap = ctxv.length(v, fs).unwrap_or(0.0)
                }
            }
            "width" => s.width = ctxv.len(first, fs),
            "height" => s.height = ctxv.len(first, fs),
            "min-height" => s.min_height = ctxv.len(first, fs),
            "max-width" => s.max_width = ctxv.len(first, fs),
            _ => {
                for (i, side) in ["top", "right", "bottom", "left"].iter().enumerate() {
                    if name == &format!("margin-{side}") {
                        s.margin[i] = ctxv.len(first, fs);
                    } else if name == &format!("padding-{side}") {
                        s.padding[i] = match ctxv.len(first, fs) {
                            Len::Auto => Len::Px(0.0),
                            l => l,
                        };
                    } else if name == &format!("border-{side}-width") {
                        s.border_width[i] = match first {
                            Some(Value::Keyword(k)) => match k.as_str() {
                                "thin" => 1.0,
                                "medium" => 3.0,
                                "thick" => 5.0,
                                _ => 0.0,
                            },
                            Some(v) => ctxv.length(v, fs).unwrap_or(0.0),
                            None => 0.0,
                        };
                    } else if name == &format!("border-{side}-color") {
                        if let Some(Value::Color(c)) = first {
                            s.border_color[i] = *c;
                        }
                    }
                }
            }
        }
    }
    // Borders only show with a visible style; `currentColor` is the default.
    for (i, side) in ["top", "right", "bottom", "left"].iter().enumerate() {
        let style = props.get(&format!("border-{side}-style"));
        let visible = style
            .and_then(|v| v.first())
            .is_some_and(|v| matches!(v, Value::Keyword(k) if k != "none" && k != "hidden"));
        if !visible {
            s.border_width[i] = 0.0;
        }
        if !props.contains_key(&format!("border-{side}-color")) {
            s.border_color[i] = s.color;
        }
    }
    s.line_height = s.line_height.clamp(0.0, MAX_FONT_SIZE * 4.0);
    if is_root && s.display == Display::Inline {
        s.display = Display::Block;
    }
    s
}

struct Ctx<'p> {
    parent: &'p ComputedStyle,
    viewport: (f32, f32),
}

impl Ctx<'_> {
    fn length(&self, v: &Value, font_size: f32) -> Option<f32> {
        let _ = self.parent;
        match v {
            Value::Length(n, u) => Some(match u {
                Unit::Px => *n,
                Unit::Em => n * font_size,
                Unit::Rem => n * 16.0,
                Unit::Pt => n * 4.0 / 3.0,
                Unit::Vw => n * self.viewport.0 / 100.0,
                Unit::Vh => n * self.viewport.1 / 100.0,
            }),
            Value::Number(n) if *n == 0.0 => Some(0.0),
            _ => None,
        }
    }

    fn len(&self, v: Option<&Value>, font_size: f32) -> Len {
        match v {
            Some(Value::Percent(p)) => Len::Percent(*p),
            Some(v) => self.length(v, font_size).map(Len::Px).unwrap_or(Len::Auto),
            None => Len::Auto,
        }
    }
}

/// Expand shorthand properties into longhands.
fn expand(d: &Declaration, props: &mut HashMap<String, Vec<Value>>) {
    let sides = ["top", "right", "bottom", "left"];
    let four = |vals: &[Value]| -> [Value; 4] {
        let v: Vec<Value> = vals.iter().filter(|v| **v != Value::Comma).cloned().collect();
        match v.len() {
            0 => [
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(0.0),
            ],
            1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
            2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
            3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
            _ => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
        }
    };
    let props = std::cell::RefCell::new(props);
    let set = |k: String, v: Vec<Value>| {
        props.borrow_mut().insert(k, v);
    };
    let name = d.name.as_str();
    match name {
        "margin" | "padding" => {
            for (i, v) in four(&d.values).into_iter().enumerate() {
                set(format!("{name}-{}", sides[i]), vec![v]);
            }
        }
        "border-width" | "border-color" | "border-style" => {
            let prop = &name[7..];
            for (i, v) in four(&d.values).into_iter().enumerate() {
                set(format!("border-{}-{prop}", sides[i]), vec![v]);
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let targets: Vec<&str> = if name == "border" {
                sides.to_vec()
            } else {
                vec![&name[7..]]
            };
            let mut width = Value::Keyword("medium".into());
            let mut style = Value::Keyword("none".into());
            let mut color: Option<Value> = None;
            for v in &d.values {
                match v {
                    Value::Color(_) => color = Some(v.clone()),
                    Value::Length(..) | Value::Number(_) => width = v.clone(),
                    Value::Keyword(k) if matches!(k.as_str(), "thin" | "medium" | "thick") => {
                        width = v.clone()
                    }
                    Value::Keyword(_) => style = v.clone(),
                    _ => {}
                }
            }
            for side in targets {
                set(format!("border-{side}-width"), vec![width.clone()]);
                set(format!("border-{side}-style"), vec![style.clone()]);
                match &color {
                    Some(c) => set(format!("border-{side}-color"), vec![c.clone()]),
                    None => {
                        props.borrow_mut().remove(&format!("border-{side}-color"));
                    }
                }
            }
        }
        "background" => {
            let color = d
                .values
                .iter()
                .rev()
                .find(|v| matches!(v, Value::Color(_)))
                .cloned();
            set(
                "background-color".into(),
                vec![color.unwrap_or(Value::Color(Color::TRANSPARENT))],
            );
        }
        "font" => {
            // font: [style] [weight] size[/line-height] family
            let mut seen_size = false;
            let mut family = Vec::new();
            for v in &d.values {
                if seen_size {
                    match v {
                        Value::Number(_) | Value::Length(..) | Value::Percent(_) if family.is_empty() => {
                            set("line-height".into(), vec![v.clone()])
                        }
                        _ => family.push(v.clone()),
                    }
                    continue;
                }
                match v {
                    Value::Keyword(k) if k == "bold" || k == "bolder" => {
                        set("font-weight".into(), vec![v.clone()])
                    }
                    Value::Keyword(k) if k == "italic" || k == "oblique" => {
                        set("font-style".into(), vec![v.clone()])
                    }
                    Value::Number(n) if *n >= 100.0 => set("font-weight".into(), vec![v.clone()]),
                    Value::Length(..) | Value::Percent(_) => {
                        set("font-size".into(), vec![v.clone()]);
                        seen_size = true;
                    }
                    Value::Keyword(k) if k.ends_with("small") || k.ends_with("large") || k == "medium" => {
                        set("font-size".into(), vec![v.clone()]);
                        seen_size = true;
                    }
                    _ => {}
                }
            }
            if !family.is_empty() {
                set("font-family".into(), family);
            }
        }
        "list-style" => {
            if let Some(v) = d.values.iter().find(|v| matches!(v, Value::Keyword(_))) {
                set("list-style-type".into(), vec![v.clone()]);
            }
        }
        "flex-flow" => {
            if let Some(v) = d.values.first() {
                set("flex-direction".into(), vec![v.clone()]);
            }
        }
        _ => set(d.name.clone(), d.values.clone()),
    }
}
