//! CSS parser: stylesheets, selectors (type/class/id/universal, descendant
//! and child combinators) and declarations with a handful of value types.

#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
    /// The `@media` blocks it's inside: it applies while all of them match.
    pub media: Vec<std::sync::Arc<Media>>,
}

impl Rule {
    /// Does the rule apply in a viewport of this size?
    pub fn applies(&self, viewport: (f32, f32)) -> bool {
        self.media.iter().all(|m| m.matches(viewport))
    }
}

/// An `@media` query list: it matches while any of its queries does.
/// Brickle is a screen with a mouse, light colours, no motion preference
/// and 1 pixel per CSS pixel; features it doesn't know never match.
#[derive(Debug, Clone, PartialEq)]
pub struct Media(Vec<Query>);

#[derive(Debug, Clone, PartialEq)]
struct Query {
    negated: bool,
    conds: Vec<Cond>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cond {
    Always,
    Never,
    Width(Cmp, f32),
    Height(Cmp, f32),
    /// Width / height.
    Aspect(Cmp, f32),
    Landscape(bool),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    AtLeast,
    AtMost,
    Above,
    Below,
    Equal,
}

impl Cmp {
    fn test(self, v: f32, limit: f32) -> bool {
        match self {
            Cmp::AtLeast => v >= limit,
            Cmp::AtMost => v <= limit,
            Cmp::Above => v > limit,
            Cmp::Below => v < limit,
            Cmp::Equal => v == limit,
        }
    }

    /// `limit OP feature` read as `feature OP' limit`.
    fn flip(self) -> Cmp {
        match self {
            Cmp::AtLeast => Cmp::AtMost,
            Cmp::AtMost => Cmp::AtLeast,
            Cmp::Above => Cmp::Below,
            Cmp::Below => Cmp::Above,
            Cmp::Equal => Cmp::Equal,
        }
    }
}

/// Most queries in one list, and conditions in one query.
const MAX_MEDIA_PARTS: usize = 32;

impl Media {
    /// Parse the part after `@media`.
    pub fn parse(text: &str) -> Media {
        // `(a)and (b)` is common in minified CSS.
        let text = text.to_ascii_lowercase().replace('(', " (").replace(')', ") ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let queries = split_top(&text, |rest| {
            if rest.starts_with(',') {
                Some(1)
            } else if rest.starts_with(" or ") {
                Some(4)
            } else {
                None
            }
        });
        Media(
            queries
                .into_iter()
                .take(MAX_MEDIA_PARTS)
                .map(Query::parse)
                .collect(),
        )
    }

    pub fn matches(&self, (w, h): (f32, f32)) -> bool {
        self.0.iter().any(|q| {
            let all = q.conds.iter().all(|c| match *c {
                Cond::Always => true,
                Cond::Never => false,
                Cond::Width(op, v) => op.test(w, v),
                Cond::Height(op, v) => op.test(h, v),
                Cond::Aspect(op, v) => h > 0.0 && op.test(w / h, v),
                Cond::Landscape(l) => (w >= h) == l,
            });
            all != q.negated
        })
    }

    /// Never matches, whatever the viewport (`print`, `speech`...).
    fn never(&self) -> bool {
        self.0
            .iter()
            .all(|q| !q.negated && q.conds.contains(&Cond::Never))
    }
}

/// Split at depth-0 separators; `sep` says how long the one at the start
/// of `rest` is.
fn split_top(text: &str, sep: impl Fn(&str) -> Option<usize>) -> Vec<&str> {
    let (mut out, mut depth, mut start, mut i) = (Vec::new(), 0i32, 0, 0);
    while i < text.len() {
        let c = text.as_bytes()[i];
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
        } else if depth == 0 && text.is_char_boundary(i) {
            if let Some(n) = sep(&text[i..]) {
                out.push(text[start..i].trim());
                i += n;
                start = i;
                continue;
            }
        }
        i += 1;
    }
    out.push(text[start..].trim());
    out
}

impl Query {
    fn parse(text: &str) -> Query {
        let mut text = text.trim();
        let mut negated = false;
        if let Some(r) = text.strip_prefix("only ") {
            text = r.trim_start();
        } else if let Some(r) = text.strip_prefix("not ") {
            negated = true;
            text = r.trim_start();
        }
        let parts = split_top(text, |rest| rest.starts_with(" and ").then_some(5));
        let conds = parts
            .into_iter()
            .take(MAX_MEDIA_PARTS)
            .flat_map(|p| {
                let p = p.trim();
                match p.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
                    Some(inner) => feature(inner.trim()),
                    None => match p {
                        "screen" | "all" | "" => vec![Cond::Always],
                        _ => vec![Cond::Never],
                    },
                }
            })
            .collect();
        Query { negated, conds }
    }
}

/// One `(feature: value)` or `(feature < value)` test (two conditions for
/// `(400px <= width < 900px)`).
fn feature(text: &str) -> Vec<Cond> {
    if let Some((name, value)) = text.split_once(':') {
        let (name, value) = (name.trim(), value.trim());
        let (op, name) = match (name.strip_prefix("min-"), name.strip_prefix("max-")) {
            (Some(n), _) => (Cmp::AtLeast, n),
            (_, Some(n)) => (Cmp::AtMost, n),
            _ => (Cmp::Equal, name),
        };
        return vec![named(name, op, value)];
    }
    // Range syntax: `width >= 600px`, `400px <= width < 900px`.
    let ops: [(&str, Cmp); 5] = [
        (">=", Cmp::AtLeast),
        ("<=", Cmp::AtMost),
        (">", Cmp::Above),
        ("<", Cmp::Below),
        ("=", Cmp::Equal),
    ];
    let mut pieces = Vec::new();
    let mut rest = text;
    'outer: loop {
        for (i, _) in rest.char_indices() {
            for (sym, op) in ops {
                if rest[i..].starts_with(sym) {
                    pieces.push((rest[..i].trim(), Some(op)));
                    rest = &rest[i + sym.len()..];
                    continue 'outer;
                }
            }
        }
        pieces.push((rest.trim(), None));
        break;
    }
    match pieces.as_slice() {
        [(name, None)] => vec![named(name, Cmp::Equal, "")],
        [(a, Some(op)), (b, None)] => {
            if is_range_feature(a) {
                vec![named(a, *op, b)]
            } else {
                vec![named(b, op.flip(), a)]
            }
        }
        [(lo, Some(op1)), (name, Some(op2)), (hi, None)] => {
            vec![named(name, op1.flip(), lo), named(name, *op2, hi)]
        }
        _ => vec![Cond::Never],
    }
}

fn is_range_feature(name: &str) -> bool {
    matches!(name, "width" | "height" | "aspect-ratio" | "resolution")
}

/// A named feature compared with a value (`""` for a bare `(hover)`).
fn named(name: &str, op: Cmp, value: &str) -> Cond {
    let px = || -> Option<f32> {
        let v = value.trim();
        let num_end = v
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
            .unwrap_or(v.len());
        let n: f32 = v[..num_end].parse().ok()?;
        let n = match v[num_end..].trim() {
            "px" | "" => n,
            "em" | "rem" => n * 16.0,
            "pt" => n * 4.0 / 3.0,
            _ => return None,
        };
        n.is_finite().then_some(n)
    };
    let is = |yes: &[&str]| {
        if yes.contains(&value) {
            Cond::Always
        } else {
            Cond::Never
        }
    };
    match name {
        "width" | "device-width" => px().map_or(Cond::Never, |v| Cond::Width(op, v)),
        "height" | "device-height" => px().map_or(Cond::Never, |v| Cond::Height(op, v)),
        "aspect-ratio" | "device-aspect-ratio" => {
            let ratio = match value.split_once('/') {
                Some((a, b)) => a
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .zip(b.trim().parse::<f32>().ok())
                    .map(|(a, b)| a / b),
                None => value.trim().parse().ok(),
            };
            ratio
                .filter(|r| r.is_finite())
                .map_or(Cond::Never, |r| Cond::Aspect(op, r))
        }
        "orientation" => match value {
            "landscape" => Cond::Landscape(true),
            "portrait" => Cond::Landscape(false),
            _ => Cond::Never,
        },
        // One pixel per CSS pixel.
        "resolution" | "-webkit-device-pixel-ratio" | "device-pixel-ratio" => {
            let v = value.trim();
            let n = v
                .strip_suffix("dppx")
                .or_else(|| v.strip_suffix('x'))
                .map(|n| n.trim().parse::<f32>().ok())
                .unwrap_or_else(|| {
                    v.strip_suffix("dpi")
                        .and_then(|n| n.trim().parse::<f32>().ok())
                        .map(|d| d / 96.0)
                        .or_else(|| v.parse().ok())
                });
            match n {
                Some(n) if op.test(1.0, n) => Cond::Always,
                _ => Cond::Never,
            }
        }
        "-webkit-min-device-pixel-ratio" => named("resolution", Cmp::AtLeast, value),
        "-webkit-max-device-pixel-ratio" => named("resolution", Cmp::AtMost, value),
        "hover" | "any-hover" => is(&["", "hover"]),
        "pointer" | "any-pointer" => is(&["", "fine"]),
        "color" => {
            if value.is_empty() || op != Cmp::Equal {
                Cond::Always
            } else {
                Cond::Never
            }
        }
        "prefers-color-scheme" => is(&["light"]),
        "prefers-reduced-motion"
        | "prefers-reduced-transparency"
        | "prefers-reduced-data"
        | "prefers-contrast" => is(&["no-preference"]),
        "forced-colors" | "inverted-colors" => is(&["none"]),
        "display-mode" => is(&["browser"]),
        "update" => is(&["", "fast"]),
        "scan" => is(&["progressive"]),
        "dynamic-range" | "video-dynamic-range" => is(&["standard"]),
        _ => Cond::Never,
    }
}

/// A complex selector, stored right-to-left: `parts[0]` is the subject.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub parts: Vec<(SimpleSelector, Combinator)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Combinator {
    /// Relationship to the *next* (more leftward) compound selector.
    Descendant,
    Child,
    /// `+`: the element right before.
    Next,
    /// `~`: any element before, with the same parent.
    Subsequent,
    None,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimpleSelector {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSelector>,
    pub root: bool,
    pub pseudo: Vec<Pseudo>,
}

/// `[name]`, or `[name op "value"]` (with `i`: ignoring ASCII case).
#[derive(Debug, Clone, PartialEq)]
pub struct AttrSelector {
    pub name: String,
    pub op: AttrOp,
    pub value: String,
    pub ignore_case: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AttrOp {
    Exists,
    /// `=`
    Equals,
    /// `~=`: one of its space-separated words.
    Word,
    /// `|=`: equal, or starting with it and a hyphen.
    Lang,
    /// `^=`, `$=`, `*=`
    Prefix,
    Suffix,
    Contains,
}

/// Pseudo-classes Brickle evaluates.
#[derive(Debug, Clone, PartialEq)]
pub enum Pseudo {
    /// `:nth-child(an+b)` and friends: the position (from 1) among the
    /// element's siblings (of its type, from the end) is `a*n + b` for some
    /// n >= 0. `:first-child` is (0, 1).
    Nth {
        a: i32,
        b: i32,
        of_type: bool,
        from_end: bool,
    },
    /// `:only-child`, `:only-of-type`
    Only { of_type: bool },
    /// `:empty`: no element children and no text.
    Empty,
    /// None of these selectors match.
    Not(Vec<Selector>),
    /// `:is()` (`where_` false) or `:where()` (true, no specificity).
    Is { list: Vec<Selector>, where_: bool },
    /// Some element relative to this one matches one of these. Each is a
    /// relative selector: its leftmost part's combinator says how that part
    /// relates to this element.
    Has(Vec<Selector>),
    /// Interaction states (`:hover`, `:focus`, `:checked`, ...): a page
    /// that's just been drawn has none of them.
    Never,
}

pub type Specificity = (usize, usize, usize);

impl Selector {
    pub fn specificity(&self) -> Specificity {
        let mut s = (0, 0, 0);
        for (p, _) in &self.parts {
            let add = |s: &mut Specificity, t: Specificity| {
                s.0 += t.0;
                s.1 += t.1;
                s.2 += t.2;
            };
            s.0 += p.id.iter().count();
            s.1 += p.classes.len() + p.attrs.len() + p.root as usize;
            s.2 += p.tag.iter().count();
            for ps in &p.pseudo {
                match ps {
                    // The most specific selector in the list counts.
                    Pseudo::Not(list) | Pseudo::Is { list, where_: false } | Pseudo::Has(list) => add(
                        &mut s,
                        list.iter().map(Selector::specificity).max().unwrap_or_default(),
                    ),
                    Pseudo::Is { where_: true, .. } => {}
                    _ => s.1 += 1,
                }
            }
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub values: Vec<Value>,
    pub important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Keyword(String),
    Length(f32, Unit),
    Percent(f32),
    Number(f32),
    Color(Color),
    Str(String),
    /// `url(...)`, with its case kept.
    Url(String),
    Comma,
}

impl Value {
    /// The value written back as CSS, for substituting `var()`.
    pub fn to_css(&self) -> String {
        match self {
            Value::Keyword(k) => k.clone(),
            Value::Length(n, u) => format!(
                "{n}{}",
                match u {
                    Unit::Px => "px",
                    Unit::Em => "em",
                    Unit::Rem => "rem",
                    Unit::Pt => "pt",
                    Unit::Vw => "vw",
                    Unit::Vh => "vh",
                }
            ),
            Value::Percent(n) => format!("{n}%"),
            Value::Number(n) => format!("{n}"),
            Value::Color(c) => format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, c.a as f32 / 255.0),
            Value::Str(s) => format!("\"{}\"", s.replace('"', "")),
            Value::Url(u) => format!("url(\"{}\")", u.replace('"', "%22")),
            Value::Comma => ",".into(),
        }
    }
}

/// Make the sheet's `url()`s absolute, relative to `base` (where the sheet
/// came from), so they mean the same once the sheet is used elsewhere.
pub fn resolve_urls(sheet: &mut Stylesheet, base: &str) {
    for rule in &mut sheet.rules {
        for d in &mut rule.declarations {
            for v in &mut d.values {
                if let Value::Url(u) = v {
                    if !u.starts_with("data:") {
                        *u = crate::net::resolve(base, u);
                    }
                }
            }
        }
    }
}

/// The `url()`s in a stylesheet, as written (`data:` ones left out).
pub fn urls(sheet: &Stylesheet) -> impl Iterator<Item = &str> {
    sheet
        .rules
        .iter()
        .flat_map(|r| &r.declarations)
        .flat_map(|d| &d.values)
        .filter_map(|v| match v {
            Value::Url(u) if !u.starts_with("data:") => Some(u.as_str()),
            _ => None,
        })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Px,
    Em,
    Rem,
    Pt,
    Vw,
    Vh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
}

/// Deepest `@media`/`@supports` nesting we follow.
const MAX_NESTING: usize = 8;
/// Longest selector (number of compound parts) we accept.
const MAX_SELECTOR_PARTS: usize = 32;
/// Deepest nesting of `:not()`, `:is()`, `:where()` and `:has()`.
const MAX_SELECTOR_NESTING: usize = 8;
/// Most selectors in one list (a rule's, or a `:is()`'s).
const MAX_SELECTOR_LIST: usize = 256;
/// Most rules kept from one stylesheet.
pub const MAX_RULES: usize = 5_000;

pub fn parse(source: &str) -> Stylesheet {
    let src = strip_comments(source);
    let mut p = Parser {
        chars: src.chars().collect(),
        pos: 0,
    };
    Stylesheet {
        rules: p.rules(0, &[]),
    }
}

/// Parse the contents of a `style="..."` attribute.
pub fn parse_inline(source: &str) -> Vec<Declaration> {
    let src = strip_comments(source);
    let mut p = Parser {
        chars: src.chars().collect(),
        pos: 0,
    };
    p.declarations()
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = match rest[i + 2..].find("*/") {
            Some(j) => &rest[i + 2 + j + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }
    /// Read up to (not including) one of `stops` at nesting depth 0.
    fn until(&mut self, stops: &[char]) -> String {
        let start = self.pos;
        let mut depth = 0i32;
        let mut quote: Option<char> = None;
        while let Some(c) = self.peek() {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                } else if c == '\\' {
                    self.pos += 1;
                }
            } else if c == '"' || c == '\'' {
                quote = Some(c);
            } else if c == '(' || c == '[' {
                depth += 1;
            } else if (c == ')' || c == ']') && depth > 0 {
                depth -= 1;
            } else if depth == 0 && stops.contains(&c) {
                break;
            }
            self.pos += 1;
        }
        // An escape at the very end can step past the input (found by fuzzing).
        self.pos = self.pos.min(self.chars.len());
        self.chars[start..self.pos].iter().collect()
    }
    /// Skip a `{ ... }` block including nested blocks.
    fn skip_block(&mut self) {
        let mut depth = 0;
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth -= 1;
                if depth <= 0 {
                    return;
                }
            }
        }
    }

    fn rules(&mut self, depth: usize, media: &[std::sync::Arc<Media>]) -> Vec<Rule> {
        let mut rules = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some('}') if depth > 0 => {
                    self.pos += 1;
                    break;
                }
                Some('@') => {
                    let prelude = self.until(&['{', ';']);
                    if self.peek() == Some(';') {
                        self.pos += 1;
                        continue;
                    }
                    let lower = prelude.to_ascii_lowercase();
                    // `@media` rules keep their query, checked against the
                    // viewport when styling (see `Rule::applies`).
                    let inner = if let Some(q) = lower.strip_prefix("@media") {
                        let m = Media::parse(q);
                        (!m.never()).then(|| {
                            let mut v = media.to_vec();
                            v.push(std::sync::Arc::new(m));
                            v
                        })
                    } else if lower.starts_with("@supports") || lower.starts_with("@layer") {
                        Some(media.to_vec())
                    } else {
                        None
                    };
                    match inner {
                        Some(inner) if depth < MAX_NESTING => {
                            self.pos += 1;
                            rules.extend(self.rules(depth + 1, &inner));
                        }
                        _ => self.skip_block(),
                    }
                }
                Some(_) => {
                    let prelude = self.until(&['{', '}']);
                    if self.peek() != Some('{') {
                        self.pos += 1;
                        continue;
                    }
                    self.pos += 1;
                    let declarations = self.declarations();
                    if self.peek() == Some('}') {
                        self.pos += 1;
                    }
                    let selectors = parse_selector_list(&prelude);
                    if !selectors.is_empty() && !declarations.is_empty() && rules.len() < MAX_RULES {
                        rules.push(Rule {
                            selectors,
                            declarations,
                            media: media.to_vec(),
                        });
                    }
                }
            }
        }
        rules
    }

    fn declarations(&mut self) -> Vec<Declaration> {
        let mut decls = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None | Some('}') => break,
                Some(';') => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            let decl = self.until(&[';', '}']);
            if let Some((name, value)) = decl.split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                let mut value = value.trim();
                let mut important = false;
                if let Some(i) = value.to_ascii_lowercase().find("!important") {
                    important = true;
                    value = value[..i].trim();
                }
                // `background: center/100% url(x)`: the size after the
                // slash becomes its own declaration (values drop slashes).
                let size = (name == "background").then(|| background_size(value)).flatten();
                if !name.is_empty() {
                    decls.push(Declaration {
                        name,
                        values: parse_values(size.as_ref().map_or(value, |(rest, _)| rest)),
                        important,
                    });
                }
                if let Some((_, size)) = size {
                    decls.push(Declaration {
                        name: "background-size".into(),
                        values: parse_values(&size),
                        important,
                    });
                }
            }
        }
        decls
    }
}

/// The `/size` in a `background` shorthand's first layer: the value without
/// it, and the size (one or two of lengths, percentages, `auto`, `cover`,
/// `contain`).
fn background_size(value: &str) -> Option<(String, String)> {
    let (mut depth, mut quote) = (0i32, None);
    for (i, c) in value.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            (_, Some(_)) => {}
            ('(', _) => depth += 1,
            (')', _) => depth -= 1,
            (',', _) if depth == 0 => return None,
            ('/', _) if depth == 0 => {
                let after = &value[i + 1..];
                let is_size = |t: &str| {
                    matches!(t, "auto" | "cover" | "contain")
                        || t.starts_with(|c: char| c.is_ascii_digit() || c == '.')
                };
                let toks: Vec<&str> = after
                    .split_whitespace()
                    .take(2)
                    .take_while(|t| is_size(t))
                    .collect();
                let first = *toks.first()?;
                let mut end = after.find(first)? + first.len();
                let mut size = first.to_string();
                if let Some(second) = toks.get(1).filter(|_| !matches!(first, "cover" | "contain")) {
                    end += after[end..].find(second)? + second.len();
                    size = format!("{size} {second}");
                }
                return Some((format!("{} {}", &value[..i], &after[end..]), size));
            }
            _ => {}
        }
    }
    None
}

/// Split at commas outside brackets, parentheses and quotes.
pub fn split_selector_list(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0, None);
    for (i, c) in text.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            (_, Some(_)) => {}
            ('[' | '(', None) => depth += 1,
            (']' | ')', None) => depth -= 1,
            (',', None) if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A rule's selectors: the ones Brickle understands (a rule whose other
/// selectors it doesn't still applies through these).
pub fn parse_selector_list(text: &str) -> Vec<Selector> {
    split_selector_list(text)
        .into_iter()
        .take(MAX_SELECTOR_LIST)
        .filter_map(parse_selector)
        .collect()
}

pub fn parse_selector(text: &str) -> Option<Selector> {
    parse_complex(text, false, 0)
}

/// Parse a complex selector; `relative` allows a leading combinator (for
/// `:has()`), which becomes the leftmost part's combinator (descendant if
/// none is written).
fn parse_complex(text: &str, relative: bool, depth: usize) -> Option<Selector> {
    let text = text.trim();
    if text.is_empty() || depth > MAX_SELECTOR_NESTING {
        return None;
    }
    let mut compounds: Vec<(SimpleSelector, Combinator)> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut pending = if relative {
        Combinator::Descendant
    } else {
        Combinator::None
    };
    let mut leading_written = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if pending == Combinator::None && !compounds.is_empty() {
                pending = Combinator::Descendant;
            }
            i += 1;
            continue;
        }
        if matches!(c, '>' | '+' | '~') {
            // A second explicit combinator in a row is an error.
            let explicit = !matches!(pending, Combinator::None | Combinator::Descendant);
            if explicit || (compounds.is_empty() && (!relative || leading_written)) {
                return None;
            }
            leading_written |= compounds.is_empty();
            pending = match c {
                '>' => Combinator::Child,
                '+' => Combinator::Next,
                _ => Combinator::Subsequent,
            };
            i += 1;
            continue;
        }
        let s = parse_compound(&chars, &mut i, depth)?;
        compounds.push((s, pending));
        pending = Combinator::None;
    }
    let trailing = !compounds.is_empty() && pending != Combinator::None;
    if compounds.is_empty() || trailing || compounds.len() > MAX_SELECTOR_PARTS {
        return None;
    }
    // Store right-to-left, with each combinator describing the link to the
    // left (for a relative selector, the leftmost one's link to the anchor).
    let n = compounds.len();
    let mut parts = Vec::with_capacity(n);
    for k in (0..n).rev() {
        let comb = if k == 0 && !relative {
            Combinator::None
        } else {
            compounds[k].1
        };
        parts.push((compounds[k].0.clone(), comb));
    }
    Some(Selector { parts })
}

/// One compound selector (`a.b#c[d]:e`) starting at `chars[*i]`.
fn parse_compound(chars: &[char], i: &mut usize, depth: usize) -> Option<SimpleSelector> {
    // A name, with CSS escapes (`.md\:flex` is the class `md:flex`;
    // `\31 0` is `10`).
    let ident = |j: &mut usize| -> String {
        let mut out = String::new();
        while *j < chars.len() {
            let c = chars[*j];
            if c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
                out.push(c);
                *j += 1;
            } else if c == '\\' && *j + 1 < chars.len() && chars[*j + 1] != '\n' {
                *j += 1;
                let hex_end = (*j..chars.len().min(*j + 6))
                    .find(|&k| !chars[k].is_ascii_hexdigit())
                    .unwrap_or(chars.len().min(*j + 6));
                if hex_end > *j {
                    let code: String = chars[*j..hex_end].iter().collect();
                    let ch = u32::from_str_radix(&code, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .filter(|&c| c != '\0')
                        .unwrap_or('\u{FFFD}');
                    out.push(ch);
                    *j = hex_end;
                    // One whitespace after a hex escape belongs to it.
                    if *j < chars.len() && chars[*j].is_whitespace() {
                        *j += 1;
                    }
                } else {
                    out.push(chars[*j]);
                    *j += 1;
                }
            } else {
                break;
            }
        }
        out
    };
    let mut s = SimpleSelector::default();
    let mut any = false;
    while *i < chars.len() && !chars[*i].is_whitespace() && !matches!(chars[*i], '>' | '+' | '~') {
        let c = chars[*i];
        match c {
            '*' => *i += 1,
            '#' => {
                *i += 1;
                s.id = Some(ident(i));
            }
            '.' => {
                *i += 1;
                s.classes.push(ident(i));
            }
            '[' => {
                let st = *i + 1;
                let mut quote = None;
                while *i < chars.len() && (quote.is_some() || chars[*i] != ']') {
                    match (chars[*i], quote) {
                        ('"' | '\'', None) => quote = Some(chars[*i]),
                        (q, Some(open)) if q == open => quote = None,
                        _ => {}
                    }
                    *i += 1;
                }
                if *i >= chars.len() {
                    return None;
                }
                let inner: String = chars[st..*i].iter().collect();
                s.attrs.push(parse_attr(&inner)?);
                *i += 1;
            }
            ':' => {
                *i += 1;
                if *i < chars.len() && chars[*i] == ':' {
                    return None; // pseudo-elements
                }
                let name = ident(i).to_ascii_lowercase();
                // A functional pseudo-class's argument, up to its `)`.
                let arg = if *i < chars.len() && chars[*i] == '(' {
                    let st = *i + 1;
                    let mut d = 0;
                    while *i < chars.len() {
                        match chars[*i] {
                            '(' => d += 1,
                            ')' => {
                                d -= 1;
                                if d == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        *i += 1;
                    }
                    if *i >= chars.len() {
                        return None;
                    }
                    *i += 1;
                    Some(chars[st..*i - 1].iter().collect::<String>())
                } else {
                    None
                };
                let list = |a: &str, relative: bool| -> Option<Vec<Selector>> {
                    let v: Vec<Selector> = split_selector_list(a)
                        .into_iter()
                        .take(MAX_SELECTOR_LIST)
                        .filter_map(|t| parse_complex(t, relative, depth + 1))
                        .collect();
                    (!v.is_empty()).then_some(v)
                };
                let nth = |of_type: bool, from_end: bool| -> Option<Pseudo> {
                    let (a, b) = parse_nth(arg.as_deref()?)?;
                    Some(Pseudo::Nth {
                        a,
                        b,
                        of_type,
                        from_end,
                    })
                };
                let first = |of_type, from_end| Pseudo::Nth {
                    a: 0,
                    b: 1,
                    of_type,
                    from_end,
                };
                match (name.as_str(), arg.as_deref()) {
                    ("root", None) => s.root = true,
                    ("link" | "any-link", None) => s.attrs.push(AttrSelector {
                        name: "href".into(),
                        op: AttrOp::Exists,
                        value: String::new(),
                        ignore_case: false,
                    }),
                    ("first-child", None) => s.pseudo.push(first(false, false)),
                    ("last-child", None) => s.pseudo.push(first(false, true)),
                    ("first-of-type", None) => s.pseudo.push(first(true, false)),
                    ("last-of-type", None) => s.pseudo.push(first(true, true)),
                    ("only-child", None) => s.pseudo.push(Pseudo::Only { of_type: false }),
                    ("only-of-type", None) => s.pseudo.push(Pseudo::Only { of_type: true }),
                    ("empty", None) => s.pseudo.push(Pseudo::Empty),
                    ("nth-child", Some(_)) => s.pseudo.push(nth(false, false)?),
                    ("nth-last-child", Some(_)) => s.pseudo.push(nth(false, true)?),
                    ("nth-of-type", Some(_)) => s.pseudo.push(nth(true, false)?),
                    ("nth-last-of-type", Some(_)) => s.pseudo.push(nth(true, true)?),
                    // Unknown selectors inside :not() make it match nothing
                    // rather than everything.
                    ("not", Some(a)) => s.pseudo.push(Pseudo::Not(list(a, false)?)),
                    ("is" | "matches" | "-webkit-any", Some(a)) => s.pseudo.push(Pseudo::Is {
                        list: list(a, false)?,
                        where_: false,
                    }),
                    ("where", Some(a)) => s.pseudo.push(Pseudo::Is {
                        list: list(a, false)?,
                        where_: true,
                    }),
                    ("has", Some(a)) => s.pseudo.push(Pseudo::Has(list(a, true)?)),
                    (
                        "hover" | "active" | "focus" | "focus-visible" | "focus-within" | "visited"
                        | "checked" | "target" | "target-within" | "indeterminate" | "invalid"
                        | "user-invalid" | "autofill" | "placeholder-shown" | "fullscreen" | "modal"
                        | "popover-open" | "open",
                        None,
                    ) => s.pseudo.push(Pseudo::Never),
                    // Form states we can tell from attributes.
                    ("disabled", None) => s.attrs.push(AttrSelector {
                        name: "disabled".into(),
                        op: AttrOp::Exists,
                        value: String::new(),
                        ignore_case: false,
                    }),
                    ("enabled" | "valid" | "read-write" | "defined" | "scope", None) => {}
                    _ => return None,
                }
            }
            c if c.is_alphanumeric() || c == '-' || c == '_' => {
                s.tag = Some(ident(i).to_ascii_lowercase());
            }
            _ => return None,
        }
        any = true;
    }
    any.then_some(s)
}

/// The inside of `[...]`.
fn parse_attr(inner: &str) -> Option<AttrSelector> {
    let inner = inner.trim();
    // The first `=` ends the name (and operator); later ones are the value's.
    let found = inner.find('=').map(|eq| {
        let op = match inner[..eq].chars().last() {
            Some('~') => AttrOp::Word,
            Some('|') => AttrOp::Lang,
            Some('^') => AttrOp::Prefix,
            Some('$') => AttrOp::Suffix,
            Some('*') => AttrOp::Contains,
            _ => AttrOp::Equals,
        };
        let at = if op == AttrOp::Equals { eq } else { eq - 1 };
        (at, eq + 1 - at, op)
    });
    let Some((at, len, op)) = found else {
        let name = inner.to_ascii_lowercase();
        let ok = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_');
        return ok.then_some(AttrSelector {
            name,
            op: AttrOp::Exists,
            value: String::new(),
            ignore_case: false,
        });
    };
    let name = inner[..at].trim().to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let mut rest = inner[at + len..].trim();
    let mut ignore_case = false;
    for flag in [" i", " I", " s", " S"] {
        if let Some(r) = rest.strip_suffix(flag) {
            ignore_case = flag.trim().eq_ignore_ascii_case("i");
            rest = r.trim_end();
        }
    }
    let value = match rest.chars().next() {
        Some(q @ ('"' | '\'')) => rest.strip_prefix(q)?.strip_suffix(q)?.to_string(),
        _ if rest.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') => rest.to_string(),
        _ => return None,
    };
    Some(AttrSelector {
        name,
        op,
        value,
        ignore_case,
    })
}

/// `an+b` as written in `:nth-child()`: `odd`, `even`, `3`, `2n+1`, `-n+3`...
/// (`of S` isn't supported).
fn parse_nth(arg: &str) -> Option<(i32, i32)> {
    let t: String = arg
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match t.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    let num = |s: &str| -> Option<i32> {
        let s = s.strip_prefix('+').unwrap_or(s);
        if s.is_empty() || !s.trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse::<i32>().ok().filter(|n| n.abs() <= 1_000_000)
    };
    match t.find('n') {
        None => Some((0, num(&t)?)),
        Some(at) => {
            let a = match &t[..at] {
                "" | "+" => 1,
                "-" => -1,
                s => num(s)?,
            };
            let b = match &t[at + 1..] {
                "" => 0,
                s if s.starts_with('+') || s.starts_with('-') => num(s)?,
                _ => return None,
            };
            Some((a, b))
        }
    }
}

pub fn parse_values(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == '/' {
            i += 1;
            continue;
        }
        if c == ',' {
            out.push(Value::Comma);
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            let st = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += 1;
            }
            out.push(Value::Str(chars[st..i.min(chars.len())].iter().collect()));
            i += 1;
            continue;
        }
        // A token: ident, number+unit, #hex, or function(...)
        let st = i;
        let mut depth = 0;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
            } else if depth == 0 && (ch.is_whitespace() || ch == ',' || ch == '/') {
                break;
            }
            i += 1;
        }
        let tok: String = chars[st..i].iter().collect();
        if let Some(v) = parse_value_token(&tok) {
            out.push(v);
        }
    }
    out
}

fn parse_value_token(tok: &str) -> Option<Value> {
    let lower = tok.to_ascii_lowercase();
    if lower.starts_with("url(") && tok.ends_with(')') {
        let inner = tok[4..tok.len() - 1].trim();
        let inner = inner.trim_matches(|c| c == '"' || c == '\'');
        return Some(Value::Url(inner.to_string()));
    }
    if let Some(hex) = lower.strip_prefix('#') {
        return parse_hex(hex).map(Value::Color);
    }
    if let Some(args) = lower.strip_prefix("rgb(").or_else(|| lower.strip_prefix("rgba(")) {
        let nums: Vec<f32> = args
            .trim_end_matches(')')
            .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(|s| {
                if let Some(p) = s.strip_suffix('%') {
                    p.parse::<f32>().ok().map(|v| v / 100.0 * 255.0)
                } else {
                    s.parse().ok()
                }
            })
            .collect();
        if nums.len() >= 3 {
            let a = nums
                .get(3)
                .map(|a| if *a <= 1.0 { a * 255.0 } else { *a })
                .unwrap_or(255.0);
            return Some(Value::Color(Color {
                r: nums[0].clamp(0.0, 255.0) as u8,
                g: nums[1].clamp(0.0, 255.0) as u8,
                b: nums[2].clamp(0.0, 255.0) as u8,
                a: a.clamp(0.0, 255.0) as u8,
            }));
        }
        return None;
    }
    if lower.contains('(') {
        // calc(), var() and the like are worked out when computing styles;
        // url() and others aren't supported.
        return Some(Value::Keyword(lower));
    }
    let num_end = lower
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0)))
        .map(|(i, _)| i)
        .unwrap_or(lower.len());
    if num_end > 0 {
        if let Ok(n) = lower[..num_end].parse::<f32>() {
            let unit = &lower[num_end..];
            return Some(match unit {
                "" => Value::Number(n),
                "%" => Value::Percent(n),
                "px" => Value::Length(n, Unit::Px),
                "em" => Value::Length(n, Unit::Em),
                "rem" => Value::Length(n, Unit::Rem),
                "pt" => Value::Length(n, Unit::Pt),
                "vw" => Value::Length(n, Unit::Vw),
                "vh" => Value::Length(n, Unit::Vh),
                "ex" | "ch" => Value::Length(n * 0.5, Unit::Em),
                // Grid fractions, read by `grid-template-columns`.
                "fr" => Value::Keyword(lower),
                _ => return None,
            });
        }
    }
    if let Some(c) = named_color(&lower) {
        return Some(Value::Color(c));
    }
    Some(Value::Keyword(lower))
}

fn parse_hex(hex: &str) -> Option<Color> {
    // Byte-slicing below assumes ASCII (found by fuzzing).
    if !hex.is_ascii() {
        return None;
    }
    let d = |s: &str| u8::from_str_radix(s, 16).ok();
    let dup = |c: char| d(&format!("{c}{c}"));
    let cs: Vec<char> = hex.chars().collect();
    match cs.len() {
        3 | 4 => Some(Color {
            r: dup(cs[0])?,
            g: dup(cs[1])?,
            b: dup(cs[2])?,
            a: if cs.len() == 4 { dup(cs[3])? } else { 255 },
        }),
        6 | 8 => Some(Color {
            r: d(&hex[0..2])?,
            g: d(&hex[2..4])?,
            b: d(&hex[4..6])?,
            a: if cs.len() == 8 { d(&hex[6..8])? } else { 255 },
        }),
        _ => None,
    }
}

fn named_color(name: &str) -> Option<Color> {
    let (r, g, b) = match name {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "navy" => (0, 0, 128),
        "purple" => (128, 0, 128),
        "teal" => (0, 128, 128),
        "orange" => (255, 165, 0),
        "pink" => (255, 192, 203),
        "brown" => (165, 42, 42),
        "gold" => (255, 215, 0),
        "indigo" => (75, 0, 130),
        "violet" => (238, 130, 238),
        "coral" => (255, 127, 80),
        "salmon" => (250, 128, 114),
        "tomato" => (255, 99, 71),
        "crimson" => (220, 20, 60),
        "khaki" => (240, 230, 140),
        "beige" => (245, 245, 220),
        "ivory" => (255, 255, 240),
        "lavender" => (230, 230, 250),
        "tan" => (210, 180, 140),
        "chocolate" => (210, 105, 30),
        "steelblue" => (70, 130, 180),
        "skyblue" => (135, 206, 235),
        "royalblue" => (65, 105, 225),
        "dodgerblue" => (30, 144, 255),
        "slategray" | "slategrey" => (112, 128, 144),
        "darkblue" => (0, 0, 139),
        "darkgreen" => (0, 100, 0),
        "darkred" => (139, 0, 0),
        "whitesmoke" => (245, 245, 245),
        "gainsboro" => (220, 220, 220),
        "aliceblue" => (240, 248, 255),
        "honeydew" => (240, 255, 240),
        "mintcream" => (245, 255, 250),
        "linen" => (250, 240, 230),
        "seagreen" => (46, 139, 87),
        "forestgreen" => (34, 139, 34),
        "rebeccapurple" => (102, 51, 153),
        "transparent" => return Some(Color::TRANSPARENT),
        _ => return None,
    };
    Some(Color::rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_names() {
        let sheet = parse(
            r".md\:flex-row{a:b} .max-w-\[1400px\]{a:b} .w-1\/2{a:b} #\31 0{a:b} .\E9t\E9{a:b} .a\.b>.c{a:b}",
        );
        let names: Vec<String> = sheet
            .rules
            .iter()
            .map(|r| {
                let s = &r.selectors[0].parts[0].0;
                s.classes.first().or(s.id.as_ref()).cloned().unwrap_or_default()
            })
            .collect();
        assert_eq!(
            names,
            ["md:flex-row", "max-w-[1400px]", "w-1/2", "10", "été", "c"]
        );
        assert_eq!(sheet.rules[5].selectors[0].parts[1].0.classes, ["a.b"]);
    }

    #[test]
    fn background_shorthand_size() {
        let d =
            parse_inline("background: no-repeat center/100% url(\"/logo.svg\"), linear-gradient(red, blue)");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].name, "background");
        assert!(d[0].values.contains(&Value::Url("/logo.svg".into())));
        assert!(!d[0].values.contains(&Value::Percent(100.0)), "{:?}", d[0].values);
        assert_eq!(
            (d[1].name.as_str(), d[1].values.clone()),
            ("background-size", vec![Value::Percent(100.0)])
        );
        let d = parse_inline("background: url(a.png) 0 0 / 20px 10px repeat-x");
        assert_eq!(
            d[1].values,
            vec![Value::Length(20.0, Unit::Px), Value::Length(10.0, Unit::Px)]
        );
        assert!(d[0].values.contains(&Value::Keyword("repeat-x".into())));
        let d = parse_inline("background: url(a.png) center / cover");
        assert_eq!(d[1].values, vec![Value::Keyword("cover".into())]);
        // No slash, or one only in a later layer or inside url(): unchanged.
        assert_eq!(parse_inline("background: red url(a/b.png)").len(), 1);
        assert_eq!(parse_inline("background: red, url(a.png) 0 0/10px").len(), 1);
    }

    #[test]
    fn media_queries_follow_the_viewport() {
        let m = |q: &str, w: f32, h: f32| Media::parse(q).matches((w, h));
        // DuckDuckGo's, minified: only for very short, wide windows.
        let ddg = "only screen and (max-height: 361.25px)and (min-width: 864px)";
        assert!(!m(ddg, 1024.0, 768.0));
        assert!(m(ddg, 1024.0, 300.0));
        assert!(m("(max-width: 600px)", 500.0, 800.0));
        assert!(!m("(max-width: 600px)", 1200.0, 800.0));
        assert!(m("screen and (min-width:40em)", 700.0, 800.0));
        assert!(!m("print", 1000.0, 800.0));
        assert!(m("print, (min-width: 100px)", 1000.0, 800.0));
        assert!(m("not print", 1000.0, 800.0));
        assert!(!m("not all and (min-width: 100px)", 1000.0, 800.0));
        assert!(m("(width >= 600px)", 600.0, 1.0));
        assert!(m("(400px <= width < 900px)", 500.0, 1.0));
        assert!(!m("(400px <= width < 900px)", 900.0, 1.0));
        assert!(m("(orientation: landscape)", 1000.0, 800.0));
        assert!(!m("(prefers-color-scheme: dark)", 1000.0, 800.0));
        assert!(m("(prefers-reduced-motion: no-preference)", 1000.0, 800.0));
        assert!(m("(hover: hover) and (pointer: fine)", 1000.0, 800.0));
        assert!(!m(
            "(-webkit-min-device-pixel-ratio: 2), (min-resolution: 192dpi)",
            1000.0,
            800.0
        ));
        assert!(!m("(some-future-thing: yes)", 1000.0, 800.0));
        assert!(m("(max-width: 500px) or (min-width: 900px)", 1000.0, 800.0));

        let sheet = parse("@media (max-width: 600px) { p { color: red } } @media print { p { color: blue } } a { color: green }");
        assert_eq!(sheet.rules.len(), 2, "print rules are dropped");
        assert!(sheet.rules[0].applies((500.0, 800.0)));
        assert!(!sheet.rules[0].applies((1000.0, 800.0)));
        assert!(sheet.rules[1].applies((1000.0, 800.0)));
    }

    #[test]
    fn urls_keep_their_case_and_resolve_against_the_sheet() {
        assert_eq!(
            parse_values("url('Img/Logo.PNG') no-repeat")[0],
            Value::Url("Img/Logo.PNG".into())
        );
        let mut sheet = parse("a { background: url(img/a.png) } b { background-image: url(/top.png) }");
        resolve_urls(&mut sheet, "https://example.com/css/site.css");
        let url = |i: usize| match &sheet.rules[i].declarations[0].values[0] {
            Value::Url(u) => u.clone(),
            v => panic!("{v:?}"),
        };
        assert_eq!(url(0), "https://example.com/css/img/a.png");
        assert_eq!(url(1), "https://example.com/top.png");
    }

    #[test]
    fn parses_rules_and_values() {
        let ss = parse("/* c */ h1, .a > p#x { color: #f00; margin: 0 auto !important }");
        assert_eq!(ss.rules.len(), 1);
        let r = &ss.rules[0];
        assert_eq!(r.selectors.len(), 2);
        assert_eq!(r.selectors[1].specificity(), (1, 1, 1));
        assert_eq!(r.selectors[1].parts[0].1, Combinator::Child);
        assert_eq!(
            r.declarations[0].values,
            vec![Value::Color(Color::rgb(255, 0, 0))]
        );
        assert!(r.declarations[1].important);
    }

    #[test]
    fn drops_unsupported_selectors_only() {
        let ss = parse("a::before, b { color: red } @font-face { x: y } i { color: blue }");
        assert_eq!(ss.rules.len(), 2);
        assert_eq!(ss.rules[0].selectors.len(), 1);
        // States the page isn't in are kept (they just don't match), so
        // `:not(:hover)` works.
        let ss = parse("a:hover, b { color: red }");
        assert_eq!(ss.rules[0].selectors.len(), 2);
    }

    #[test]
    fn non_ascii_hex_color() {
        // Found by fuzzing: used to slice through a multi-byte character.
        assert_eq!(parse_values("#ab\u{cd}d\u{e2}"), vec![]);
        parse("p { color: #f\u{fffd}0 }");
    }

    #[test]
    fn escape_at_end_of_input() {
        // Found by fuzzing: a trailing backslash inside a string overran.
        parse("a { content: \"x\\");
        parse_inline("x: 'a\\");
    }

    #[test]
    fn rgb_functions() {
        assert_eq!(
            parse_values("rgba(10, 20, 30, 0.5)"),
            vec![Value::Color(Color {
                r: 10,
                g: 20,
                b: 30,
                a: 127
            })]
        );
    }
}
