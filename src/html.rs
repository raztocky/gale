//! A forgiving HTML parser: tokenizer + a simplified tree builder.
//!
//! This is nowhere near the full WHATWG algorithm, but it handles the things
//! real pages lean on most: void elements, implicitly closed <p>/<li>/<td>,
//! raw-text <script>/<style>, comments, doctypes and character references.

use crate::dom::{self, AttrMap, Node};

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track",
    "wbr",
];
const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title"];
/// Opening one of these closes an open <p>.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "div",
    "dl",
    "fieldset",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];

/// Deepest element nesting we build. Deeper start tags are ignored (their
/// text is kept), which stops hostile pages from overflowing the stack in the
/// recursive style/layout/paint passes. Browsers use similar caps.
pub const MAX_DEPTH: usize = 256;
/// Most nodes we create for one document.
pub const MAX_NODES: usize = 100_000;

pub fn parse(source: &str) -> Node {
    let tokens = Tokenizer {
        chars: source.chars().collect(),
        pos: 0,
    }
    .run();
    let mut b = TreeBuilder {
        stack: vec![dom::elem("#document".into(), AttrMap::new(), vec![])],
        nodes: 0,
    };
    for t in tokens {
        b.feed(t);
    }
    normalize(b.finish())
}

#[derive(Debug)]
enum Token {
    Start {
        name: String,
        attrs: AttrMap,
        self_closing: bool,
    },
    End(String),
    Text(String),
    Comment(String),
}

struct Tokenizer {
    chars: Vec<char>,
    pos: usize,
}

impl Tokenizer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.chars.get(self.pos + i) == Some(&c))
    }
    fn starts_with_ci(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.chars.get(self.pos + i).map(|x| x.to_ascii_lowercase()) == Some(c))
    }
    fn eat_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let start = self.pos;
        while self.peek().is_some_and(&f) {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }
    fn skip_ws(&mut self) {
        self.eat_while(char::is_whitespace);
    }
    fn skip_past(&mut self, s: &str) {
        while self.pos < self.chars.len() && !self.starts_with(s) {
            self.pos += 1;
        }
        self.pos = (self.pos + s.len()).min(self.chars.len());
    }

    fn run(mut self) -> Vec<Token> {
        let mut out = Vec::new();
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c == '<' {
                let next = self.chars.get(self.pos + 1).copied().unwrap_or(' ');
                if self.starts_with("<!--") {
                    if !text.is_empty() {
                        out.push(Token::Text(decode_entities(&std::mem::take(&mut text))));
                    }
                    // `<!-->` and `<!--->` are empty comments.
                    let start = self.pos + 4;
                    self.pos += 2;
                    self.skip_past("-->");
                    let end = self.pos.saturating_sub(3).max(start).min(self.chars.len());
                    out.push(Token::Comment(self.chars[start.min(end)..end].iter().collect()));
                    continue;
                }
                if next == '!' || next == '?' {
                    self.skip_past(">"); // doctype / processing instruction
                    continue;
                }
                if next == '/' || next.is_ascii_alphabetic() {
                    if !text.is_empty() {
                        out.push(Token::Text(decode_entities(&std::mem::take(&mut text))));
                    }
                    let tok = self.tag();
                    if let Token::Start {
                        name,
                        self_closing: false,
                        ..
                    } = &tok
                    {
                        if RAW_TEXT.contains(&name.as_str()) {
                            let name = name.clone();
                            out.push(tok);
                            let close = format!("</{name}");
                            let start = self.pos;
                            while self.pos < self.chars.len() && !self.starts_with_ci(&close) {
                                self.pos += 1;
                            }
                            let raw: String = self.chars[start..self.pos].iter().collect();
                            if !raw.is_empty() {
                                let raw = if name == "title" || name == "textarea" {
                                    decode_entities(&raw)
                                } else {
                                    raw
                                };
                                out.push(Token::Text(raw));
                            }
                            self.skip_past(">");
                            out.push(Token::End(name));
                            continue;
                        }
                    }
                    out.push(tok);
                    continue;
                }
            }
            text.push(c);
            self.pos += 1;
        }
        if !text.is_empty() {
            out.push(Token::Text(decode_entities(&text)));
        }
        out
    }

    fn tag(&mut self) -> Token {
        self.pos += 1; // '<'
        let closing = self.peek() == Some('/');
        if closing {
            self.pos += 1;
        }
        let name = self
            .eat_while(|c| !c.is_whitespace() && c != '>' && c != '/')
            .to_ascii_lowercase();
        let mut attrs = AttrMap::new();
        let mut self_closing = false;
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some('>') => {
                    self.pos += 1;
                    break;
                }
                Some('/') => {
                    self.pos += 1;
                    self_closing = true;
                }
                Some(_) => {
                    let key = self
                        .eat_while(|c| !c.is_whitespace() && c != '=' && c != '>' && c != '/')
                        .to_ascii_lowercase();
                    if key.is_empty() {
                        self.pos += 1;
                        continue;
                    }
                    self.skip_ws();
                    let mut val = String::new();
                    if self.peek() == Some('=') {
                        self.pos += 1;
                        self.skip_ws();
                        val = match self.peek() {
                            Some(q @ ('"' | '\'')) => {
                                self.pos += 1;
                                let v = self.eat_while(|c| c != q);
                                // The closing quote may be missing at end of input.
                                if self.peek() == Some(q) {
                                    self.pos += 1;
                                }
                                v
                            }
                            _ => self.eat_while(|c| !c.is_whitespace() && c != '>'),
                        };
                    }
                    attrs.entry(key).or_insert_with(|| decode_entities(&val));
                }
            }
        }
        if closing {
            Token::End(name)
        } else {
            Token::Start {
                name,
                attrs,
                self_closing,
            }
        }
    }
}

struct TreeBuilder {
    stack: Vec<Node>,
    nodes: usize,
}

impl TreeBuilder {
    fn current_tag(&self) -> &str {
        self.stack.last().and_then(|n| n.tag()).unwrap_or("")
    }

    fn pop(&mut self) {
        if self.stack.len() > 1 {
            let n = self.stack.pop().unwrap();
            self.stack.last_mut().unwrap().children.push(n);
        }
    }

    /// Close `name` if it is open and no element from `boundaries` sits above it.
    fn close_in_scope(&mut self, names: &[&str], boundaries: &[&str]) {
        for i in (1..self.stack.len()).rev() {
            let t = self.stack[i].tag().unwrap_or("");
            if names.contains(&t) {
                while self.stack.len() > i {
                    self.pop();
                }
                return;
            }
            if boundaries.contains(&t) {
                return;
            }
        }
    }

    fn feed(&mut self, tok: Token) {
        match tok {
            Token::Text(mut t) => {
                let parent = self.stack.last_mut().unwrap();
                // A newline right after <pre> is ignored.
                if matches!(parent.tag(), Some("pre") | Some("textarea")) && parent.children.is_empty() {
                    if let Some(rest) = t.strip_prefix('\n') {
                        t = rest.to_string();
                    }
                }
                if let Some(Node {
                    node_type: dom::NodeType::Text(prev),
                    ..
                }) = parent.children.last_mut()
                {
                    prev.push_str(&t);
                } else if self.nodes < MAX_NODES {
                    self.nodes += 1;
                    parent.children.push(dom::text(t));
                }
            }
            Token::Comment(c) => {
                if self.nodes < MAX_NODES {
                    self.nodes += 1;
                    self.stack.last_mut().unwrap().children.push(dom::comment(c));
                }
            }
            Token::Start {
                name,
                attrs,
                self_closing,
            } => {
                if self.nodes >= MAX_NODES {
                    return;
                }
                let block_boundary = ["div", "td", "th", "li", "body", "html", "button", "blockquote"];
                if CLOSES_P.contains(&name.as_str()) {
                    self.close_in_scope(&["p"], &block_boundary);
                }
                match name.as_str() {
                    "li" => self.close_in_scope(&["li"], &["ul", "ol", "menu"]),
                    "dt" | "dd" => self.close_in_scope(&["dt", "dd"], &["dl"]),
                    "tr" => self.close_in_scope(&["tr"], &["table"]),
                    "td" | "th" => self.close_in_scope(&["td", "th"], &["tr", "table"]),
                    "option" => self.close_in_scope(&["option"], &["select"]),
                    "a" => self.close_in_scope(&["a"], &block_boundary),
                    _ => {}
                }
                let void = VOID.contains(&name.as_str());
                if self.stack.len() >= MAX_DEPTH {
                    return;
                }
                self.nodes += 1;
                self.stack.push(dom::elem(name, attrs, vec![]));
                if void || self_closing {
                    self.pop();
                }
            }
            Token::End(name) => {
                if matches!(name.as_str(), "body" | "html") || VOID.contains(&name.as_str()) {
                    return;
                }
                if self.current_tag() == name {
                    self.pop();
                } else if let Some(i) = self.stack.iter().skip(1).rposition(|n| n.tag() == Some(&name)) {
                    // skip(1): never match the internal #document root (found
                    // by fuzzing: `</#document>` looped forever).
                    let i = i + 1;
                    while self.stack.len() > i {
                        self.pop();
                    }
                }
            }
        }
    }

    fn finish(mut self) -> Node {
        while self.stack.len() > 1 {
            self.pop();
        }
        self.stack.pop().unwrap()
    }
}

/// Make sure we end up with `<html><head>..</head><body>..</body></html>`.
fn normalize(doc: Node) -> Node {
    let mut html_children: Vec<Node> = Vec::new();
    let mut html_attrs = AttrMap::new();
    for c in doc.children {
        if c.tag() == Some("html") {
            if let dom::NodeType::Element(e) = &c.node_type {
                html_attrs = e.attrs.clone();
            }
            html_children.extend(c.children);
        } else {
            html_children.push(c);
        }
    }
    let head_tags = ["title", "meta", "link", "style", "script", "base"];
    let mut head: Option<Node> = None;
    let mut body: Option<Node> = None;
    let mut loose_head = Vec::new();
    let mut loose_body = Vec::new();
    for c in html_children {
        match c.tag() {
            Some("head") if head.is_none() => head = Some(c),
            Some("body") if body.is_none() => body = Some(c),
            Some(t) if head_tags.contains(&t) && body.is_none() && loose_body.is_empty() => {
                loose_head.push(c)
            }
            None if body.is_none() && loose_body.is_empty() && c.text_content().trim().is_empty() => {}
            _ => loose_body.push(c),
        }
    }
    let mut head = head.unwrap_or_else(|| dom::elem("head".into(), AttrMap::new(), vec![]));
    head.children.extend(loose_head);
    let mut body = body.unwrap_or_else(|| dom::elem("body".into(), AttrMap::new(), vec![]));
    body.children.extend(loose_body);
    dom::elem("html".into(), html_attrs, vec![head, body])
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
            .map(|e| e + 1)
            .unwrap_or(rest.len());
        let name = &rest[1..end];
        let decoded = if let Some(num) = name.strip_prefix('#') {
            let n = if let Some(hex) = num.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                num.parse().ok()
            };
            n.and_then(char::from_u32)
        } else {
            named_entity(name)
        };
        match decoded {
            Some(c) if end > 1 => {
                out.push(c);
                rest = &rest[end..];
                if rest.starts_with(';') {
                    rest = &rest[1..];
                }
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "times" => '×',
        "deg" => '°',
        "euro" => '€',
        "pound" => '£',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn comments_are_kept_but_not_text() {
        let d = parse("<p>a<!-- one -->b<!---->c<!-->d</p>");
        let p = d.children[1].children[0].clone();
        let kinds: Vec<String> = p
            .children
            .iter()
            .map(|c| match &c.node_type {
                crate::dom::NodeType::Text(t) => format!("t:{t}"),
                crate::dom::NodeType::Comment(t) => format!("c:{t}"),
                crate::dom::NodeType::Element(_) => "e".into(),
            })
            .collect();
        assert_eq!(kinds, ["t:a", "c: one ", "t:b", "c:", "t:c", "c:", "t:d"]);
        assert_eq!(p.text_content(), "abcd");
    }

    use super::*;

    fn body(src: &str) -> String {
        let doc = parse(src);
        let mut s = String::new();
        doc.children[1].dump(0, &mut s);
        s
    }

    #[test]
    fn implicit_p_and_li_closing() {
        let s = body("<p>one<p>two<ul><li>a<li>b</ul>");
        assert_eq!(
            s,
            "<body>\n  <p>\n    \"one\"\n  <p>\n    \"two\"\n  <ul>\n    <li>\n      \"a\"\n    <li>\n      \"b\"\n"
        );
    }

    #[test]
    fn void_and_entities() {
        let s = body("a&amp;b<br>c &#x41;&lt;");
        assert_eq!(s, "<body>\n  \"a&b\"\n  <br>\n  \"c A<\"\n");
    }

    #[test]
    fn nesting_and_size_are_capped() {
        fn depth(n: &Node) -> usize {
            1 + n.children.iter().map(depth).max().unwrap_or(0)
        }
        let doc = parse(&format!("{}deep", "<div>".repeat(100_000)));
        assert!(depth(&doc) <= MAX_DEPTH + 3);
        assert!(doc.text_content().contains("deep"));

        let doc = parse(&"<b>x</b>".repeat(MAX_NODES));
        let mut all = Vec::new();
        doc.walk(&mut all, &|_| true);
        assert!(all.len() <= MAX_NODES + 4);
    }

    #[test]
    fn closing_the_document_root_terminates() {
        // Found by fuzzing: this used to loop forever.
        let doc = parse("<p>a</#document>b");
        assert_eq!(doc.children[1].text_content(), "ab");
    }

    #[test]
    fn unterminated_attribute_at_eof() {
        // Found by fuzzing: used to index past the end of the input.
        parse("<h oref=\".1>text");
        parse("<a href='x");
        parse("<p class=\"");
    }

    #[test]
    fn raw_text_script() {
        let doc = parse("<script>if (a < b) { x = '</p>'; }</script><p>hi");
        assert_eq!(
            doc.children[0].children[0].text_content(),
            "if (a < b) { x = '</p>'; }"
        );
    }
}
