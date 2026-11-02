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
    None,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimpleSelector {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<String>,
    pub root: bool,
}

pub type Specificity = (usize, usize, usize);

impl Selector {
    pub fn specificity(&self) -> Specificity {
        let mut s = (0, 0, 0);
        for (p, _) in &self.parts {
            s.0 += p.id.iter().count();
            s.1 += p.classes.len() + p.attrs.len() + p.root as usize;
            s.2 += p.tag.iter().count();
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
    Comma,
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
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
}

/// Deepest `@media`/`@supports` nesting we follow.
const MAX_NESTING: usize = 8;
/// Longest selector (number of compound parts) we accept.
const MAX_SELECTOR_PARTS: usize = 32;
/// Most rules kept from one stylesheet.
pub const MAX_RULES: usize = 5_000;

pub fn parse(source: &str) -> Stylesheet {
    let src = strip_comments(source);
    let mut p = Parser { chars: src.chars().collect(), pos: 0 };
    Stylesheet { rules: p.rules(0) }
}

/// Parse the contents of a `style="..."` attribute.
pub fn parse_inline(source: &str) -> Vec<Declaration> {
    let src = strip_comments(source);
    let mut p = Parser { chars: src.chars().collect(), pos: 0 };
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

    fn rules(&mut self, depth: usize) -> Vec<Rule> {
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
                    // We render to a "screen"; honour simple @media screen /
                    // min-width queries generously, skip everything else.
                    let usable = (lower.starts_with("@media")
                        && !lower.contains("print")
                        && !lower.contains("max-width"))
                        || lower.starts_with("@supports")
                        || lower.starts_with("@layer");
                    if usable && depth < MAX_NESTING {
                        self.pos += 1;
                        rules.extend(self.rules(depth + 1));
                    } else {
                        self.skip_block();
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
                    let selectors: Vec<Selector> =
                        prelude.split(',').filter_map(parse_selector).collect();
                    if !selectors.is_empty() && !declarations.is_empty() && rules.len() < MAX_RULES {
                        rules.push(Rule { selectors, declarations });
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
                if !name.is_empty() && !name.starts_with("--") {
                    decls.push(Declaration { name, values: parse_values(value), important });
                }
            }
        }
        decls
    }
}

pub fn parse_selector(text: &str) -> Option<Selector> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut compounds: Vec<(SimpleSelector, Combinator)> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut pending = Combinator::None;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if pending == Combinator::None && !compounds.is_empty() {
                pending = Combinator::Descendant;
            }
            i += 1;
            continue;
        }
        if c == '>' {
            pending = Combinator::Child;
            i += 1;
            continue;
        }
        if c == '+' || c == '~' {
            return None; // sibling combinators unsupported
        }
        // Parse one compound selector.
        let mut s = SimpleSelector::default();
        let mut any = false;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '>' {
            let c = chars[i];
            let ident = |j: &mut usize| -> String {
                let st = *j;
                while *j < chars.len()
                    && (chars[*j].is_alphanumeric() || chars[*j] == '-' || chars[*j] == '_')
                {
                    *j += 1;
                }
                chars[st..*j].iter().collect()
            };
            match c {
                '*' => {
                    i += 1;
                }
                '#' => {
                    i += 1;
                    s.id = Some(ident(&mut i));
                }
                '.' => {
                    i += 1;
                    s.classes.push(ident(&mut i));
                }
                '[' => {
                    let st = i;
                    while i < chars.len() && chars[i] != ']' {
                        i += 1;
                    }
                    let inner: String = chars[st + 1..i.min(chars.len())].iter().collect();
                    // Only presence selectors like [hidden]; anything fancier fails.
                    if inner.contains('=') {
                        return None;
                    }
                    s.attrs.push(inner.trim().to_ascii_lowercase());
                    i += 1;
                }
                ':' => {
                    i += 1;
                    if i < chars.len() && chars[i] == ':' {
                        return None; // pseudo-elements
                    }
                    let name = ident(&mut i).to_ascii_lowercase();
                    match name.as_str() {
                        "root" => s.root = true,
                        "link" | "any-link" => s.attrs.push("href".into()),
                        "visited" | "hover" | "active" | "focus" | "focus-visible"
                        | "focus-within" | "checked" | "disabled" | "target" | "first-child"
                        | "last-child" | "nth-child" | "not" | "is" | "where" | "has"
                        | "first-of-type" | "last-of-type" | "nth-of-type" | "empty" => {
                            return None
                        }
                        _ => return None,
                    }
                }
                c if c.is_alphanumeric() || c == '-' || c == '_' => {
                    s.tag = Some(ident(&mut i).to_ascii_lowercase());
                }
                _ => return None,
            }
            any = true;
        }
        if !any {
            return None;
        }
        compounds.push((s, pending));
        pending = Combinator::None;
    }
    if compounds.is_empty() || pending != Combinator::None || compounds.len() > MAX_SELECTOR_PARTS {
        return None;
    }
    // Store right-to-left, with each combinator describing the link to the left.
    let n = compounds.len();
    let mut parts = Vec::with_capacity(n);
    for k in (0..n).rev() {
        let comb = if k == 0 { Combinator::None } else { compounds[k].1 };
        parts.push((compounds[k].0.clone(), comb));
    }
    Some(Selector { parts })
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
            let a = nums.get(3).map(|a| if *a <= 1.0 { a * 255.0 } else { *a }).unwrap_or(255.0);
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
        // calc(), var(), url(), etc. — unsupported for now.
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
    fn parses_rules_and_values() {
        let ss = parse("/* c */ h1, .a > p#x { color: #f00; margin: 0 auto !important }");
        assert_eq!(ss.rules.len(), 1);
        let r = &ss.rules[0];
        assert_eq!(r.selectors.len(), 2);
        assert_eq!(r.selectors[1].specificity(), (1, 1, 1));
        assert_eq!(r.selectors[1].parts[0].1, Combinator::Child);
        assert_eq!(r.declarations[0].values, vec![Value::Color(Color::rgb(255, 0, 0))]);
        assert!(r.declarations[1].important);
    }

    #[test]
    fn drops_unsupported_selectors_only() {
        let ss = parse("a:hover, b { color: red } @font-face { x: y } i { color: blue }");
        assert_eq!(ss.rules.len(), 2);
        assert_eq!(ss.rules[0].selectors.len(), 1);
    }

    #[test]
    fn rgb_functions() {
        assert_eq!(
            parse_values("rgba(10, 20, 30, 0.5)"),
            vec![Value::Color(Color { r: 10, g: 20, b: 30, a: 127 })]
        );
    }
}
