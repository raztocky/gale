//! Selector matching, the cascade, inheritance and computed values.

use crate::css::{self, Color, Declaration, PseudoElement, Stylesheet, Unit, Value};
use crate::dom::{self, ElementData, Node, NodeType};
use crate::selectors::{self, Flat};
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
    InlineFlex,
    Table,
    InlineTable,
    /// `thead`, `tbody`, `tfoot` and their `display` equivalents.
    TableRowGroup,
    TableRow,
    TableCell,
    TableCaption,
    None,
}

/// Where flex items go along a line (`justify-content`) or where lines go
/// in their container (`align-content`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Justify {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    /// `align-content` only: lines share the extra space.
    Stretch,
}

/// Where a flex item goes across its line (`align-items`, `align-self`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    Stretch,
    Start,
    End,
    Center,
}

/// `background-size`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
    /// Width and height; `Auto` in one keeps the image's proportions.
    Len(Len, Len),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    /// Laid out like `static` (pages aren't scrolled within), but still a
    /// containing block for absolutely positioned descendants.
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// Which bundled family stands in for a `font-family` name (lowercase);
/// None for names we know nothing about, so the next one in the list is
/// tried.
pub(crate) fn family_of(name: &str) -> Option<Family> {
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["mono", "courier", "consol", "menlo", "monaco", "code"]) {
        Some(Family::Mono)
    } else if has(&[
        "system-ui",
        "-apple-system",
        "blinkmacsystemfont",
        "segoe",
        "roboto",
        "inter",
        "ubuntu",
        "cantarell",
    ]) || name == "ui-sans-serif"
    {
        Some(Family::System)
    } else if has(&[
        "serif",
        "times",
        "georgia",
        "garamond",
        "cambria",
        "palatino",
        "baskerville",
    ]) && !has(&["sans"])
    {
        Some(Family::Serif)
    } else if has(&["sans", "arial", "helvetica", "verdana", "tahoma", "trebuchet"]) {
        Some(Family::Sans)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Family {
    /// Arial and Helvetica's widths.
    Sans,
    /// Times New Roman's.
    Serif,
    /// Courier New's.
    Mono,
    /// Modern interface fonts: `system-ui`, Segoe UI, San Francisco, Roboto.
    System,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Len {
    Px(f32),
    Percent(f32),
    /// `calc()`: px plus a percentage of the container.
    Calc(f32, f32),
    Auto,
}

impl Len {
    pub fn resolve(self, container: f32) -> Option<f32> {
        match self {
            Len::Px(v) => Some(v),
            Len::Percent(p) => Some(container * p / 100.0),
            Len::Calc(px, p) => Some(px + container * p / 100.0),
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
    /// `<summary>`'s triangle, pointing right (closed) or down (open).
    DisclosureClosed,
    DisclosureOpen,
    None,
}

/// Fully computed style for one element. Edges are [top, right, bottom, left].
#[derive(Debug, Clone)]
pub struct ComputedStyle {
    pub display: Display,
    pub color: Color,
    pub background: Color,
    /// `background-image: url(...)`, as written (absolute if it came from
    /// an external stylesheet).
    pub background_image: Option<String>,
    /// `background-image: linear-gradient(...)` and the like, drawn instead
    /// of an image.
    pub background_gradient: Option<Rc<crate::gradient::Gradient>>,
    /// Repeats across, and down.
    pub background_repeat: (bool, bool),
    pub background_size: BgSize,
    /// `background-position`: x, then y.
    pub background_position: [Len; 2],
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
    /// `box-sizing: border-box`: `width`, `height` and their limits include
    /// padding and border.
    pub border_box: bool,
    pub min_height: Len,
    pub max_width: Len,
    pub visible: bool,
    pub flex_row: bool,
    /// `row-reverse` or `column-reverse`.
    pub flex_reverse: bool,
    pub flex_wrap: bool,
    pub justify: Justify,
    pub align_items: Align,
    /// `None` is `auto`: the container's `align-items`.
    pub align_self: Option<Align>,
    pub align_content: Justify,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    /// `Auto` takes the item's width (or height), else its content's size.
    pub flex_basis: Len,
    pub order: i32,
    /// `overflow: auto | scroll`. Brickle doesn't scroll inside boxes, so
    /// these grow to fit their content instead (the page scrolls).
    pub scrolls: bool,
    /// The root element, and whether its `overflow` was set (then `body`'s
    /// isn't the window's).
    pub root: bool,
    pub root_overflow: bool,
    /// `aspect-ratio` (width / height) for boxes whose height is `auto`.
    pub aspect_ratio: Option<f32>,
    /// `display: grid`: laid out as a grid of [`ComputedStyle::grid_columns`]
    /// (its items are built like a flex container's, so `display` is `Flex`).
    pub is_grid: bool,
    pub grid_columns: Option<Rc<GridTemplate>>,
    /// A grid item's `grid-column` start and end.
    pub grid_column: [GridLine; 2],
    /// `column-gap`: between items in a row, or between columns.
    pub gap: f32,
    pub row_gap: f32,
    /// Horizontal and vertical `border-spacing` between table cells.
    pub border_spacing: [f32; 2],
    pub border_collapse: bool,
    /// Only used for table cells.
    pub vertical_align: VAlign,
    /// Custom properties (`--name: value`), inherited, with any `var()`
    /// in them already substituted.
    pub vars: Rc<HashMap<String, Vec<Value>>>,
    pub float: Float,
    pub clear: Clear,
    /// `overflow` other than `visible`, or `display: flow-root`: the box
    /// contains its floats and doesn't wrap around outside ones.
    pub contain_floats: bool,
    /// `overflow` other than `visible`: a real browser clips the content to
    /// the padding box. (Brickle doesn't clip painting yet, but text outside
    /// isn't offered for copying.)
    pub overflow_clip: bool,
    pub position: Position,
    /// `top`, `right`, `bottom`, `left`.
    pub inset: [Len; 4],
    /// `None` is `auto`.
    pub z_index: Option<i32>,
    /// A `::before`/`::after` box's `content`, as text. `None` (`normal`,
    /// `none`, or not set) means there's no such box.
    pub content: Option<Rc<str>>,
}

impl ComputedStyle {
    pub fn root(viewport: (f32, f32)) -> ComputedStyle {
        let _ = viewport;
        ComputedStyle {
            display: Display::Block,
            color: Color::BLACK,
            background: Color::TRANSPARENT,
            background_image: None,
            background_gradient: None,
            background_repeat: (true, true),
            background_size: BgSize::Auto,
            background_position: [Len::Percent(0.0); 2],
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
            border_box: false,
            height: Len::Auto,
            min_height: Len::Auto,
            max_width: Len::Auto,
            visible: true,
            flex_row: true,
            flex_reverse: false,
            flex_wrap: false,
            justify: Justify::Start,
            align_items: Align::Stretch,
            align_self: None,
            align_content: Justify::Stretch,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Len::Auto,
            order: 0,
            scrolls: false,
            root: false,
            root_overflow: false,
            aspect_ratio: None,
            is_grid: false,
            grid_columns: None,
            grid_column: [GridLine::Auto; 2],
            gap: 0.0,
            row_gap: 0.0,
            border_spacing: [0.0; 2],
            border_collapse: false,
            vertical_align: VAlign::Middle,
            vars: Rc::default(),
            float: Float::None,
            clear: Clear::None,
            contain_floats: false,
            overflow_clip: false,
            position: Position::Static,
            inset: [Len::Auto; 4],
            z_index: None,
            content: None,
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
        s.border_spacing = self.border_spacing;
        s.vars = self.vars.clone();
        s.border_collapse = self.border_collapse;
        s.display = Display::Inline;
        s
    }
}

/// One size in a grid track: the smallest or largest a column may be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackSize {
    Px(f32),
    Percent(f32),
    /// A share of the space left (`1fr`); only as a largest size.
    Fr(f32),
    /// Sized by the content (`auto`, `min-content`, `max-content`).
    Auto,
}

/// A column: `200px` is 200px at both ends, `1fr` is `minmax(auto, 1fr)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub min: TrackSize,
    pub max: TrackSize,
}

/// `grid-template-columns`.
#[derive(Debug, Clone, PartialEq)]
pub struct GridTemplate {
    pub tracks: Vec<Track>,
    /// `repeat(auto-fill | auto-fit, ...)`: tracks repeated as often as they
    /// fit, inserted before `tracks[at]`. `fit` collapses the empty ones.
    pub repeat: Option<AutoRepeat>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoRepeat {
    pub tracks: Vec<Track>,
    pub at: usize,
    pub fit: bool,
}

/// Most columns a template may ask for.
pub const MAX_GRID_TRACKS: usize = 1000;

impl GridTemplate {
    /// Parse the value (`none` or nonsense gives `None`).
    fn parse(text: &str, font_size: f32, rem: f32) -> Option<GridTemplate> {
        let mut t = GridTemplate {
            tracks: Vec::new(),
            repeat: None,
        };
        for tok in grid_tokens(text) {
            if let Some(args) = tok.strip_prefix("repeat(").and_then(|a| a.strip_suffix(')')) {
                let (count, rest) = args.split_once(',')?;
                let inner: Vec<Track> = grid_tokens(rest)
                    .iter()
                    .map(|t| Track::parse(t, font_size, rem))
                    .collect::<Option<_>>()?;
                if inner.is_empty() {
                    return None;
                }
                match count.trim() {
                    c @ ("auto-fill" | "auto-fit") if t.repeat.is_none() => {
                        t.repeat = Some(AutoRepeat {
                            tracks: inner,
                            at: t.tracks.len(),
                            fit: c == "auto-fit",
                        })
                    }
                    c => {
                        let n: usize = c.parse().ok().filter(|&n| n > 0)?;
                        for _ in 0..n {
                            if t.tracks.len() + inner.len() > MAX_GRID_TRACKS {
                                return None;
                            }
                            t.tracks.extend(&inner);
                        }
                    }
                }
            } else if tok.starts_with('[') {
                // Line names.
            } else {
                t.tracks.push(Track::parse(&tok, font_size, rem)?);
            }
            if t.tracks.len() > MAX_GRID_TRACKS {
                return None;
            }
        }
        (!t.tracks.is_empty() || t.repeat.is_some()).then_some(t)
    }
}

/// Split a track list at spaces outside parentheses and brackets.
fn grid_tokens(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in text.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        if (c.is_whitespace() || c == ',') && depth <= 0 {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else if !c.is_whitespace() || depth > 0 {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Track {
    fn parse(tok: &str, font_size: f32, rem: f32) -> Option<Track> {
        let tok = tok.trim();
        if let Some(args) = tok.strip_prefix("minmax(").and_then(|a| a.strip_suffix(')')) {
            let (a, b) = args.split_once(',')?;
            let min = match track_size(a, font_size, rem)? {
                TrackSize::Fr(_) => TrackSize::Auto,
                m => m,
            };
            return Some(Track {
                min,
                max: track_size(b, font_size, rem)?,
            });
        }
        if let Some(arg) = tok.strip_prefix("fit-content(").and_then(|a| a.strip_suffix(')')) {
            return Some(Track {
                min: TrackSize::Auto,
                max: track_size(arg, font_size, rem)?,
            });
        }
        let size = track_size(tok, font_size, rem)?;
        Some(match size {
            TrackSize::Fr(_) => Track {
                min: TrackSize::Auto,
                max: size,
            },
            _ => Track { min: size, max: size },
        })
    }
}

fn track_size(tok: &str, font_size: f32, rem: f32) -> Option<TrackSize> {
    let tok = tok.trim();
    if matches!(tok, "auto" | "min-content" | "max-content") {
        return Some(TrackSize::Auto);
    }
    let end = tok
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(tok.len());
    let n: f32 = tok[..end]
        .parse()
        .ok()
        .filter(|n: &f32| n.is_finite() && *n >= 0.0)?;
    Some(match &tok[end..] {
        "px" => TrackSize::Px(n),
        "" if n == 0.0 => TrackSize::Px(0.0),
        "fr" => TrackSize::Fr(n),
        "%" => TrackSize::Percent(n),
        "em" => TrackSize::Px(n * font_size),
        "rem" => TrackSize::Px(n * rem),
        "pt" => TrackSize::Px(n * 4.0 / 3.0),
        _ => return None,
    })
}

/// A `grid-column` start or end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridLine {
    Auto,
    /// A line number: 1 is the first, -1 the last.
    Line(i32),
    Span(u32),
}

impl GridLine {
    /// The lines in a `grid-column` value (`2 / span 3` arrives as `2 span 3`:
    /// slashes are dropped).
    fn parse(vals: &[Value]) -> Vec<GridLine> {
        let mut out = Vec::new();
        let mut span = false;
        for v in vals {
            match v {
                Value::Keyword(k) if k == "span" => span = true,
                Value::Keyword(k) if k == "auto" => out.push(GridLine::Auto),
                Value::Number(n) if n.is_finite() => {
                    let n = n.clamp(-1000.0, 1000.0) as i32;
                    out.push(if span {
                        GridLine::Span(n.max(1) as u32)
                    } else if n == 0 {
                        GridLine::Auto
                    } else {
                        GridLine::Line(n)
                    });
                    span = false;
                }
                _ => {}
            }
        }
        if span {
            out.push(GridLine::Span(1));
        }
        out
    }
}

pub struct StyledNode<'a> {
    pub node: NodeRef<'a>,
    pub style: Rc<ComputedStyle>,
    pub children: Vec<StyledNode<'a>>,
}

/// A styled node's DOM node: one in the document, or a `::before`/`::after`
/// box made while styling. Generated boxes are elements named `::before` or
/// `::after` holding their text; they carry their element's `uid`, so a
/// click on one is a click on the element. Scripts never see them.
pub enum NodeRef<'a> {
    Doc(&'a Node),
    Generated(Box<Node>),
}

impl std::ops::Deref for NodeRef<'_> {
    type Target = Node;
    fn deref(&self) -> &Node {
        match self {
            NodeRef::Doc(n) => n,
            NodeRef::Generated(n) => n,
        }
    }
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
    /// Your text size: absolute font sizes and line heights (px, pt, the
    /// size keywords, rem) are multiplied by it; relative ones follow.
    pub text_scale: f32,
}

/// Smallest and largest text size, as a multiple of the page's own.
pub const MIN_TEXT_SCALE: f32 = 0.5;
pub const MAX_TEXT_SCALE: f32 = 3.0;

fn text_scale(ctx: &StyleContext) -> f32 {
    if ctx.text_scale.is_finite() {
        ctx.text_scale.clamp(MIN_TEXT_SCALE, MAX_TEXT_SCALE)
    } else {
        1.0
    }
}

pub fn style_tree<'a>(root: &'a Node, ctx: &StyleContext) -> StyledNode<'a> {
    let mut base = ComputedStyle::root(ctx.viewport);
    let k = text_scale(ctx);
    base.font_size *= k;
    base.line_height *= k;
    let mut ancestors = Vec::new();
    let index = RuleIndex::new(&ctx.sheets, ctx.viewport, false);
    let pseudo = RuleIndex::new(&ctx.sheets, ctx.viewport, true);
    let flat = Flat::new(root);
    selectors::reset_budget();
    let mut next_id = 0;
    style_node(
        root,
        &base,
        &mut ancestors,
        ctx,
        &Indexes {
            elements: &index,
            pseudo: (!pseudo.entries.is_empty()).then_some(&pseudo),
        },
        &flat,
        &mut next_id,
        true,
    )
}

struct Indexes<'i, 's> {
    elements: &'i RuleIndex<'s>,
    /// Rules for `::before` and `::after`, if there are any.
    pseudo: Option<&'i RuleIndex<'s>>,
}

/// Elements that never get `::before`/`::after` boxes: replaced elements and
/// form controls (as in browsers), and ones that aren't drawn.
const NO_GENERATED: &[&str] = &[
    "img", "input", "br", "hr", "select", "textarea", "iframe", "video", "audio", "canvas", "embed",
    "object", "svg", "picture", "meter", "progress", "script", "style", "head", "title", "meta", "link",
    "template", "wbr",
];

/// Most characters of generated text in one box.
const MAX_CONTENT: usize = 1000;

/// Text for `content`, or `None` when there's no box. Strings, `attr()` and
/// quotes are supported; counters and images give nothing. Icon-font
/// characters (private use) are left out: Brickle doesn't load web fonts,
/// so they'd draw as boxes.
fn content_text(values: &[Value], e: &ElementData) -> Option<Rc<str>> {
    let mut out = String::new();
    for v in values {
        match v {
            Value::Keyword(k) if k == "none" || k == "normal" => return None,
            Value::Str(t) => out.push_str(t),
            Value::Keyword(k) => {
                if let Some(name) = k.strip_prefix("attr(").and_then(|a| a.strip_suffix(')')) {
                    // `attr(name)`, maybe with a type and fallback after it.
                    let name = name.split([' ', ',']).next().unwrap_or("").trim();
                    if let Some(a) = e.attrs.get(name) {
                        out.push_str(a);
                    }
                } else if k == "open-quote" {
                    out.push('\u{201C}');
                } else if k == "close-quote" {
                    out.push('\u{201D}');
                }
            }
            _ => {}
        }
    }
    let private = |c: char| matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF);
    Some(
        out.chars()
            .filter(|&c| !private(c))
            .take(MAX_CONTENT)
            .collect::<String>()
            .into(),
    )
}

/// Selectors bucketed by their subject's id, first class or tag, so each
/// element is only tested against selectors that could possibly match it.
/// (Content-blocker lists add over ten thousand hiding selectors.)
struct RuleIndex<'s> {
    /// (rule order, origin, rule, selector)
    entries: Vec<(usize, u8, &'s css::Rule, &'s css::Selector)>,
    by_id: HashMap<&'s str, Vec<usize>>,
    by_class: HashMap<&'s str, Vec<usize>>,
    by_tag: HashMap<&'s str, Vec<usize>>,
    any: Vec<usize>,
}

impl<'s> RuleIndex<'s> {
    /// Rules whose `@media` doesn't match `viewport` are left out. With
    /// `pseudo`, only the selectors for `::before`/`::after`; else the rest.
    fn new(sheets: &[(u8, &'s Stylesheet)], viewport: (f32, f32), pseudo: bool) -> Self {
        let mut ix = RuleIndex {
            entries: Vec::new(),
            by_id: HashMap::new(),
            by_class: HashMap::new(),
            by_tag: HashMap::new(),
            any: Vec::new(),
        };
        let mut order = 0;
        for (origin, sheet) in sheets {
            for rule in &sheet.rules {
                if !rule.applies(viewport) {
                    order += 1;
                    continue;
                }
                for sel in &rule.selectors {
                    if sel.parts[0].0.pseudo_element.is_some() != pseudo {
                        continue;
                    }
                    let i = ix.entries.len();
                    ix.entries.push((order, *origin, rule, sel));
                    let subject = &sel.parts[0].0;
                    if let Some(id) = &subject.id {
                        ix.by_id.entry(id.as_str()).or_default().push(i);
                    } else if let Some(c) = subject.classes.first() {
                        ix.by_class.entry(c.as_str()).or_default().push(i);
                    } else if let Some(t) = &subject.tag {
                        ix.by_tag.entry(t.as_str()).or_default().push(i);
                    } else {
                        ix.any.push(i);
                    }
                }
                order += 1;
            }
        }
        ix
    }

    fn candidates(&self, e: &ElementData) -> Vec<usize> {
        let mut out: Vec<usize> = self.any.clone();
        if let Some(v) = e.id().and_then(|id| self.by_id.get(id)) {
            out.extend(v);
        }
        for c in e.classes() {
            if let Some(v) = self.by_class.get(c) {
                out.extend(v);
            }
        }
        if let Some(v) = self.by_tag.get(e.tag.as_str()) {
            out.extend(v);
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// `id` is `node`'s id in `flat`; `next_id` counts nodes in tree order, as
/// `Flat` numbers them.
#[allow(clippy::too_many_arguments)]
fn style_node<'a>(
    node: &'a Node,
    parent: &ComputedStyle,
    ancestors: &mut Vec<&'a ElementData>,
    ctx: &StyleContext,
    index: &Indexes,
    flat: &Flat,
    next_id: &mut u32,
    is_root: bool,
) -> StyledNode<'a> {
    let id = *next_id;
    *next_id += 1;
    let style = match &node.node_type {
        NodeType::Text(_) => text_style(parent),
        NodeType::Element(e) => compute(
            e,
            parent,
            ancestors,
            ctx,
            index.elements,
            (flat, id),
            is_root,
            None,
        ),
    };
    let style = Rc::new(style);
    let mut children = Vec::new();
    if let NodeType::Element(e) = &node.node_type {
        // `::before` and `::after` boxes, if the element is drawn and some
        // rule gives them content.
        let generated = |pe: PseudoElement, ancestors: &[&ElementData]| -> Option<StyledNode<'a>> {
            let pseudo = index.pseudo?;
            if style.display == Display::None || NO_GENERATED.contains(&e.tag.as_str()) {
                return None;
            }
            let ps = compute(e, &style, ancestors, ctx, pseudo, (flat, id), false, Some(pe));
            let text = ps.content.clone()?;
            if ps.display == Display::None {
                return None;
            }
            let name = match pe {
                PseudoElement::Before => "::before",
                PseudoElement::After => "::after",
            };
            let mut t = dom::text(text.to_string());
            t.uid = node.uid;
            let mut g = dom::elem(name.into(), Default::default(), vec![t]);
            g.uid = node.uid;
            let ps = Rc::new(ps);
            let text_node = StyledNode {
                node: NodeRef::Generated(Box::new(g.children[0].clone())),
                style: Rc::new(text_style(&ps)),
                children: Vec::new(),
            };
            Some(StyledNode {
                node: NodeRef::Generated(Box::new(g)),
                style: ps,
                children: if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text_node]
                },
            })
        };
        // Matched with the element itself as the subject, so with its own
        // ancestors.
        let before = generated(PseudoElement::Before, ancestors);
        let summary = summary_marker(node, e, &style, before.is_some());
        children.extend(before.or(summary));
        // A closed `<details>` shows only its summary (whatever the page's
        // CSS says, as in browsers).
        let first_summary =
            (e.tag == "details").then(|| node.children.iter().position(|c| c.tag() == Some("summary")));
        let closed = e.tag == "details" && !e.attrs.contains_key("open");
        ancestors.push(e);
        for (i, c) in node.children.iter().enumerate() {
            let styled = style_node(c, &style, ancestors, ctx, index, flat, next_id, false);
            if !closed || first_summary == Some(Some(i)) {
                children.push(styled);
            }
        }
        ancestors.pop();
        children.extend(generated(PseudoElement::After, ancestors));
    }
    StyledNode {
        node: NodeRef::Doc(node),
        style,
        children,
    }
}

/// The triangle before a `<summary>`'s text (when the page doesn't give it
/// a `::before` of its own or `list-style: none`): an inline box like a
/// `::before`.
fn summary_marker<'a>(
    node: &Node,
    e: &ElementData,
    style: &Rc<ComputedStyle>,
    has_before: bool,
) -> Option<StyledNode<'a>> {
    let mark = match style.list_style {
        ListStyle::DisclosureClosed => "\u{25B8}\u{A0}",
        ListStyle::DisclosureOpen => "\u{25BE}\u{A0}",
        _ => return None,
    };
    if e.tag != "summary" || has_before || style.display == Display::None {
        return None;
    }
    let mut t = dom::text(mark.into());
    t.uid = node.uid;
    let mut g = dom::elem("::before".into(), Default::default(), vec![]);
    g.uid = node.uid;
    let ps = Rc::new(text_style(style));
    Some(StyledNode {
        node: NodeRef::Generated(Box::new(g)),
        style: ps.clone(),
        children: vec![StyledNode {
            node: NodeRef::Generated(Box::new(t)),
            style: Rc::new(text_style(&ps)),
            children: Vec::new(),
        }],
    })
}

fn text_style(parent: &ComputedStyle) -> ComputedStyle {
    let mut s = parent.inherit();
    s.display = Display::Inline;
    s
}

/// `at` is the element in the document being matched against.
#[allow(clippy::too_many_arguments)]
fn compute(
    e: &ElementData,
    parent: &ComputedStyle,
    ancestors: &[&ElementData],
    ctx: &StyleContext,
    index: &RuleIndex,
    at: (&Flat, u32),
    is_root: bool,
    pseudo: Option<PseudoElement>,
) -> ComputedStyle {
    let mut matched: Vec<MatchedRule> = Vec::new();
    let mut order = 0;
    // Best specificity per matching rule, in stylesheet order.
    let mut hits: std::collections::BTreeMap<usize, (u8, &css::Rule, css::Specificity)> = Default::default();
    for i in index.candidates(e) {
        let (rule_order, origin, rule, sel) = index.entries[i];
        let hit = match pseudo {
            None => selectors::matches(at.0, at.1, sel),
            Some(pe) => selectors::matches_pseudo(at.0, at.1, sel, pe),
        };
        if hit {
            let spec = sel.specificity();
            hits.entry(rule_order)
                .and_modify(|h| h.2 = h.2.max(spec))
                .or_insert((origin, rule, spec));
        }
    }
    let _ = &ctx.sheets;
    for (origin, rule, spec) in hits.into_values() {
        for d in &rule.declarations {
            order += 1;
            matched.push(MatchedRule {
                key: (d.important, origin, spec, order),
                decl: d,
            });
        }
    }
    // A generated box has no attributes of its own.
    let inline = e
        .attrs
        .get("style")
        .filter(|_| pseudo.is_none())
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
                && !matches!(e.tag.as_str(), "img" | "table" | "td" | "th" | "tr" | "hr")
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
    let table = ancestors.iter().rev().find(|a| a.tag == "table");
    let px = |v: &str| {
        let v = v.trim();
        if v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty() {
            Some(format!("{v}px"))
        } else {
            None
        }
    };
    let mut hint = |name: &str, value: &str| {
        presentational.push(Declaration {
            name: name.into(),
            values: css::parse_values(value),
            important: false,
        })
    };
    if e.tag == "table" {
        if let Some(v) = e.attrs.get("cellspacing").and_then(|v| px(v)) {
            hint("border-spacing", &v);
        }
        if e.attrs
            .get("align")
            .is_some_and(|a| a.eq_ignore_ascii_case("center"))
        {
            hint("margin-left", "auto");
            hint("margin-right", "auto");
        }
    }
    if matches!(e.tag.as_str(), "img" | "table") {
        match e.attrs.get("align").map(|a| a.to_ascii_lowercase()).as_deref() {
            Some("left") => hint("float", "left"),
            Some("right") => hint("float", "right"),
            _ => {}
        }
    }
    if e.tag == "br" {
        match e.attrs.get("clear").map(|a| a.to_ascii_lowercase()).as_deref() {
            Some("all" | "both") => hint("clear", "both"),
            Some("left") => hint("clear", "left"),
            Some("right") => hint("clear", "right"),
            _ => {}
        }
    }
    if matches!(e.tag.as_str(), "td" | "th") {
        if let Some(v) = table.and_then(|t| t.attrs.get("cellpadding")).and_then(|v| px(v)) {
            hint("padding", &v);
        }
        if e.attrs.contains_key("nowrap") {
            hint("white-space", "nowrap");
        }
        // `valign` on the cell, else on its row.
        let row = ancestors.iter().rev().find(|a| a.tag == "tr");
        let valign = e
            .attrs
            .get("valign")
            .or_else(|| row.and_then(|r| r.attrs.get("valign")));
        if let Some(v) = valign {
            hint("vertical-align", &v.to_ascii_lowercase());
        }
    }
    if matches!(
        e.tag.as_str(),
        "td" | "th"
            | "tr"
            | "tbody"
            | "thead"
            | "tfoot"
            | "div"
            | "p"
            | "caption"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    ) {
        if let Some(a) = e.attrs.get("align") {
            let a = a.to_ascii_lowercase();
            if matches!(a.as_str(), "left" | "center" | "right") {
                hint("text-align", &a);
            }
        }
    }
    if e.attrs.contains_key("hidden") {
        presentational.push(Declaration {
            name: "display".into(),
            values: vec![Value::Keyword("none".into())],
            important: false,
        });
    }
    if pseudo.is_some() {
        presentational.clear();
    }
    for d in &presentational {
        matched.push(MatchedRule {
            key: (false, 1, (0, 0, 0), 0),
            decl: d,
        });
    }
    matched.sort_by_key(|m| m.key);

    // Custom properties first, in cascade order, each with `var()` in it
    // resolved against the ones before.
    let mut vars = parent.vars.clone();
    let customs: Vec<&Declaration> = matched
        .iter()
        .map(|m| m.decl)
        .filter(|d| d.name.starts_with("--"))
        .collect();
    if !customs.is_empty() {
        let mut map = (*vars).clone();
        for d in customs {
            match substitute(&d.values, &map) {
                Some(v) if map.len() < MAX_VARS || map.contains_key(&d.name) => {
                    map.insert(d.name.clone(), v);
                }
                _ => {
                    map.remove(&d.name);
                }
            }
        }
        vars = Rc::new(map);
    }

    let mut props: HashMap<String, Vec<Value>> = HashMap::new();
    for m in &matched {
        if m.decl.name.starts_with("--") {
            continue;
        }
        if !m.decl.values.iter().any(has_var) {
            expand(m.decl, &mut props);
        } else if let Some(values) = substitute(&m.decl.values, &vars) {
            // Before shorthands are expanded, so `margin: var(--m)` works.
            expand(
                &Declaration {
                    name: m.decl.name.clone(),
                    values,
                    important: m.decl.important,
                },
                &mut props,
            );
        }
    }

    let mut s = parent.inherit();
    s.vars = vars;
    s.display = Display::Inline;
    let scale = text_scale(ctx);
    let ctxv = Ctx {
        parent,
        viewport: ctx.viewport,
        rem: 16.0 * scale,
    };
    // Absolute sizes grow with your text size; em and % follow their parent.
    let absolute = |v: &Value| matches!(v, Value::Length(_, Unit::Px | Unit::Pt));

    // font-size first: em units in other properties depend on it.
    if let Some(v) = props.get("font-size").and_then(|v| v.first()) {
        s.font_size = match v {
            Value::Keyword(k) => match k.as_str() {
                "xx-small" => 9.0 * scale,
                "x-small" => 10.0 * scale,
                "small" => 13.0 * scale,
                "medium" => 16.0 * scale,
                "large" => 18.0 * scale,
                "x-large" => 24.0 * scale,
                "xx-large" => 32.0 * scale,
                "smaller" => parent.font_size / 1.2,
                "larger" => parent.font_size * 1.2,
                _ => parent.font_size,
            },
            Value::Percent(p) => parent.font_size * p / 100.0,
            v if absolute(v) => ctxv
                .length(v, parent.font_size)
                .map_or(parent.font_size, |px| px * scale),
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
                s.is_grid = matches!(kw, "grid" | "inline-grid");
                s.display = match kw {
                    "block" => Display::Block,
                    "grid" => Display::Flex,
                    "flow-root" => {
                        s.contain_floats = true;
                        Display::Block
                    }
                    "inline" => Display::Inline,
                    "inline-block" => Display::InlineBlock,
                    "inline-grid" => Display::InlineFlex,
                    "inline-flex" => Display::InlineFlex,
                    "list-item" => Display::ListItem,
                    "flex" => Display::Flex,
                    "table" => Display::Table,
                    "inline-table" => Display::InlineTable,
                    "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
                    "table-row" => Display::TableRow,
                    "table-cell" => Display::TableCell,
                    "table-caption" => Display::TableCaption,
                    // Columns only carry widths and backgrounds, which aren't supported yet.
                    "none" | "table-column" | "table-column-group" => Display::None,
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
            "background-image" => {
                // The first image or gradient (more layers aren't supported).
                let first = vals.iter().find_map(|v| match v {
                    Value::Url(u) if !u.is_empty() => Some((Some(u.clone()), None)),
                    Value::Keyword(k) if k.contains("gradient(") => {
                        Some((None, crate::gradient::parse(k, s.font_size).map(Rc::new)))
                    }
                    _ => None,
                });
                (s.background_image, s.background_gradient) = first.unwrap_or((None, None));
            }
            "background-repeat" => {
                let kws: Vec<&str> = vals
                    .iter()
                    .filter_map(|v| match v {
                        Value::Keyword(k) => Some(k.as_str()),
                        _ => None,
                    })
                    .collect();
                let rep = |k: &str| k != "no-repeat";
                s.background_repeat = match kws.as_slice() {
                    ["repeat-x"] => (true, false),
                    ["repeat-y"] => (false, true),
                    [k] => (rep(k), rep(k)),
                    [x, y, ..] => (rep(x), rep(y)),
                    [] => s.background_repeat,
                };
            }
            "background-size" => {
                s.background_size = match kw {
                    "cover" => BgSize::Cover,
                    "contain" => BgSize::Contain,
                    _ => {
                        let w = ctxv.len(vals.first(), fs);
                        let h = ctxv.len(vals.get(1), fs);
                        match (w, h) {
                            (Len::Auto, Len::Auto) => BgSize::Auto,
                            (w, h) => BgSize::Len(w, h),
                        }
                    }
                }
            }
            "background-position" => {
                // Keywords as percentages; `top`/`bottom` first means y.
                let pos = |v: &Value| match v {
                    Value::Keyword(k) => match k.as_str() {
                        "left" | "top" => Some(Len::Percent(0.0)),
                        "center" => Some(Len::Percent(50.0)),
                        "right" | "bottom" => Some(Len::Percent(100.0)),
                        _ => None,
                    },
                    v => match ctxv.len(Some(v), fs) {
                        Len::Auto => None,
                        l => Some(l),
                    },
                };
                let parts: Vec<(Len, bool)> = vals
                    .iter()
                    .filter_map(|v| {
                        let vertical = matches!(v, Value::Keyword(k) if k == "top" || k == "bottom");
                        pos(v).map(|l| (l, vertical))
                    })
                    .collect();
                s.background_position = match parts.as_slice() {
                    [(l, true)] => [Len::Percent(50.0), *l],
                    [(l, false)] => [*l, Len::Percent(50.0)],
                    [(a, true), (b, _)] => [*b, *a],
                    [(a, _), (b, _), ..] => [*a, *b],
                    [] => s.background_position,
                };
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
                    let fam = family_of(&name);
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
                    Some(v) if absolute(v) => ctxv.length(v, fs).map_or(s.line_height, |px| px * scale),
                    Some(v) => ctxv.length(v, fs).unwrap_or(s.line_height),
                    None => s.line_height,
                }
            }
            "box-sizing" => {
                s.border_box = match kw {
                    "border-box" => true,
                    "content-box" => false,
                    _ => s.border_box,
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
                    "disclosure-closed" => ListStyle::DisclosureClosed,
                    "disclosure-open" => ListStyle::DisclosureOpen,
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
            "flex-direction" => {
                s.flex_row = !kw.starts_with("column");
                s.flex_reverse = kw.ends_with("-reverse");
            }
            "flex-wrap" => s.flex_wrap = kw == "wrap" || kw == "wrap-reverse",
            "justify-content" => s.justify = justify(kw).unwrap_or(s.justify),
            "align-content" => s.align_content = justify(kw).unwrap_or(s.align_content),
            "align-items" => s.align_items = align(kw).unwrap_or(s.align_items),
            "align-self" => s.align_self = align(kw),
            "flex-grow" | "flex-shrink" => {
                if let Some(Value::Number(n)) = first {
                    let n = n.clamp(0.0, 1e6);
                    if name == "flex-grow" {
                        s.flex_grow = n;
                    } else {
                        s.flex_shrink = n;
                    }
                }
            }
            "flex-basis" => {
                s.flex_basis = match kw {
                    "auto" | "content" => Len::Auto,
                    _ => ctxv.len(first, fs),
                }
            }
            "order" => {
                if let Some(Value::Number(n)) = first {
                    s.order = n.clamp(-1e6, 1e6) as i32;
                }
            }
            "aspect-ratio" => {
                // `16 / 9` (the slash is dropped), `1.5` or `auto 4/3`.
                let n: Vec<f32> = vals
                    .iter()
                    .filter_map(|v| match v {
                        Value::Number(n) => Some(*n),
                        _ => None,
                    })
                    .collect();
                s.aspect_ratio = match n.as_slice() {
                    [a] => Some(*a),
                    [a, b, ..] if *b > 0.0 => Some(a / b),
                    _ => None,
                }
                .filter(|r| r.is_finite() && *r > 0.0)
                .map(|r| r.clamp(1e-3, 1e3));
            }
            "grid-template-columns" => {
                let text: Vec<String> = vals.iter().map(|v| v.to_css()).collect();
                s.grid_columns = GridTemplate::parse(&text.join(" "), fs, ctxv.rem).map(Rc::new);
            }
            "grid-column" | "grid-column-start" | "grid-column-end" => {
                let lines = GridLine::parse(vals);
                match name.as_str() {
                    "grid-column" => {
                        // `a / b`; `a` alone is `a / auto`, but a span stays one.
                        let at = |i: usize| lines.get(i).copied().unwrap_or(GridLine::Auto);
                        s.grid_column = [at(0), at(1)];
                    }
                    "grid-column-start" => {
                        s.grid_column[0] = lines.first().copied().unwrap_or(GridLine::Auto)
                    }
                    _ => s.grid_column[1] = lines.first().copied().unwrap_or(GridLine::Auto),
                }
            }
            "column-gap" => {
                if let Some(v) = first {
                    s.gap = ctxv.length(v, fs).unwrap_or(0.0)
                }
            }
            "row-gap" => {
                if let Some(v) = first {
                    s.row_gap = ctxv.length(v, fs).unwrap_or(0.0)
                }
            }
            "border-spacing" => {
                let mut ls = vals.iter().filter_map(|v| ctxv.length(v, fs));
                if let Some(h) = ls.next() {
                    s.border_spacing = [h, ls.next().unwrap_or(h)];
                }
            }
            "border-collapse" => s.border_collapse = kw == "collapse",
            "float" => {
                s.float = match kw {
                    "left" | "inline-start" => Float::Left,
                    "right" | "inline-end" => Float::Right,
                    _ => Float::None,
                }
            }
            "clear" => {
                s.clear = match kw {
                    "left" | "inline-start" => Clear::Left,
                    "right" | "inline-end" => Clear::Right,
                    "both" => Clear::Both,
                    _ => Clear::None,
                }
            }
            "position" => {
                s.position = match kw {
                    "relative" => Position::Relative,
                    "absolute" => Position::Absolute,
                    "fixed" => Position::Fixed,
                    "sticky" | "-webkit-sticky" => Position::Sticky,
                    _ => Position::Static,
                }
            }
            "top" => s.inset[0] = ctxv.len(first, fs),
            "right" => s.inset[1] = ctxv.len(first, fs),
            "bottom" => s.inset[2] = ctxv.len(first, fs),
            "left" => s.inset[3] = ctxv.len(first, fs),
            "z-index" => {
                s.z_index = match first {
                    Some(Value::Number(n)) => Some(n.clamp(-1e9, 1e9) as i32),
                    _ => None,
                }
            }
            "overflow" | "overflow-x" | "overflow-y" => {
                if matches!(kw, "hidden" | "auto" | "scroll" | "clip") {
                    s.contain_floats = true;
                    s.overflow_clip = true;
                    s.scrolls |= matches!(kw, "auto" | "scroll");
                }
            }
            "vertical-align" => {
                s.vertical_align = match kw {
                    "top" | "text-top" | "baseline" => VAlign::Top,
                    "bottom" | "text-bottom" => VAlign::Bottom,
                    "middle" => VAlign::Middle,
                    _ => s.vertical_align,
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
    // The root's `overflow` (or `body`'s, if the root has none) is the
    // window's: it doesn't clip the box.
    let propagated = if is_root {
        s.root = true;
        s.root_overflow = s.overflow_clip;
        true
    } else {
        pseudo.is_none() && e.tag == "body" && parent.root && !parent.root_overflow
    };
    if propagated {
        s.overflow_clip = false;
        s.scrolls = false;
    }
    if pseudo.is_some() {
        s.content = props.get("content").and_then(|v| content_text(v, e));
    }
    // Absolutely positioned boxes don't float, and both are block-level.
    if matches!(s.position, Position::Absolute | Position::Fixed) {
        s.float = Float::None;
    }
    if s.float != Float::None || matches!(s.position, Position::Absolute | Position::Fixed) {
        s.display = match s.display {
            Display::InlineTable => Display::Table,
            Display::InlineFlex => Display::Flex,
            Display::Table | Display::Flex | Display::ListItem | Display::None => s.display,
            _ => Display::Block,
        };
    }
    s
}

fn justify(kw: &str) -> Option<Justify> {
    Some(match kw {
        "normal" | "start" | "flex-start" | "left" | "baseline" => Justify::Start,
        "end" | "flex-end" | "right" => Justify::End,
        "center" => Justify::Center,
        "space-between" => Justify::SpaceBetween,
        "space-around" => Justify::SpaceAround,
        "space-evenly" => Justify::SpaceEvenly,
        "stretch" => Justify::Stretch,
        _ => return None,
    })
}

fn align(kw: &str) -> Option<Align> {
    Some(match kw {
        "normal" | "stretch" => Align::Stretch,
        "start" | "flex-start" | "self-start" | "baseline" | "first" | "left" => Align::Start,
        "end" | "flex-end" | "self-end" | "last" | "right" => Align::End,
        "center" => Align::Center,
        _ => return None,
    })
}

/// Most custom properties per element, values per substituted property,
/// and characters per `var()` expansion: nesting variables in variables
/// can't make values blow up.
const MAX_VARS: usize = 1024;
const MAX_VAR_VALUES: usize = 1024;
const MAX_VAR_TEXT: usize = 16 * 1024;

fn has_var(v: &Value) -> bool {
    matches!(v, Value::Keyword(k) if k.contains("var("))
}

/// `values` with each `var(--name[, fallback])` replaced by the variable's
/// value, or its fallback. `None` if one has neither, or it gets too big.
fn substitute(values: &[Value], vars: &HashMap<String, Vec<Value>>) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::Keyword(k) if has_var(v) => out.extend(css::parse_values(&replace_vars(k, vars, 0)?)),
            v => out.push(v.clone()),
        }
        if out.len() > MAX_VAR_VALUES {
            return None;
        }
    }
    Some(out)
}

/// `text` with its `var()`s written out.
fn replace_vars(text: &str, vars: &HashMap<String, Vec<Value>>, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("var(") {
        out.push_str(&rest[..at]);
        let inner_start = at + 4;
        // The matching `)`.
        let mut level = 1;
        let mut end = None;
        for (i, c) in rest[inner_start..].char_indices() {
            match c {
                '(' => level += 1,
                ')' => {
                    level -= 1;
                    if level == 0 {
                        end = Some(inner_start + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let inner = &rest[inner_start..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        match (vars.get(name), fallback) {
            (Some(v), _) => {
                let parts: Vec<String> = v.iter().map(Value::to_css).collect();
                out.push_str(&parts.join(" "));
            }
            (None, Some(f)) => out.push_str(&replace_vars(f, vars, depth + 1)?),
            (None, None) => return None,
        }
        if out.len() > MAX_VAR_TEXT {
            return None;
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// A `calc()` result: a plain number, or a length as px plus a percentage.
#[derive(Clone, Copy)]
enum Calc {
    Num(f32),
    Len(f32, f32),
}

/// Evaluates `calc()`, `min()`, `max()` and `clamp()` (as written, already
/// lowercased): `+ - * /`, parentheses, and the length units Brickle knows.
struct CalcParser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
    font_size: f32,
    viewport: (f32, f32),
}

impl CalcParser<'_> {
    fn skip_ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.skip_ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn sum(&mut self) -> Option<Calc> {
        let mut acc = self.product()?;
        loop {
            let neg = if self.eat(b'+') {
                false
            } else if self.eat(b'-') {
                true
            } else {
                return Some(acc);
            };
            let rhs = self.product()?;
            let k = if neg { -1.0 } else { 1.0 };
            acc = match (acc, rhs) {
                (Calc::Num(a), Calc::Num(b)) => Calc::Num(a + k * b),
                (Calc::Len(a, p), Calc::Len(b, q)) => Calc::Len(a + k * b, p + k * q),
                // `0` may stand for a length.
                (Calc::Len(a, p), Calc::Num(0.0)) => Calc::Len(a, p),
                (Calc::Num(0.0), Calc::Len(b, q)) => Calc::Len(k * b, k * q),
                _ => return None,
            };
        }
    }

    fn product(&mut self) -> Option<Calc> {
        let mut acc = self.unary()?;
        loop {
            let div = if self.eat(b'*') {
                false
            } else if self.eat(b'/') {
                true
            } else {
                return Some(acc);
            };
            let rhs = self.unary()?;
            acc = match (acc, rhs, div) {
                (Calc::Num(a), Calc::Num(b), false) => Calc::Num(a * b),
                (Calc::Num(a), Calc::Num(b), true) if b != 0.0 => Calc::Num(a / b),
                (Calc::Len(a, p), Calc::Num(b), false) | (Calc::Num(b), Calc::Len(a, p), false) => {
                    Calc::Len(a * b, p * b)
                }
                (Calc::Len(a, p), Calc::Num(b), true) if b != 0.0 => Calc::Len(a / b, p / b),
                _ => return None,
            };
        }
    }

    fn unary(&mut self) -> Option<Calc> {
        self.depth += 1;
        if self.depth > 32 {
            return None;
        }
        self.skip_ws();
        let r = if self.eat(b'(') {
            let v = self.sum()?;
            self.eat(b')').then_some(v)
        } else if self.eat(b'-') {
            match self.unary()? {
                Calc::Num(n) => Some(Calc::Num(-n)),
                Calc::Len(a, p) => Some(Calc::Len(-a, -p)),
            }
        } else if self.s[self.i..].first().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.function()
        } else {
            self.number()
        };
        self.depth -= 1;
        r
    }

    fn function(&mut self) -> Option<Calc> {
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'-')
        {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i]).ok()?.to_string();
        if !self.eat(b'(') {
            return None;
        }
        let mut args = vec![self.sum()?];
        while self.eat(b',') {
            args.push(self.sum()?);
        }
        if !self.eat(b')') {
            return None;
        }
        // With percentages, min/max can't be worked out yet: take the
        // preferred (middle) value of clamp(), or the first.
        let px = |c: &Calc| match c {
            Calc::Num(n) => Some(*n),
            Calc::Len(a, p) if *p == 0.0 => Some(*a),
            _ => None,
        };
        let all_px: Option<Vec<f32>> = args.iter().map(px).collect();
        let same_kind = |v: f32| match args[0] {
            Calc::Num(_) => Calc::Num(v),
            Calc::Len(..) => Calc::Len(v, 0.0),
        };
        match (name.as_str(), all_px) {
            ("calc", _) if args.len() == 1 => Some(args[0]),
            ("min", Some(v)) => Some(same_kind(v.into_iter().fold(f32::INFINITY, f32::min))),
            ("max", Some(v)) => Some(same_kind(v.into_iter().fold(f32::NEG_INFINITY, f32::max))),
            ("clamp", Some(v)) if v.len() == 3 => Some(same_kind(v[1].min(v[2]).max(v[0]))),
            ("clamp", None) if args.len() == 3 => Some(args[1]),
            ("min" | "max", None) => Some(args[0]),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<Calc> {
        self.skip_ws();
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
        {
            self.i += 1;
        }
        let n: f32 = std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()?;
        let ustart = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'%')
        {
            self.i += 1;
        }
        let fs = self.font_size;
        Some(match &self.s[ustart..self.i] {
            b"" => Calc::Num(n),
            b"%" => Calc::Len(0.0, n),
            b"px" => Calc::Len(n, 0.0),
            b"em" => Calc::Len(n * fs, 0.0),
            b"ex" | b"ch" => Calc::Len(n * fs * 0.5, 0.0),
            b"rem" => Calc::Len(n * 16.0, 0.0),
            b"pt" => Calc::Len(n * 4.0 / 3.0, 0.0),
            b"vw" => Calc::Len(n * self.viewport.0 / 100.0, 0.0),
            b"vh" => Calc::Len(n * self.viewport.1 / 100.0, 0.0),
            _ => return None,
        })
    }
}

/// A `calc()`-like value, as written, worked out.
fn eval_calc(text: &str, font_size: f32, viewport: (f32, f32)) -> Option<Calc> {
    if text.len() > 4096
        || !["calc(", "min(", "max(", "clamp("]
            .iter()
            .any(|f| text.starts_with(f))
    {
        return None;
    }
    let mut p = CalcParser {
        s: text.as_bytes(),
        i: 0,
        depth: 0,
        font_size,
        viewport,
    };
    let v = p.unary()?;
    p.skip_ws();
    let finite = match v {
        Calc::Num(n) => n.is_finite(),
        Calc::Len(a, b) => a.is_finite() && b.is_finite(),
    };
    (p.i == text.len() && finite).then_some(v)
}

struct Ctx<'p> {
    parent: &'p ComputedStyle,
    viewport: (f32, f32),
    /// 1rem in px.
    rem: f32,
}

impl Ctx<'_> {
    fn length(&self, v: &Value, font_size: f32) -> Option<f32> {
        let _ = self.parent;
        match v {
            Value::Length(n, u) => Some(match u {
                Unit::Px => *n,
                Unit::Em => n * font_size,
                Unit::Rem => n * self.rem,
                Unit::Pt => n * 4.0 / 3.0,
                Unit::Vw => n * self.viewport.0 / 100.0,
                Unit::Vh => n * self.viewport.1 / 100.0,
            }),
            Value::Number(n) if *n == 0.0 => Some(0.0),
            Value::Keyword(k) => match eval_calc(k, font_size, self.viewport)? {
                Calc::Len(px, 0.0) => Some(px),
                Calc::Num(0.0) => Some(0.0),
                _ => None,
            },
            _ => None,
        }
    }

    fn len(&self, v: Option<&Value>, font_size: f32) -> Len {
        match v {
            Some(Value::Percent(p)) => Len::Percent(*p),
            Some(Value::Keyword(k)) => match eval_calc(k, font_size, self.viewport) {
                Some(Calc::Len(px, 0.0)) => Len::Px(px),
                Some(Calc::Len(0.0, p)) => Len::Percent(p),
                Some(Calc::Len(px, p)) => Len::Calc(px, p),
                Some(Calc::Num(0.0)) => Len::Px(0.0),
                _ => Len::Auto,
            },
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
        "inset" => {
            for (i, v) in four(&d.values).into_iter().enumerate() {
                set(sides[i].to_string(), vec![v]);
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
            // The image and how it's laid out; the shorthand resets them.
            let kw = |v: &Value, ks: &[&str]| matches!(v, Value::Keyword(k) if ks.contains(&k.as_str()));
            let pick =
                |ks: &[&str]| -> Vec<Value> { d.values.iter().filter(|v| kw(v, ks)).cloned().collect() };
            let url: Vec<Value> = d
                .values
                .iter()
                .filter(|v| {
                    matches!(v, Value::Url(_)) || matches!(v, Value::Keyword(k) if k.contains("gradient("))
                })
                .take(1)
                .cloned()
                .collect();
            set("background-image".into(), url);
            let repeat = pick(&["repeat", "no-repeat", "repeat-x", "repeat-y", "space", "round"]);
            set(
                "background-repeat".into(),
                if repeat.is_empty() {
                    vec![Value::Keyword("repeat".into())]
                } else {
                    repeat
                },
            );
            let size = pick(&["cover", "contain"]);
            set(
                "background-size".into(),
                if size.is_empty() {
                    vec![Value::Keyword("auto".into())]
                } else {
                    size
                },
            );
            let position: Vec<Value> = d
                .values
                .iter()
                .filter(|v| {
                    kw(v, &["left", "right", "top", "bottom", "center"])
                        || matches!(v, Value::Length(..) | Value::Percent(_))
                })
                .take(2)
                .cloned()
                .collect();
            set(
                "background-position".into(),
                if position.is_empty() {
                    vec![Value::Percent(0.0), Value::Percent(0.0)]
                } else {
                    position
                },
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
            for v in &d.values {
                if let Value::Keyword(k) = v {
                    let prop = if k.contains("wrap") {
                        "flex-wrap"
                    } else {
                        "flex-direction"
                    };
                    set(prop.into(), vec![v.clone()]);
                }
            }
        }
        // `gap: <row> [<column>]`.
        "gap" => {
            let v: Vec<Value> = d.values.iter().filter(|v| **v != Value::Comma).cloned().collect();
            if let Some(row) = v.first() {
                set("row-gap".into(), vec![row.clone()]);
                set("column-gap".into(), vec![v.get(1).unwrap_or(row).clone()]);
            }
        }
        // `flex: none | auto | <grow> [<shrink>] [<basis>] | <basis>`.
        "flex" => {
            let kw = |v: &Value, k: &str| matches!(v, Value::Keyword(x) if x == k);
            let num = |n: f32| vec![Value::Number(n)];
            let (grow, shrink, basis) = match d.values.as_slice() {
                [v] if kw(v, "none") => (num(0.0), num(0.0), vec![Value::Keyword("auto".into())]),
                [v] if kw(v, "auto") => (num(1.0), num(1.0), vec![Value::Keyword("auto".into())]),
                [v] if kw(v, "initial") => (num(0.0), num(1.0), vec![Value::Keyword("auto".into())]),
                vals => {
                    let nums: Vec<f32> = vals
                        .iter()
                        .filter_map(|v| match v {
                            Value::Number(n) => Some(*n),
                            _ => None,
                        })
                        .collect();
                    let basis = vals
                        .iter()
                        .find(|v| !matches!(v, Value::Number(_)))
                        .cloned()
                        .unwrap_or(Value::Length(0.0, Unit::Px));
                    (
                        num(nums.first().copied().unwrap_or(1.0)),
                        num(nums.get(1).copied().unwrap_or(1.0)),
                        vec![basis],
                    )
                }
            };
            set("flex-grow".into(), grow);
            set("flex-shrink".into(), shrink);
            set("flex-basis".into(), basis);
        }
        _ => set(d.name.clone(), d.values.clone()),
    }
}

#[cfg(test)]
mod text_scale_tests {
    use super::*;

    /// The computed styles of `<p>`s in `body`, rendered at `scale`.
    fn paragraphs(body: &str, scale: f32) -> Vec<ComputedStyle> {
        let dom = crate::html::parse(body);
        let ua = css::parse(UA_CSS);
        let ctx = StyleContext {
            sheets: vec![(0, &ua)],
            viewport: (1000.0, 600.0),
            text_scale: scale,
        };
        let styled = style_tree(&dom, &ctx);
        fn walk(n: &StyledNode, out: &mut Vec<ComputedStyle>) {
            if n.node.tag() == Some("p") {
                out.push((*n.style).clone());
            }
            for c in &n.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(&styled, &mut out);
        out
    }

    #[test]
    fn absolute_sizes_grow_and_relative_ones_follow() {
        let body = "<p>plain</p><p style='font-size:20px;line-height:30px'>px</p>\
                    <div style='font-size:10px'><p style='font-size:2em'>em</p></div>\
                    <p style='font-size:large'>keyword</p><p style='font-size:2rem'>rem</p>\
                    <p style='font-size:2vw'>vw</p>";
        let one = paragraphs(body, 1.0);
        let big = paragraphs(body, 1.5);
        let sizes = |v: &[ComputedStyle]| v.iter().map(|s| s.font_size).collect::<Vec<_>>();
        assert_eq!(sizes(&one), [16.0, 20.0, 20.0, 18.0, 32.0, 20.0]);
        assert_eq!(sizes(&big), [24.0, 30.0, 30.0, 27.0, 48.0, 20.0], "vw stays put");
        assert_eq!(big[1].line_height, 45.0);
        // Nonsense scales are clamped or ignored.
        assert_eq!(paragraphs(body, 100.0)[0].font_size, 16.0 * MAX_TEXT_SCALE);
        assert_eq!(paragraphs(body, f32::NAN)[0].font_size, 16.0);
    }
}
