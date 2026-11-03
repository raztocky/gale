//! Layout: turns the styled tree into positioned boxes.
//!
//! Supports normal flow (block + inline formatting contexts), the CSS box
//! model, auto margins, max-width, sibling margin collapsing, text wrapping
//! and alignment, list markers, inline-blocks, a simple flexbox, floats,
//! positioning and tables (the CSS 2.1 automatic table layout, in the
//! separated-borders model).

use crate::css::Color;
use crate::dom::NodeType;
use crate::font;
use crate::image::Decoded;
use crate::style::{
    Align, Clear, ComputedStyle, Display, Float, Justify, Len, ListStyle, Position, StyledNode, TextAlign,
    VAlign,
};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn expand(self, e: Edges) -> Rect {
        Rect {
            x: self.x - e.l,
            y: self.y - e.t,
            w: self.w + e.l + e.r,
            h: self.h + e.t + e.b,
        }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    /// The overlap of two rectangles (zero-sized if they don't meet).
    pub fn intersect(self, o: Rect) -> Rect {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        Rect {
            x,
            y,
            w: ((self.x + self.w).min(o.x + o.w) - x).max(0.0),
            h: ((self.y + self.h).min(o.y + o.h) - y).max(0.0),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Edges {
    pub t: f32,
    pub r: f32,
    pub b: f32,
    pub l: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Dimensions {
    pub content: Rect,
    pub padding: Edges,
    pub border: Edges,
    pub margin: Edges,
}

impl Dimensions {
    pub fn padding_box(&self) -> Rect {
        self.content.expand(self.padding)
    }
    pub fn border_box(&self) -> Rect {
        self.padding_box().expand(self.border)
    }
    pub fn margin_box(&self) -> Rect {
        self.border_box().expand(self.margin)
    }
}

#[derive(Debug, Clone)]
pub struct TextFrag {
    pub x: f32,
    pub baseline: f32,
    pub w: f32,
    pub text: String,
    pub style: Rc<ComputedStyle>,
    pub bg: Color,
    pub link: Option<String>,
    /// The DOM node the text came from.
    pub uid: u32,
}

#[derive(Debug, Clone)]
pub enum InlineItem {
    Text {
        text: String,
        style: Rc<ComputedStyle>,
        bg: Color,
        link: Option<String>,
        uid: u32,
    },
    Break(Rc<ComputedStyle>),
    /// Index into the anonymous box's `children` (an inline-block).
    Atomic(usize),
    /// Index into the anonymous box's `children` (a float). Floats sit in
    /// the inline content they appear in, so lines can flow around them.
    Float(usize),
    /// Index into the anonymous box's `children` (an absolutely positioned
    /// box). It takes no space; where it appears is its static position.
    OutOfFlow(usize),
}

pub enum BoxKind<'a> {
    Block(&'a StyledNode<'a>),
    /// An anonymous block holding an inline formatting context.
    Anonymous {
        items: Vec<InlineItem>,
        frags: Vec<TextFrag>,
        style: Rc<ComputedStyle>,
    },
}

pub struct LayoutBox<'a> {
    pub dims: Dimensions,
    pub kind: BoxKind<'a>,
    pub children: Vec<LayoutBox<'a>>,
    pub marker: Option<String>,
    pub link: Option<String>,
    /// The DOM node this box is for (0 for anonymous boxes).
    pub uid: u32,
    /// A decoded `<img>`: the box is sized from it and paints it.
    pub image: Option<Decoded>,
}

impl<'a> LayoutBox<'a> {
    fn new(kind: BoxKind<'a>) -> Self {
        let kind_uid = match &kind {
            BoxKind::Block(n) => Some(n.node.uid),
            _ => None,
        };
        LayoutBox {
            dims: Dimensions::default(),
            kind,
            children: Vec::new(),
            marker: None,
            link: None,
            uid: kind_uid.unwrap_or(0),
            image: None,
        }
    }

    pub fn style(&self) -> &ComputedStyle {
        match &self.kind {
            BoxKind::Block(n) => &n.style,
            BoxKind::Anonymous { style, .. } => style,
        }
    }

    fn translate(&mut self, dx: f32, dy: f32) {
        self.dims.content.x += dx;
        self.dims.content.y += dy;
        if let BoxKind::Anonymous { frags, .. } = &mut self.kind {
            for f in frags {
                f.x += dx;
                f.baseline += dy;
            }
        }
        for c in &mut self.children {
            c.translate(dx, dy);
        }
    }

    fn position(&self) -> Position {
        match self.kind {
            BoxKind::Block(n) => n.style.position,
            BoxKind::Anonymous { .. } => Position::Static,
        }
    }

    /// Absolutely positioned (`absolute` or `fixed`).
    fn out_of_flow(&self) -> bool {
        matches!(self.position(), Position::Absolute | Position::Fixed)
    }

    fn is_float(&self) -> bool {
        matches!(self.kind, BoxKind::Block(n) if n.style.float != Float::None)
    }

    /// Move what's inside this box, but not the box itself.
    fn translate_contents(&mut self, dy: f32) {
        if let BoxKind::Anonymous { frags, .. } = &mut self.kind {
            for f in frags {
                f.baseline += dy;
            }
        }
        for c in &mut self.children {
            c.translate(0.0, dy);
        }
    }
}

// ---------------------------------------------------------------------------
// Box tree construction
// ---------------------------------------------------------------------------

pub fn build_layout_tree<'a>(root: &'a StyledNode<'a>) -> LayoutBox<'a> {
    build_block(root, None)
}

fn build_block<'a>(node: &'a StyledNode<'a>, link: Option<String>) -> LayoutBox<'a> {
    if matches!(node.style.display, Display::Table | Display::InlineTable) {
        return build_table(node, link);
    }
    let mut b = LayoutBox::new(BoxKind::Block(node));
    b.link = link.clone();
    let mut run: Option<LayoutBox<'a>> = None;
    let mut li_index = 0;
    for child in &node.children {
        let link = child_link(child, &link);
        match child.display() {
            Display::None => {}
            _ if out_of_flow(child) || (child.style.float != Float::None && !is_flex(node.style.display)) => {
                let anon = run.get_or_insert_with(|| {
                    LayoutBox::new(BoxKind::Anonymous {
                        items: Vec::new(),
                        frags: Vec::new(),
                        style: node.style.clone(),
                    })
                });
                flatten_inline(child, anon, Color::TRANSPARENT, link);
            }
            // Table parts outside a table are laid out as blocks.
            Display::Block
            | Display::ListItem
            | Display::Flex
            | Display::Table
            | Display::TableRowGroup
            | Display::TableRow
            | Display::TableCaption => {
                flush_run(&mut b, &mut run);
                let mut cb = build_block(child, link);
                if child.display() == Display::ListItem {
                    li_index += 1;
                    let start: i32 = node
                        .node
                        .element()
                        .and_then(|e| e.attrs.get("start"))
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                    cb.marker = match child.style.list_style {
                        ListStyle::Disc => Some("•".into()),
                        ListStyle::Circle => Some("◦".into()),
                        ListStyle::Square => Some("▪".into()),
                        ListStyle::Decimal => Some(format!("{}.", start + li_index - 1)),
                        ListStyle::None => None,
                    };
                }
                b.children.push(cb);
            }
            // Flex items are blockified: each element child is its own item.
            Display::Inline
            | Display::InlineBlock
            | Display::InlineFlex
            | Display::InlineTable
            | Display::TableCell
                if is_flex(node.style.display) && child.node.element().is_some() =>
            {
                flush_run(&mut b, &mut run);
                b.children.push(build_block(child, link));
            }
            // A cell outside a row is approximated as an inline-block.
            Display::Inline
            | Display::InlineBlock
            | Display::InlineFlex
            | Display::InlineTable
            | Display::TableCell => {
                let anon = run.get_or_insert_with(|| {
                    LayoutBox::new(BoxKind::Anonymous {
                        items: Vec::new(),
                        frags: Vec::new(),
                        style: node.style.clone(),
                    })
                });
                flatten_inline(child, anon, Color::TRANSPARENT, link);
            }
        }
    }
    flush_run(&mut b, &mut run);
    b
}

fn is_flex(d: Display) -> bool {
    matches!(d, Display::Flex | Display::InlineFlex)
}

fn out_of_flow(n: &StyledNode) -> bool {
    n.node.element().is_some() && matches!(n.style.position, Position::Absolute | Position::Fixed)
}

fn child_link(child: &StyledNode, inherited: &Option<String>) -> Option<String> {
    match child.node.element() {
        Some(e) if e.tag == "a" => e.attrs.get("href").cloned().or_else(|| inherited.clone()),
        _ => inherited.clone(),
    }
}

fn flush_run<'a>(parent: &mut LayoutBox<'a>, run: &mut Option<LayoutBox<'a>>) {
    if let Some(anon) = run.take() {
        let keep = match &anon.kind {
            BoxKind::Anonymous { items, .. } => items.iter().any(|i| match i {
                InlineItem::Text { text, style, .. } => style.pre || !is_collapsible_ws(text),
                _ => true,
            }),
            _ => true,
        };
        if keep {
            parent.children.push(anon);
        }
    }
}

fn is_collapsible_ws(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_whitespace())
}

fn flatten_inline<'a>(node: &'a StyledNode<'a>, anon: &mut LayoutBox<'a>, bg: Color, link: Option<String>) {
    let push = |anon: &mut LayoutBox<'a>, item: InlineItem| {
        if let BoxKind::Anonymous { items, .. } = &mut anon.kind {
            items.push(item);
        }
    };
    match &node.node.node_type {
        NodeType::Text(t) => {
            push(
                anon,
                InlineItem::Text {
                    text: t.clone(),
                    style: node.style.clone(),
                    bg,
                    link,
                    uid: node.node.uid,
                },
            );
        }
        NodeType::Element(e) => {
            if node.display() == Display::None {
                return;
            }
            if e.tag == "br" {
                push(anon, InlineItem::Break(node.style.clone()));
                return;
            }
            let float = node.style.float != Float::None;
            let oof = out_of_flow(node);
            if float
                || oof
                || matches!(
                    node.display(),
                    Display::InlineBlock | Display::InlineFlex | Display::InlineTable | Display::TableCell
                )
                || e.tag == "img"
            {
                let mut ib = build_block(node, link.clone());
                if e.tag == "img" && ib.children.is_empty() {
                    // No image decoding yet: show the alt text in a frame.
                    let alt = e.attrs.get("alt").cloned().unwrap_or_default();
                    if !alt.is_empty() {
                        let mut a = LayoutBox::new(BoxKind::Anonymous {
                            items: vec![InlineItem::Text {
                                text: alt,
                                style: node.style.clone(),
                                bg: Color::TRANSPARENT,
                                link: link.clone(),
                                uid: node.node.uid,
                            }],
                            frags: vec![],
                            style: node.style.clone(),
                        });
                        a.link = link;
                        ib.children.push(a);
                    }
                }
                let idx = anon.children.len();
                anon.children.push(ib);
                push(
                    anon,
                    if oof {
                        InlineItem::OutOfFlow(idx)
                    } else if float {
                        InlineItem::Float(idx)
                    } else {
                        InlineItem::Atomic(idx)
                    },
                );
                return;
            }
            let bg = if node.style.background.a > 0 {
                node.style.background
            } else {
                bg
            };
            for c in &node.children {
                let l = child_link(c, &link);
                match c.display() {
                    _ if c.style.float != Float::None || out_of_flow(c) => flatten_inline(c, anon, bg, l),
                    Display::Block
                    | Display::ListItem
                    | Display::Flex
                    | Display::Table
                    | Display::TableRowGroup
                    | Display::TableRow
                    | Display::TableCaption => {
                        // Block inside inline: approximate by breaking the line
                        // and laying the block out as an inline-block.
                        push(anon, InlineItem::Break(node.style.clone()));
                        let idx = anon.children.len();
                        anon.children.push(build_block(c, l));
                        push(anon, InlineItem::Atomic(idx));
                        push(anon, InlineItem::Break(node.style.clone()));
                    }
                    _ => flatten_inline(c, anon, bg, l),
                }
            }
        }
    }
}

/// A table's box: its children are captions, then row groups and rows, with
/// `<thead>` moved to the top and `<tfoot>` to the bottom. Stray cells and
/// other content are wrapped in anonymous rows, as CSS requires.
fn build_table<'a>(node: &'a StyledNode<'a>, link: Option<String>) -> LayoutBox<'a> {
    let mut t = LayoutBox::new(BoxKind::Block(node));
    t.link = link.clone();
    let (mut captions, mut head, mut body, mut foot) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for child in &node.children {
        let link = child_link(child, &link);
        match child.display() {
            Display::TableCaption => captions.push(build_block(child, link)),
            Display::TableRowGroup => {
                let mut g = LayoutBox::new(BoxKind::Block(child));
                g.link = link.clone();
                for c in &child.children {
                    push_row_content(&mut g.children, c, child_link(c, &link), &child.style);
                }
                match child.node.element().map(|e| e.tag.as_str()) {
                    Some("thead") if head.is_empty() => head.push(g),
                    Some("tfoot") if foot.is_empty() => foot.push(g),
                    _ => body.push(g),
                }
            }
            _ => push_row_content(&mut body, child, link, &node.style),
        }
    }
    t.children = captions;
    t.children.extend(head);
    t.children.extend(body);
    t.children.extend(foot);
    t
}

/// Add a child of a table or row group: a row, or a cell for an anonymous row.
fn push_row_content<'a>(
    rows: &mut Vec<LayoutBox<'a>>,
    child: &'a StyledNode<'a>,
    link: Option<String>,
    parent_style: &Rc<ComputedStyle>,
) {
    match child.display() {
        Display::None => {}
        Display::TableRow => {
            let mut r = LayoutBox::new(BoxKind::Block(child));
            r.link = link.clone();
            for c in &child.children {
                if let Some(cell) = build_cell(c, child_link(c, &link), &child.style) {
                    r.children.push(cell);
                }
            }
            rows.push(r);
        }
        _ => {
            let Some(cell) = build_cell(child, link, parent_style) else {
                return;
            };
            let anon_row = |b: &LayoutBox| matches!(b.kind, BoxKind::Anonymous { .. });
            if !rows.last().is_some_and(anon_row) {
                rows.push(LayoutBox::new(BoxKind::Anonymous {
                    items: Vec::new(),
                    frags: Vec::new(),
                    style: parent_style.clone(),
                }));
            }
            if let Some(r) = rows.last_mut() {
                r.children.push(cell);
            }
        }
    }
}

/// A child of a row as a cell. Stray text becomes an anonymous cell;
/// whitespace between cells is dropped.
fn build_cell<'a>(
    child: &'a StyledNode<'a>,
    link: Option<String>,
    parent_style: &Rc<ComputedStyle>,
) -> Option<LayoutBox<'a>> {
    match &child.node.node_type {
        NodeType::Text(t) if is_collapsible_ws(t) => None,
        NodeType::Text(_) => {
            let mut a = LayoutBox::new(BoxKind::Anonymous {
                items: Vec::new(),
                frags: Vec::new(),
                style: parent_style.clone(),
            });
            flatten_inline(child, &mut a, Color::TRANSPARENT, link);
            Some(a)
        }
        NodeType::Element(_) if child.display() == Display::None => None,
        NodeType::Element(_) => Some(build_block(child, link)),
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Lay out for a viewport `viewport_w` wide. Absolutely positioned boxes
/// with no positioned ancestor use the page's own height as the viewport's.
pub fn layout_tree<'a>(root: &'a StyledNode<'a>, viewport_w: f32) -> LayoutBox<'a> {
    layout_tree_with_images(root, (viewport_w, 0.0), &HashMap::new())
}

/// Lay out with decoded images for `<img>` elements, by node.
pub fn layout_tree_with_images<'a>(
    root: &'a StyledNode<'a>,
    viewport: (f32, f32),
    images: &HashMap<u32, Decoded>,
) -> LayoutBox<'a> {
    let viewport_w = viewport.0;
    let mut b = build_layout_tree(root);
    if !images.is_empty() {
        attach_images(&mut b, images);
    }
    b.layout_block(0.0, viewport_w, 0.0, None);
    // The initial containing block: absolutely positioned boxes with no
    // positioned ancestor, and fixed ones, are placed in it. Pages are
    // painted whole, so fixed boxes don't stay put when scrolling.
    let h = if viewport.1 > 0.0 {
        viewport.1
    } else {
        b.dims.margin_box().h
    };
    let icb = Rect {
        x: 0.0,
        y: 0.0,
        w: viewport_w,
        h,
    };
    b.lay_out_contained(icb, Some(icb));
    apply_relative(&mut b, icb);
    b
}

/// Shift `position: relative` boxes by their offsets, once everything else
/// is in place (they don't affect the layout around them).
fn apply_relative(b: &mut LayoutBox, cb: Rect) {
    for c in &mut b.children {
        if c.position() == Position::Relative {
            let s = c.style();
            let [t, r, bo, l] = s.inset;
            let dx = l.resolve(cb.w).or(r.resolve(cb.w).map(|r| -r)).unwrap_or(0.0);
            let dy = t.resolve(cb.h).or(bo.resolve(cb.h).map(|b| -b)).unwrap_or(0.0);
            if dx != 0.0 || dy != 0.0 {
                c.translate(dx, dy);
            }
        }
        // Anonymous boxes pass their parent's containing block through.
        let inner = match c.kind {
            BoxKind::Block(_) => c.dims.content,
            BoxKind::Anonymous { .. } => cb,
        };
        apply_relative(c, inner);
    }
}

fn attach_images(b: &mut LayoutBox, images: &HashMap<u32, Decoded>) {
    if let BoxKind::Block(n) = b.kind {
        if n.node.tag() == Some("img") {
            if let Some(d) = images.get(&b.uid) {
                // The image replaces the alt text.
                b.image = Some(d.clone());
                b.children.clear();
                return;
            }
        }
    }
    for c in &mut b.children {
        attach_images(c, images);
    }
}

impl<'a> LayoutBox<'a> {
    /// Lay out in normal flow, among the floats of the enclosing block
    /// formatting context.
    fn layout(&mut self, cb_x: f32, cb_w: f32, y: f32, floats: &mut Floats) {
        match self.kind {
            BoxKind::Block(_) if self.new_bfc() => self.layout_block(cb_x, cb_w, y, None),
            BoxKind::Block(_) => self.layout_block_in(cb_x, cb_w, y, None, floats, false),
            BoxKind::Anonymous { .. } => {
                self.dims.content = Rect {
                    x: cb_x,
                    y,
                    w: cb_w,
                    h: 0.0,
                };
                self.layout_inline(floats);
            }
        }
    }

    /// Does this box start its own block formatting context? Then it keeps
    /// its floats inside and doesn't flow around outside ones.
    fn new_bfc(&self) -> bool {
        match self.kind {
            BoxKind::Block(n) => {
                let s = &n.style;
                s.contain_floats
                    || s.float != Float::None
                    || matches!(s.position, Position::Absolute | Position::Fixed)
                    || matches!(
                        s.display,
                        Display::Flex
                            | Display::InlineFlex
                            | Display::Table
                            | Display::InlineTable
                            | Display::TableCell
                            | Display::TableCaption
                            | Display::InlineBlock
                    )
            }
            BoxKind::Anonymous { .. } => false,
        }
    }

    fn edges(&mut self, cb_w: f32) {
        let s = self.style().clone();
        let r = |l: Len| l.or_zero(cb_w);
        self.dims.padding = Edges {
            t: r(s.padding[0]),
            r: r(s.padding[1]),
            b: r(s.padding[2]),
            l: r(s.padding[3]),
        };
        self.dims.border = Edges {
            t: s.border_width[0],
            r: s.border_width[1],
            b: s.border_width[2],
            l: s.border_width[3],
        };
        self.dims.margin.t = r(s.margin[0]);
        self.dims.margin.b = r(s.margin[2]);
    }

    /// Lay out as the root of a new block formatting context.
    /// `forced_w`: border-box width imposed by a flex/inline-block parent.
    fn layout_block(&mut self, cb_x: f32, cb_w: f32, y: f32, forced_w: Option<f32>) {
        self.layout_block_in(cb_x, cb_w, y, forced_w, &mut Floats::default(), true);
    }

    /// `contain`: this box is the root of `floats`' formatting context, so
    /// it grows to contain them.
    fn layout_block_in(
        &mut self,
        cb_x: f32,
        cb_w: f32,
        y: f32,
        forced_w: Option<f32>,
        floats: &mut Floats,
        contain: bool,
    ) {
        self.edges(cb_w);
        let s = self.style().clone();
        let hz = {
            let d = &self.dims;
            d.padding.l + d.padding.r + d.border.l + d.border.r
        };
        let table =
            matches!(s.display, Display::Table | Display::InlineTable).then(|| self.table_columns(cb_w));
        let d = &mut self.dims;

        let mut width = s.width.resolve(cb_w);
        if let Some(t) = &table {
            // A table is as wide as its columns want, within the space it has.
            // Its `width` is for the border box, and it never gets narrower
            // than its columns' minimum.
            let (min, max) = t.content_widths();
            width = Some(match width {
                Some(w) => (w - hz).max(min),
                None => {
                    let margins = s.margin[1].or_zero(cb_w) + s.margin[3].or_zero(cb_w);
                    max.min(cb_w - margins - hz).max(min)
                }
            });
        }
        if let (Some(w), Some(max)) = (width, s.max_width.resolve(cb_w)) {
            width = Some(w.min(max));
        }
        let mut ml = s.margin[3].resolve(cb_w);
        let mut mr = s.margin[1].resolve(cb_w);
        if let Some(fw) = forced_w {
            width = Some((fw - hz).max(0.0));
            ml = Some(ml.unwrap_or(0.0));
            mr = Some(mr.unwrap_or(0.0));
        }
        let solve = |width: Option<f32>, ml: Option<f32>, mr: Option<f32>| -> (f32, f32, f32) {
            let known = width.unwrap_or(0.0) + ml.unwrap_or(0.0) + mr.unwrap_or(0.0) + hz;
            let under = cb_w - known;
            match (width, ml, mr) {
                (None, ml, mr) => {
                    let (l, r) = (ml.unwrap_or(0.0), mr.unwrap_or(0.0));
                    (under.max(0.0), l, r + under.min(0.0))
                }
                (Some(w), None, None) => (w, (under / 2.0).max(0.0), (under / 2.0).max(0.0)),
                (Some(w), None, Some(r)) => (w, under.max(0.0), r),
                (Some(w), Some(l), None) => (w, l, under),
                (Some(w), Some(l), Some(r)) => (w, l, r + under),
            }
        };
        let replaced = self
            .image
            .as_ref()
            .map(|i| replaced_size(&s, i.natural, cb_w, forced_w.map(|f| f - hz)));
        if let Some((rw, _)) = replaced {
            if forced_w.is_none() {
                width = Some(rw);
            }
        }
        let (mut w, mut l, mut r) = solve(width, ml, mr);
        if let Some(max) = s.max_width.resolve(cb_w) {
            if w > max {
                (w, l, r) = solve(Some(max), ml, mr);
            }
        }
        if forced_w.is_some() {
            r = mr.unwrap_or(0.0);
        }
        d.margin.l = l;
        d.margin.r = r;
        d.content.w = w;
        d.content.x = cb_x + l + d.border.l + d.padding.l;
        d.content.y = y + d.margin.t + d.border.t + d.padding.t;

        if let Some((_, rh)) = replaced {
            // Height follows the width actually used, keeping the aspect
            // ratio unless a height was given.
            let w = self.dims.content.w;
            self.dims.content.h = match (s.height, self.image.as_ref()) {
                (Len::Px(_), _) => rh,
                (_, Some(i)) if i.natural.0 > 0 => w * i.natural.1 as f32 / i.natural.0 as f32,
                _ => rh,
            };
        } else if let Some(t) = table {
            self.layout_table(t);
        } else if is_flex(s.display) {
            let definite_h = match s.height {
                Len::Px(h) => Some(h),
                _ => None,
            };
            self.layout_flex(definite_h);
        } else {
            self.layout_block_children(floats);
        }
        if contain {
            if let Some(b) = floats.bottom() {
                let c = &mut self.dims.content;
                c.h = c.h.max(b - c.y);
            }
        }

        let s = self.style().clone();
        let d = &mut self.dims;
        if let Len::Px(h) = s.height {
            d.content.h = h;
        }
        if let Len::Px(h) = s.min_height {
            d.content.h = d.content.h.max(h);
        }
        // A positioned box is the containing block of the absolutely
        // positioned boxes inside it. (Those do this themselves once their
        // height is known, in `layout_absolute`.)
        if !matches!(
            s.position,
            Position::Static | Position::Absolute | Position::Fixed
        ) {
            self.lay_out_contained(self.dims.padding_box(), None);
        }
    }

    /// Lay out the absolutely positioned boxes whose containing block is
    /// `cb`. For the root, `icb` is the initial containing block: fixed
    /// boxes go there, from anywhere in the tree.
    fn lay_out_contained(&mut self, cb: Rect, icb: Option<Rect>) {
        // A positioned root has already placed its own absolute boxes.
        let own = icb.is_none() || self.position() == Position::Static;
        walk_contained(self, cb, icb, own);
        fn walk_contained(b: &mut LayoutBox, cb: Rect, icb: Option<Rect>, own: bool) {
            for c in &mut b.children {
                match (c.position(), icb) {
                    (Position::Absolute, _) if own => {
                        c.layout_absolute(cb);
                        if icb.is_some() {
                            walk_contained(c, cb, icb, false);
                        }
                    }
                    (Position::Fixed, Some(v)) => {
                        c.layout_absolute(v);
                        walk_contained(c, cb, icb, false);
                    }
                    (Position::Static, _) => walk_contained(c, cb, icb, own),
                    // Another containing block: only the root looks inside,
                    // for fixed boxes.
                    (_, Some(_)) => walk_contained(c, cb, icb, false),
                    _ => {}
                }
            }
        }
    }

    /// Size and place an absolutely positioned box in containing block `cb`
    /// (a padding box). Its `dims.content` origin holds its static position.
    fn layout_absolute(&mut self, cb: Rect) {
        let (sx, sy) = (self.dims.content.x, self.dims.content.y);
        let s = self.style().clone();
        let [t, r, b, l] = [
            s.inset[0].resolve(cb.h),
            s.inset[1].resolve(cb.w),
            s.inset[2].resolve(cb.h),
            s.inset[3].resolve(cb.w),
        ];
        let px = |l: Len, base: f32| l.or_zero(base);
        let pb = px(s.padding[1], cb.w) + px(s.padding[3], cb.w) + s.border_width[1] + s.border_width[3];
        let (ml, mr) = (s.margin[3].resolve(cb.w), s.margin[1].resolve(cb.w));
        let mh = ml.unwrap_or(0.0) + mr.unwrap_or(0.0);
        let width = s.width.resolve(cb.w);
        // Border-box width: as given, stretched between `left` and `right`,
        // or shrink-to-fit.
        let bw = match (width, l, r) {
            (Some(w), ..) => w + pb,
            (None, Some(l), Some(r)) => (cb.w - l - r - mh).max(0.0),
            _ => {
                let (min, max) = self.intrinsic_widths(cb.w);
                let avail = cb.w - l.unwrap_or(0.0) - r.unwrap_or(0.0);
                max.min(avail).max(min) - mh
            }
        };
        let bw = match s.max_width.resolve(cb.w) {
            Some(m) if width.is_some() => bw.min(m + pb),
            _ => bw,
        };
        // Auto margins center a box pinned on both sides.
        let (ml, mr) = match (l, r, width, ml, mr) {
            (Some(l), Some(r), Some(_), None, None) => {
                let free = ((cb.w - l - r - bw) / 2.0).max(0.0);
                (free, free)
            }
            _ => (ml.unwrap_or(0.0), mr.unwrap_or(0.0)),
        };
        self.layout_block(0.0, bw + ml + mr, 0.0, Some(bw));
        self.dims.margin.l = ml;
        self.dims.margin.r = mr;

        let (mt, mb) = (s.margin[0].or_zero(cb.w), s.margin[2].or_zero(cb.w));
        let pbv = self.dims.border_box().h - self.dims.content.h;
        if let (Len::Auto, Some(t), Some(b)) = (s.height, t, b) {
            self.dims.content.h = (cb.h - t - b - mt - mb - pbv).max(0.0);
        }
        let bh = self.dims.border_box().h;
        let x = match (l, r) {
            (Some(l), _) => cb.x + l + ml,
            (None, Some(r)) => cb.x + cb.w - r - mr - bw,
            _ => sx + ml,
        };
        let y = match (t, b) {
            (Some(t), _) => cb.y + t + mt,
            (None, Some(b)) => cb.y + cb.h - b - mb - bh,
            _ => sy + mt,
        };
        // Its own absolute boxes, now that its size is final.
        self.lay_out_contained(self.dims.padding_box(), None);
        let bb = self.dims.border_box();
        self.translate(x - bb.x, y - bb.y);
    }

    fn layout_block_children(&mut self, floats: &mut Floats) {
        let content = self.dims.content;
        let mut cursor = content.y;
        let mut prev_mb = 0.0f32;
        for child in &mut self.children {
            let (mt, clear) = match child.kind {
                BoxKind::Block(n) => (n.style.margin[0].or_zero(content.w), n.style.clear),
                _ => (0.0, Clear::None),
            };
            // Collapse adjacent vertical margins (positive margins only).
            let collapse = prev_mb.min(mt).max(0.0);
            let mut y = cursor - collapse;
            // `clear` moves the border box below the floats on that side.
            if let Some(cy) = floats.clear_y(clear) {
                if cy > y + mt {
                    y = cy - mt;
                }
            }
            if child.new_bfc() {
                // A new formatting context sits beside the floats, not under them.
                let (l, w) = floats.space(y + mt, 1.0, content.x, content.x + content.w);
                child.layout(l, w, y, floats);
            } else {
                child.layout(content.x, content.w, y, floats);
            }
            // A run holding only floats takes no space and doesn't stop the
            // margins around it from collapsing.
            if matches!(child.kind, BoxKind::Anonymous { .. }) && child.dims.content.h == 0.0 {
                continue;
            }
            cursor = child.dims.margin_box().y + child.dims.margin_box().h;
            prev_mb = if matches!(child.kind, BoxKind::Block(_)) {
                child.dims.margin.b
            } else {
                0.0
            };
        }
        self.dims.content.h = cursor - content.y;
    }

    /// Flexbox, simplified from CSS Flexible Box Layout: items start at
    /// their basis, grow or shrink to fill each line (never below their
    /// content's minimum), wrap onto more lines if allowed, and are then
    /// placed along each line (`justify-content`) and across it
    /// (`align-items`, `align-self`), with lines placed by `align-content`.
    /// `definite_h` is the container's `height`, if set.
    fn layout_flex(&mut self, definite_h: Option<f32>) {
        let content = self.dims.content;
        let s = self.style().clone();
        let (items, others): (Vec<usize>, Vec<usize>) =
            (0..self.children.len()).partition(|&i| self.children[i].is_flex_item());
        // Placeholders for absolutely positioned children sit at the start.
        for i in others {
            self.children[i].layout_at(content.x, content.y, content.w);
        }
        let mut items = items;
        items.sort_by_key(|&i| self.children[i].flex().order);
        if s.flex_row {
            self.layout_flex_row(items, &s, definite_h);
        } else {
            if s.flex_reverse {
                items.reverse();
            }
            self.layout_flex_column(items, &s, definite_h);
        }
    }

    fn layout_flex_row(&mut self, items: Vec<usize>, s: &ComputedStyle, definite_h: Option<f32>) {
        let content = self.dims.content;
        let avail = content.w;
        let (gap, row_gap) = (s.gap, s.row_gap);
        // Outer (margin-box) main sizes: the basis, and the smallest the
        // item can be without its content overflowing.
        let mut base = Vec::with_capacity(items.len());
        let mut min = Vec::with_capacity(items.len());
        for &i in &items {
            let c = &self.children[i];
            let (lo, hi) = c.intrinsic_widths(avail);
            // The automatic minimum: the content's minimum, but no more
            // than a width that's set.
            let lo = lo.min(c.intrinsic_widths_of(avail, false).0);
            let outer_extra = c.outer_extra_w(avail);
            base.push(match c.flex().basis.resolve(avail) {
                Some(b) => b + outer_extra,
                None => hi,
            });
            min.push(lo);
        }
        let hyp: Vec<f32> = base.iter().zip(&min).map(|(b, m)| b.max(*m)).collect();
        // Break into lines.
        let mut lines: Vec<Vec<usize>> = Vec::new();
        let mut used = 0.0;
        for (k, &h) in hyp.iter().enumerate() {
            match lines.last_mut() {
                Some(line) if !s.flex_wrap || used + gap + h <= avail => {
                    line.push(k);
                    used += gap + h;
                }
                _ => {
                    lines.push(vec![k]);
                    used = h;
                }
            }
        }
        // Resolve flexible sizes and lay each item out at its size.
        let mut size = hyp.clone();
        for line in &lines {
            let gaps = gap * (line.len() - 1) as f32;
            let free = avail - gaps - line.iter().map(|&k| hyp[k]).sum::<f32>();
            if free > 0.0 {
                let grow: f32 = line.iter().map(|&k| self.children[items[k]].flex().grow).sum();
                if grow > 0.0 {
                    for &k in line {
                        size[k] += free * self.children[items[k]].flex().grow / grow;
                    }
                }
            } else if free < 0.0 {
                // Shrink in proportion to shrink × basis; items that reach
                // their minimum stop, and the rest shrink a little more.
                for _ in 0..4 {
                    let over = line.iter().map(|&k| size[k]).sum::<f32>() + gaps - avail;
                    let open: Vec<usize> = line
                        .iter()
                        .copied()
                        .filter(|&k| size[k] > min[k] + 0.01)
                        .collect();
                    let weight = |k: usize| self.children[items[k]].flex().shrink * base[k].max(0.0);
                    let total: f32 = open.iter().map(|&k| weight(k)).sum();
                    if over <= 0.01 || total <= 0.0 {
                        break;
                    }
                    for &k in &open {
                        size[k] = (size[k] - over * weight(k) / total).max(min[k]);
                    }
                }
            }
            for &k in line {
                let c = &mut self.children[items[k]];
                c.layout_outer(content.x, content.y, size[k], avail);
            }
        }
        // Line heights, then `align-content` if the container's height is set.
        let mut cross: Vec<f32> = lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|&k| self.children[items[k]].dims.margin_box().h)
                    .fold(0.0, f32::max)
            })
            .collect();
        let gaps = row_gap * lines.len().saturating_sub(1) as f32;
        let mut line_y = content.y;
        let mut line_gap = row_gap;
        if let Some(h) = definite_h {
            if !s.flex_wrap && cross.len() == 1 {
                cross[0] = h;
            } else if !cross.is_empty() {
                let free = h - cross.iter().sum::<f32>() - gaps;
                let (start, extra) = spread(s.align_content, free, cross.len());
                if s.align_content == Justify::Stretch && free > 0.0 {
                    cross.iter_mut().for_each(|c| *c += free / lines.len() as f32);
                } else {
                    line_y += start;
                    line_gap += extra;
                }
            }
        }
        for (line, &line_h) in lines.iter().zip(&cross) {
            let gaps = gap * (line.len() - 1) as f32;
            let free = avail - gaps - line.iter().map(|&k| size[k]).sum::<f32>();
            let (start, extra) = spread(s.justify, free, line.len());
            let mut x = content.x + start;
            for &k in line {
                let item = items[k];
                let align = self.children[item].flex().align.unwrap_or(s.align_items);
                let c = &mut self.children[item];
                let mb = c.dims.margin_box();
                let dy = match align {
                    Align::Stretch => {
                        if c.flex().auto_height {
                            c.dims.content.h += (line_h - mb.h).max(0.0);
                        }
                        0.0
                    }
                    Align::Start => 0.0,
                    Align::End => line_h - mb.h,
                    Align::Center => (line_h - mb.h) / 2.0,
                };
                // `row-reverse` mirrors each line.
                let target_x = if s.flex_reverse {
                    content.x + content.w - (x - content.x) - size[k]
                } else {
                    x
                };
                c.translate(target_x - mb.x, line_y + dy - mb.y);
                x += size[k] + gap + extra;
            }
            line_y += line_h + line_gap;
        }
        self.dims.content.h = if lines.is_empty() {
            0.0
        } else {
            line_y - line_gap - content.y
        };
    }

    fn layout_flex_column(&mut self, items: Vec<usize>, s: &ComputedStyle, definite_h: Option<f32>) {
        let content = self.dims.content;
        let gap = s.row_gap;
        // Width across: stretched, or shrink-to-fit when aligned.
        let mut size = Vec::with_capacity(items.len());
        for &i in &items {
            let align = self.children[i].flex().align.unwrap_or(s.align_items);
            let c = &mut self.children[i];
            let w = if align == Align::Stretch && c.flex().auto_width {
                content.w
            } else {
                let (lo, hi) = c.intrinsic_widths(content.w);
                hi.min(content.w).max(lo)
            };
            c.layout_outer(content.x, content.y, w, content.w);
            let h = c.dims.margin_box().h;
            let basis = c
                .flex()
                .basis
                .resolve(definite_h.unwrap_or(0.0))
                .filter(|_| definite_h.is_some());
            size.push(match basis {
                Some(b) => b + (h - c.dims.content.h),
                None => h,
            });
        }
        let gaps = gap * items.len().saturating_sub(1) as f32;
        let mut free = 0.0;
        if let Some(main) = definite_h {
            free = main - gaps - size.iter().sum::<f32>();
            let grow: f32 = items.iter().map(|&i| self.children[i].flex().grow).sum();
            if free > 0.0 && grow > 0.0 {
                for (k, &i) in items.iter().enumerate() {
                    size[k] += free * self.children[i].flex().grow / grow;
                }
                free = 0.0;
            }
        }
        let (start, extra) = if definite_h.is_some() {
            spread(s.justify, free, items.len())
        } else {
            (0.0, 0.0)
        };
        let mut y = content.y + start;
        for (k, &i) in items.iter().enumerate() {
            let align = self.children[i].flex().align.unwrap_or(s.align_items);
            let c = &mut self.children[i];
            let mb = c.dims.margin_box();
            c.dims.content.h += (size[k] - mb.h).max(0.0);
            let dx = match align {
                Align::Stretch | Align::Start => 0.0,
                Align::End => content.w - mb.w,
                Align::Center => (content.w - mb.w) / 2.0,
            };
            c.translate(content.x + dx - mb.x, y - mb.y);
            y += size[k] + gap + extra;
        }
        self.dims.content.h = if items.is_empty() {
            0.0
        } else {
            y - gap - extra - content.y
        };
    }

    /// Does this child of a flex container take part in the flex layout?
    /// Elements do (unless absolutely positioned), and so do runs of text
    /// that aren't just whitespace.
    fn is_flex_item(&self) -> bool {
        match &self.kind {
            BoxKind::Block(_) => !self.out_of_flow(),
            BoxKind::Anonymous { items, .. } => items.iter().any(|i| match i {
                InlineItem::Text { text, style, .. } => style.pre || !is_collapsible_ws(text),
                InlineItem::Atomic(_) | InlineItem::Float(_) => true,
                InlineItem::Break(_) | InlineItem::OutOfFlow(_) => false,
            }),
        }
    }

    /// This item's flex properties. Anonymous items (runs of text) have the
    /// initial values.
    fn flex(&self) -> FlexItem {
        match self.kind {
            BoxKind::Block(n) => {
                let s = &n.style;
                FlexItem {
                    grow: s.flex_grow,
                    shrink: s.flex_shrink,
                    basis: s.flex_basis,
                    align: s.align_self,
                    order: s.order,
                    auto_width: s.width == Len::Auto,
                    auto_height: s.height == Len::Auto,
                }
            }
            BoxKind::Anonymous { .. } => FlexItem {
                grow: 0.0,
                shrink: 1.0,
                basis: Len::Auto,
                align: None,
                order: 0,
                auto_width: true,
                auto_height: true,
            },
        }
    }

    /// Horizontal margins, padding and borders: what's around the content box.
    fn outer_extra_w(&self, cb_w: f32) -> f32 {
        match self.kind {
            BoxKind::Block(n) => {
                let s = &n.style;
                let e = |l: &[Len; 4]| l[1].or_zero(cb_w) + l[3].or_zero(cb_w);
                e(&s.margin) + e(&s.padding) + s.border_width[1] + s.border_width[3]
            }
            BoxKind::Anonymous { .. } => 0.0,
        }
    }

    /// Lay out with margin box `outer_w` wide, its left edge at `x`, in a
    /// container `cb_w` wide (for percentages).
    fn layout_outer(&mut self, x: f32, y: f32, outer_w: f32, cb_w: f32) {
        match self.kind {
            BoxKind::Block(_) => {
                let m = margins_h(self, cb_w);
                self.layout_block(x, cb_w, y, Some((outer_w - m).max(0.0)));
            }
            BoxKind::Anonymous { .. } => self.layout_at(x, y, outer_w),
        }
    }

    /// Lay out at a forced border-box width `w`, as a flex item or table cell.
    fn layout_at(&mut self, x: f32, y: f32, w: f32) {
        match self.kind {
            BoxKind::Block(_) => self.layout_block(x, w, y, Some(w)),
            BoxKind::Anonymous { .. } => {
                self.dims.content = Rect { x, y, w, h: 0.0 };
                self.layout_inline(&mut Floats::default());
            }
        }
    }

    /// Shrink-to-fit preferred width (border box, including margins).
    fn max_content_width(&self, cb_w: f32) -> f32 {
        self.intrinsic_widths(cb_w).1
    }

    /// Min-content and max-content widths (border box, including margins):
    /// the narrowest this box can be without overflowing, and the width it
    /// takes when nothing wraps.
    fn intrinsic_widths(&self, cb_w: f32) -> (f32, f32) {
        self.intrinsic_widths_of(cb_w, true)
    }

    /// `intrinsic_widths`, but with `sized: false` this box's own `width` is
    /// ignored (its descendants' still count): the size of its content.
    fn intrinsic_widths_of(&self, cb_w: f32, sized: bool) -> (f32, f32) {
        let s = self.style();
        let edges = |l: &[Len; 4]| l[1].or_zero(cb_w) + l[3].or_zero(cb_w);
        let (pb, m) = match self.kind {
            BoxKind::Block(_) => (
                edges(&s.padding) + s.border_width[1] + s.border_width[3],
                edges(&s.margin),
            ),
            _ => (0.0, 0.0),
        };
        let hz = pb + m;
        if let Some(i) = &self.image {
            let (w, _) = replaced_size(s, i.natural, cb_w, None);
            return (w + hz, w + hz);
        }
        if let BoxKind::Block(_) = self.kind {
            if matches!(s.display, Display::Table | Display::InlineTable) {
                let (min, max) = self.table_columns(cb_w).content_widths();
                // A percentage width doesn't make the table any less able to shrink.
                return match (s.width, s.width.resolve(cb_w)) {
                    (Len::Px(_), Some(w)) => {
                        let w = (w - pb).max(min) + hz;
                        (w, w)
                    }
                    (_, Some(w)) => (min + hz, (w - pb).max(min) + hz),
                    _ => (min + hz, max + hz),
                };
            }
            // A cell with a fixed width still grows to fit its content.
            // Percentages are left to the table.
            if s.display != Display::TableCell && sized {
                if let Some(w) = s.width.resolve(cb_w) {
                    return (w + hz, w + hz);
                }
            }
        }
        let (min, max) = match &self.kind {
            BoxKind::Anonymous { items, .. } => {
                let (mut best, mut line) = (0.0f32, 0.0f32);
                // Widest unbreakable run, and the one we're in.
                let (mut min, mut run) = (0.0f32, 0.0f32);
                for item in items {
                    match item {
                        InlineItem::Text { text, style, .. } => {
                            let collapsed: String = if style.pre {
                                text.clone()
                            } else {
                                text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
                            };
                            let txt = if style.uppercase {
                                collapsed.to_uppercase()
                            } else {
                                collapsed
                            };
                            let f = font::index(style);
                            let measure = |t: &str| font::measure(f, style.font_size, t);
                            for (i, part) in txt.split('\n').enumerate() {
                                if i > 0 {
                                    best = best.max(line);
                                    line = 0.0;
                                }
                                line += measure(part);
                                if !style.pre && text.ends_with(|c: char| c.is_ascii_whitespace()) {
                                    line += measure(" ");
                                }
                            }
                            if style.pre {
                                for part in txt.split('\n') {
                                    min = min.max(measure(&part.replace('\t', "        ")));
                                }
                                run = 0.0;
                            } else if style.nowrap {
                                run += measure(&txt);
                                min = min.max(run);
                            } else {
                                // The same break opportunities as `layout_inline`.
                                if text.starts_with(|c: char| c.is_ascii_whitespace()) {
                                    run = 0.0;
                                }
                                for (wi, word) in txt.split(' ').enumerate() {
                                    if wi > 0 {
                                        run = 0.0;
                                    }
                                    let mut start = 0;
                                    for (bi, _) in unicode_linebreak::linebreaks(word) {
                                        if bi > start && bi < word.len() {
                                            run += measure(&word[start..bi]);
                                            min = min.max(run);
                                            run = 0.0;
                                            start = bi;
                                        }
                                    }
                                    run += measure(&word[start..]);
                                    min = min.max(run);
                                }
                                if text.ends_with(|c: char| c.is_ascii_whitespace()) {
                                    run = 0.0;
                                }
                            }
                        }
                        InlineItem::Break(_) => {
                            best = best.max(line);
                            line = 0.0;
                            run = 0.0;
                        }
                        InlineItem::OutOfFlow(_) => {}
                        InlineItem::Atomic(i) | InlineItem::Float(i) => {
                            let (cmin, cmax) = self.children[*i].intrinsic_widths(cb_w);
                            line += cmax;
                            min = min.max(cmin);
                            run = 0.0;
                        }
                    }
                }
                (min, best.max(line))
            }
            BoxKind::Block(_) => {
                let row = is_flex(s.display) && s.flex_row;
                let ws: Vec<(f32, f32)> = self
                    .children
                    .iter()
                    .filter(|c| !is_flex(s.display) || c.is_flex_item())
                    .map(|c| c.intrinsic_widths(cb_w))
                    .collect();
                if row {
                    let gaps = s.gap * ws.len().saturating_sub(1) as f32;
                    let max = ws.iter().map(|w| w.1).sum::<f32>() + gaps;
                    // Wrapping lines can be as narrow as their widest item.
                    let min = if s.flex_wrap {
                        ws.iter().map(|w| w.0).fold(0.0, f32::max)
                    } else {
                        ws.iter().map(|w| w.0).sum::<f32>() + gaps
                    };
                    (min, max)
                } else {
                    let ws = ws.into_iter();
                    ws.fold((0.0, 0.0), |(a, b), (x, y)| (f32::max(a, x), f32::max(b, y)))
                }
            }
        };
        let (mut min, mut max) = (min + hz, max + hz);
        if let (BoxKind::Block(_), Some(w), true) = (&self.kind, s.width.resolve(cb_w), sized) {
            // A fixed-width cell (percentages were skipped above).
            if matches!(s.width, Len::Px(_)) {
                min = min.max(w + hz);
                max = min;
            }
        }
        if let Some(m) = s.max_width.resolve(cb_w) {
            min = min.min(m + hz);
            max = max.min(m + hz);
        }
        (min, max)
    }

    // -----------------------------------------------------------------------
    // Inline formatting context
    // -----------------------------------------------------------------------

    fn layout_inline(&mut self, floats: &mut Floats) {
        let content = self.dims.content;
        let avail = content.w;
        let (items, container_style) = match &self.kind {
            BoxKind::Anonymous { items, style, .. } => (items.clone(), style.clone()),
            _ => return,
        };

        // Lay out atomic inline boxes and floats at the origin first so we
        // know their size. Floats shrink to fit, but not below their minimum.
        for item in &items {
            match item {
                InlineItem::Atomic(i) => {
                    let child = &mut self.children[*i];
                    let pref = child.max_content_width(avail);
                    let w = pref.min(avail);
                    child.layout_block(0.0, w, 0.0, Some(w - margins_h(child, avail)));
                }
                InlineItem::Float(i) => {
                    let child = &mut self.children[*i];
                    let (min, max) = child.intrinsic_widths(avail);
                    let w = max.min(avail).max(min);
                    child.layout_block(0.0, w, 0.0, Some(w - margins_h(child, avail)));
                }
                _ => {}
            }
        }

        enum Piece {
            Word {
                text: String,
                style: Rc<ComputedStyle>,
                bg: Color,
                link: Option<String>,
                uid: u32,
                w: f32,
                /// No line break allowed between this and the previous piece.
                glue: bool,
            },
            Space {
                style: Rc<ComputedStyle>,
                bg: Color,
                link: Option<String>,
                uid: u32,
                w: f32,
            },
            Break(Rc<ComputedStyle>),
            Atomic(usize, f32),
            Float(usize),
            OutOfFlow(usize),
        }
        let mut pieces = Vec::new();
        let mut last_space = true;
        // Last character of the previous word, if nothing (no space, break
        // or inline-block) separates it from the next one.
        let mut prev_tail: Option<char> = None;
        for item in &items {
            match item {
                InlineItem::Text {
                    text,
                    style,
                    bg,
                    link,
                    uid,
                } => {
                    let f = font::index(style);
                    let text = if style.uppercase {
                        text.to_uppercase()
                    } else {
                        text.clone()
                    };
                    let mk_word = |t: &str, glue: bool| Piece::Word {
                        text: t.to_string(),
                        style: style.clone(),
                        bg: *bg,
                        link: link.clone(),
                        uid: *uid,
                        w: font::measure(f, style.font_size, t),
                        glue,
                    };
                    if style.pre {
                        for (i, line) in text.split('\n').enumerate() {
                            if i > 0 {
                                pieces.push(Piece::Break(style.clone()));
                            }
                            let line = line.replace('\t', "        ");
                            if !line.is_empty() {
                                pieces.push(mk_word(&line, false));
                            }
                        }
                        last_space = false;
                        prev_tail = None;
                        continue;
                    }
                    if style.nowrap {
                        let mut t = text.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
                        if text.starts_with(|c: char| c.is_ascii_whitespace()) && !last_space {
                            t.insert(0, ' ');
                        }
                        if !t.trim().is_empty() {
                            pieces.push(mk_word(&t, false));
                            last_space = text.ends_with(|c: char| c.is_ascii_whitespace());
                        }
                        prev_tail = None;
                        continue;
                    }
                    // A run of non-space text is split further wherever the
                    // Unicode line-breaking rules (UAX #14) allow: between
                    // CJK characters, after hyphens and slashes, and so on.
                    let push_word = |word: &str, pieces: &mut Vec<Piece>, prev_tail: &mut Option<char>| {
                        let first = word.chars().next().unwrap_or(' ');
                        let mut glue = prev_tail.is_some_and(|a| !break_between(a, first));
                        let mut start = 0;
                        for (i, _) in unicode_linebreak::linebreaks(word) {
                            if i > start && i < word.len() {
                                pieces.push(mk_word(&word[start..i], glue));
                                glue = false;
                                start = i;
                            }
                        }
                        pieces.push(mk_word(&word[start..], glue));
                        *prev_tail = word.chars().last();
                    };
                    let mut word = String::new();
                    for c in text.chars() {
                        if c.is_ascii_whitespace() {
                            if !word.is_empty() {
                                push_word(&word, &mut pieces, &mut prev_tail);
                                word.clear();
                            }
                            prev_tail = None;
                            if !last_space {
                                pieces.push(Piece::Space {
                                    style: style.clone(),
                                    bg: *bg,
                                    link: link.clone(),
                                    uid: *uid,
                                    w: font::measure(f, style.font_size, " "),
                                });
                                last_space = true;
                            }
                        } else {
                            word.push(c);
                            last_space = false;
                        }
                    }
                    if !word.is_empty() {
                        push_word(&word, &mut pieces, &mut prev_tail);
                    }
                }
                InlineItem::Break(s) => {
                    pieces.push(Piece::Break(s.clone()));
                    last_space = true;
                    prev_tail = None;
                }
                InlineItem::Atomic(i) => {
                    let w = self.children[*i].dims.margin_box().w;
                    pieces.push(Piece::Atomic(*i, w));
                    last_space = false;
                    prev_tail = None;
                }
                InlineItem::Float(i) => pieces.push(Piece::Float(*i)),
                InlineItem::OutOfFlow(i) => pieces.push(Piece::OutOfFlow(*i)),
            }
        }

        // Break into lines. Each line is as wide as the floats beside it
        // allow, and is placed as soon as it's complete, so that floats met
        // along the way affect the lines after them.
        let (x0, x1) = (content.x, content.x + content.w);
        let band = {
            let (a, d) = half_leading(&container_style);
            (a + d).max(1.0)
        };
        let mut line_y = content.y;
        let (mut left, mut avail) = floats.space(line_y, band, x0, x1);
        let mut cur: Vec<Placed> = Vec::new();
        let mut x = 0.0f32;
        let mut frags = Vec::new();
        // Floats that didn't fit on the current line go below it.
        let mut pending: Vec<usize> = Vec::new();
        macro_rules! next_line {
            ($br:expr) => {{
                let br: Option<Rc<ComputedStyle>> = $br;
                let clear = br.as_ref().map_or(Clear::None, |s| s.clear);
                line_y += self.place_line(
                    std::mem::take(&mut cur),
                    x,
                    br,
                    left,
                    avail,
                    line_y,
                    &container_style,
                    &mut frags,
                );
                x = 0.0;
                if let Some(cy) = floats.clear_y(clear) {
                    line_y = line_y.max(cy);
                }
                for i in pending.drain(..) {
                    self.place_float(i, line_y, floats, x0, x1);
                }
                (left, avail) = floats.space(line_y, band, x0, x1);
            }};
        }
        // On an empty line, move down past floats until `w` fits.
        macro_rules! make_room {
            ($w:expr) => {{
                while cur.is_empty() && $w > avail {
                    match floats.next_bottom(line_y, band) {
                        Some(b) => {
                            line_y = b;
                            (left, avail) = floats.space(line_y, band, x0, x1);
                        }
                        None => break,
                    }
                }
            }};
        }
        // Width of each glued group of words starting at piece i: a group
        // moves to the next line as a whole.
        let mut group_w = vec![0.0f32; pieces.len()];
        {
            let mut i = pieces.len();
            let mut acc = 0.0;
            while i > 0 {
                i -= 1;
                if let Piece::Word { w, glue, .. } = &pieces[i] {
                    acc += w;
                    group_w[i] = acc;
                    if !glue {
                        acc = 0.0;
                    }
                } else {
                    acc = 0.0;
                }
            }
        }
        for (pi, p) in pieces.into_iter().enumerate() {
            match p {
                Piece::Word {
                    text,
                    style,
                    bg,
                    link,
                    uid,
                    w,
                    glue,
                } => {
                    let gw = group_w[pi];
                    if !glue && x + gw > avail && !cur.is_empty() && !(style.pre && style.nowrap) {
                        next_line!(None);
                    }
                    if !glue {
                        make_room!(gw);
                    }
                    cur.push(Placed::Text(TextFrag {
                        x,
                        baseline: 0.0,
                        w,
                        text,
                        style,
                        bg,
                        link,
                        uid,
                    }));
                    x += w;
                }
                Piece::Space {
                    style,
                    bg,
                    link,
                    uid,
                    w,
                } => {
                    if cur.is_empty() {
                        continue;
                    }
                    cur.push(Placed::Text(TextFrag {
                        x,
                        baseline: 0.0,
                        w,
                        text: " ".into(),
                        style,
                        bg,
                        link,
                        uid,
                    }));
                    x += w;
                }
                Piece::Break(s) => next_line!(Some(s)),
                Piece::Atomic(i, w) => {
                    if x + w > avail && !cur.is_empty() {
                        next_line!(None);
                    }
                    make_room!(w);
                    cur.push(Placed::Atomic(i, x));
                    x += w;
                }
                Piece::OutOfFlow(i) => {
                    // Remember the static position; the containing block
                    // lays it out later.
                    self.children[i].dims = Dimensions {
                        content: Rect {
                            x: left + x,
                            y: line_y,
                            w: 0.0,
                            h: 0.0,
                        },
                        ..Dimensions::default()
                    };
                }
                Piece::Float(i) => {
                    let fw = self.children[i].dims.margin_box().w;
                    if cur.is_empty() || x + fw <= avail {
                        self.place_float(i, line_y, floats, x0, x1);
                        (left, avail) = floats.space(line_y, band, x0, x1);
                    } else {
                        pending.push(i);
                    }
                }
            }
        }
        if !cur.is_empty() {
            line_y += self.place_line(cur, x, None, left, avail, line_y, &container_style, &mut frags);
        }
        for i in pending {
            self.place_float(i, line_y, floats, x0, x1);
        }
        self.dims.content.h = line_y - content.y;
        if let BoxKind::Anonymous { frags: f, .. } = &mut self.kind {
            *f = frags;
        }
    }

    /// Align one finished line at `y` and move its text and inline-blocks
    /// into place. Returns the line's height.
    #[allow(clippy::too_many_arguments)]
    fn place_line(
        &mut self,
        mut line: Vec<Placed>,
        mut width: f32,
        br: Option<Rc<ComputedStyle>>,
        left: f32,
        avail: f32,
        y: f32,
        container_style: &ComputedStyle,
        frags: &mut Vec<TextFrag>,
    ) -> f32 {
        // Drop trailing spaces.
        while let Some(Placed::Text(t)) = line.last() {
            if t.text == " " {
                width -= t.w;
                line.pop();
            } else {
                break;
            }
        }
        let mut asc = 0.0f32;
        let mut desc = 0.0f32;
        for p in &line {
            let (a, d) = match p {
                Placed::Text(t) => half_leading(&t.style),
                Placed::Atomic(i, _) => {
                    // Align the inline-block's last line baseline with ours.
                    let c = &self.children[*i];
                    let h = c.dims.margin_box().h;
                    let a = c.last_baseline().map(|b| b - c.dims.margin_box().y).unwrap_or(h);
                    (a, h - a)
                }
            };
            asc = asc.max(a);
            desc = desc.max(d);
        }
        if line.is_empty() {
            let s = br.as_deref().unwrap_or(container_style);
            let (a, d) = half_leading(s);
            asc = a;
            desc = d;
        }
        let baseline = y + asc;
        let shift = match container_style.text_align {
            TextAlign::Left => 0.0,
            TextAlign::Center => ((avail - width) / 2.0).max(0.0),
            TextAlign::Right => (avail - width).max(0.0),
        };
        for p in line {
            match p {
                Placed::Text(mut t) => {
                    t.x += left + shift;
                    t.baseline = baseline;
                    frags.push(t);
                }
                Placed::Atomic(i, local_x) => {
                    // Laid out at the origin earlier; move onto the line,
                    // sitting on the baseline.
                    let child = &mut self.children[i];
                    let mb = child.dims.margin_box();
                    let a = child.last_baseline().map(|b| b - mb.y).unwrap_or(mb.h);
                    child.translate(left + shift + local_x - mb.x, baseline - a - mb.y);
                }
            }
        }
        asc + desc
    }

    /// Move float child `i` (laid out at the origin) to the highest spot at
    /// or below `y` where it fits, and add it to `floats`.
    fn place_float(&mut self, i: usize, y: f32, floats: &mut Floats, x0: f32, x1: f32) {
        let child = &mut self.children[i];
        let s = child.style();
        let side = s.float;
        let y = floats.clear_y(s.clear).map_or(y, |cy| y.max(cy));
        let mb = child.dims.margin_box();
        let (fx, fy) = floats.place(mb.w, mb.h, side, y, x0, x1);
        child.translate(fx - mb.x, fy - mb.y);
        floats.add(child.dims.margin_box(), side);
    }
}

/// A box's identity, for maps keyed by box.
pub fn box_addr(b: &LayoutBox) -> usize {
    b as *const LayoutBox as usize
}

/// What each box is clipped to by `overflow` on its ancestors, by
/// [`box_addr`] (boxes that aren't clipped aren't listed). A box's own
/// `overflow` clips what's inside it, not itself. Fixed boxes escape all
/// clipping, and absolutely positioned ones escape the clipping of boxes
/// outside their containing block, as in CSS.
pub fn clip_map(root: &LayoutBox) -> HashMap<usize, Rect> {
    fn walk(b: &LayoutBox, clip: Option<Rect>, abs_clip: Option<Rect>, out: &mut HashMap<usize, Rect>) {
        let block = matches!(b.kind, BoxKind::Block(_));
        let inner = if block && b.style().overflow_clip {
            let p = b.dims.padding_box();
            Some(clip.map_or(p, |c| c.intersect(p)))
        } else {
            clip
        };
        let inner_abs = if block && b.position() != Position::Static {
            inner
        } else {
            abs_clip
        };
        for c in &b.children {
            let (cc, cabs) = match c.position() {
                Position::Fixed => (None, None),
                Position::Absolute => (inner_abs, inner_abs),
                _ => (inner, inner_abs),
            };
            if let Some(r) = cc {
                out.insert(box_addr(c), r);
            }
            walk(c, cc, cabs, out);
        }
    }
    let mut out = HashMap::new();
    walk(root, None, None, &mut out);
    out
}

/// Every box in the order it's painted, back to front (CSS 2.1 Appendix E,
/// simplified): within each stacking context, positioned boxes with a
/// negative `z-index`, then the normal flow in tree order, then floats, then
/// the other positioned boxes by `z-index`. Each float and positioned box is
/// painted as a unit, with everything inside it.
pub fn paint_order<'b, 'a>(root: &'b LayoutBox<'a>) -> Vec<&'b LayoutBox<'a>> {
    type Boxes<'b, 'a> = Vec<&'b LayoutBox<'a>>;
    fn walk<'b, 'a>(
        b: &'b LayoutBox<'a>,
        flow: &mut Boxes<'b, 'a>,
        floats: &mut Boxes<'b, 'a>,
        layers: &mut Vec<(i32, &'b LayoutBox<'a>)>,
    ) {
        for c in &b.children {
            if c.position() != Position::Static {
                layers.push((c.style().z_index.unwrap_or(0), c));
            } else if c.is_float() {
                floats.push(c);
            } else {
                flow.push(c);
                walk(c, flow, floats, layers);
            }
        }
    }
    fn stack<'b, 'a>(b: &'b LayoutBox<'a>, out: &mut Boxes<'b, 'a>) {
        out.push(b);
        let (mut flow, mut floats, mut layers) = (Vec::new(), Vec::new(), Vec::new());
        walk(b, &mut flow, &mut floats, &mut layers);
        // Stable, so equal `z-index` keeps tree order.
        layers.sort_by_key(|(z, _)| *z);
        let split = layers.partition_point(|(z, _)| *z < 0);
        for (_, c) in &layers[..split] {
            stack(c, out);
        }
        out.extend(flow);
        for f in floats {
            stack(f, out);
        }
        for (_, c) in &layers[split..] {
            stack(c, out);
        }
    }
    let mut out = Vec::new();
    stack(root, &mut out);
    out
}

/// A piece of a line, positioned relative to the line's start.
enum Placed {
    Text(TextFrag),
    /// An inline-block child, and its x on the line.
    Atomic(usize, f32),
}

// ---------------------------------------------------------------------------
// Floats
// ---------------------------------------------------------------------------

/// Past this many floats in one formatting context, more are still laid out
/// but text no longer flows around them, so hostile pages can't make every
/// line check thousands of floats.
const MAX_FLOATS: usize = 256;

/// The floats placed so far in one block formatting context, as margin boxes
/// in page coordinates.
#[derive(Default)]
struct Floats {
    items: Vec<(Rect, Float)>,
}

impl Floats {
    fn overlapping(&self, y: f32, h: f32) -> impl Iterator<Item = &(Rect, Float)> {
        self.items
            .iter()
            .filter(move |(r, _)| r.y < y + h && r.y + r.h > y)
    }

    /// The part of `x0..x1` that floats leave free in the band `y..y + h`.
    fn space(&self, y: f32, h: f32, x0: f32, x1: f32) -> (f32, f32) {
        let (mut l, mut r) = (x0, x1);
        for (f, side) in self.overlapping(y, h) {
            match side {
                Float::Left => l = l.max(f.x + f.w),
                _ => r = r.min(f.x),
            }
        }
        (l, r.max(l) - l)
    }

    /// The nearest bottom edge, below `y`, of a float in the band.
    fn next_bottom(&self, y: f32, h: f32) -> Option<f32> {
        self.overlapping(y, h).map(|(r, _)| r.y + r.h).reduce(f32::min)
    }

    /// The bottom of the floats `clear` has to go below.
    fn clear_y(&self, clear: Clear) -> Option<f32> {
        self.items
            .iter()
            .filter(|(_, side)| match clear {
                Clear::None => false,
                Clear::Left => *side == Float::Left,
                Clear::Right => *side == Float::Right,
                Clear::Both => true,
            })
            .map(|(r, _)| r.y + r.h)
            .reduce(f32::max)
    }

    fn bottom(&self) -> Option<f32> {
        self.clear_y(Clear::Both)
    }

    /// Where a `w` × `h` float goes: as high as possible at or below `y`
    /// (and never above an earlier float), as far to its side as the other
    /// floats allow. Returns its margin box's top-left corner.
    fn place(&self, w: f32, h: f32, side: Float, y: f32, x0: f32, x1: f32) -> (f32, f32) {
        let mut y = self.items.last().map_or(y, |(r, _)| y.max(r.y));
        let h = h.max(0.01);
        // Each step moves below at least one float, so this ends.
        for _ in 0..=self.items.len() {
            let (l, avail) = self.space(y, h, x0, x1);
            let next = self.next_bottom(y, h);
            if avail >= w || next.is_none() {
                return match side {
                    Float::Right => (l + avail - w, y),
                    _ => (l, y),
                };
            }
            y = next.unwrap_or(y);
        }
        (x0, y)
    }

    fn add(&mut self, r: Rect, side: Float) {
        if self.items.len() < MAX_FLOATS {
            self.items.push((r, side));
        }
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// Hostile pages can ask for huge colspans; columns past this are dropped.
const MAX_COLS: usize = 1000;

/// A row of the table, as a path from the table box.
#[derive(Clone, Copy)]
enum RowPath {
    Direct(usize),
    Grouped(usize, usize),
}

struct GridCell {
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
    /// Index into the row box's children.
    idx: usize,
}

/// A table's grid and what its columns want.
struct TableCols {
    rows: Vec<RowPath>,
    cells: Vec<GridCell>,
    min: Vec<f32>,
    max: Vec<f32>,
    /// Percentage widths from `width: N%` cells.
    pct: Vec<Option<f32>>,
    spacing: [f32; 2],
}

impl TableCols {
    fn ncols(&self) -> usize {
        self.min.len()
    }

    /// Horizontal space taken by `border-spacing`.
    fn spacing_w(&self) -> f32 {
        if self.ncols() == 0 {
            0.0
        } else {
            self.spacing[0] * (self.ncols() + 1) as f32
        }
    }

    /// Min and max content-box widths of the table.
    fn content_widths(&self) -> (f32, f32) {
        let sp = self.spacing_w();
        (
            self.min.iter().sum::<f32>() + sp,
            self.max.iter().sum::<f32>() + sp,
        )
    }

    /// Column widths that add up to `avail` where possible (CSS 2.1 §17.5.2.2,
    /// as browsers do it): percentage columns first, then the rest between
    /// their min and max, then any extra in proportion to their max.
    fn distribute(&self, avail: f32) -> Vec<f32> {
        let n = self.ncols();
        let mut w = vec![0.0f32; n];
        let mut rem = avail;
        for (c, wc) in w.iter_mut().enumerate() {
            if let Some(p) = self.pct[c] {
                *wc = (avail * p / 100.0).max(self.min[c]);
                rem -= *wc;
            }
        }
        let auto: Vec<usize> = (0..n).filter(|&c| self.pct[c].is_none()).collect();
        let smin: f32 = auto.iter().map(|&c| self.min[c]).sum();
        let smax: f32 = auto.iter().map(|&c| self.max[c]).sum();
        if auto.is_empty() {
            let total: f32 = w.iter().sum();
            if rem > 0.0 && total > 0.0 {
                w.iter_mut().for_each(|x| *x += rem * *x / total);
            }
        } else if rem <= smin {
            auto.iter().for_each(|&c| w[c] = self.min[c]);
        } else if rem <= smax {
            let f = (rem - smin) / (smax - smin);
            auto.iter()
                .for_each(|&c| w[c] = self.min[c] + (self.max[c] - self.min[c]) * f);
        } else {
            let extra = rem - smax;
            for &c in &auto {
                let share = if smax > 0.0 {
                    self.max[c] / smax
                } else {
                    1.0 / auto.len() as f32
                };
                w[c] = self.max[c] + extra * share;
            }
        }
        w
    }
}

/// Grow `cols[range]` so they add up to at least `target`, sharing the
/// difference in proportion to `weights` (or equally).
fn grow(cols: &mut [f32], weights: &[f32], target: f32) {
    let have: f32 = cols.iter().sum();
    if target <= have || cols.is_empty() {
        return;
    }
    let total: f32 = weights.iter().sum();
    let n = cols.len() as f32;
    for (c, w) in cols.iter_mut().zip(weights) {
        let share = if total > 0.0 { w / total } else { 1.0 / n };
        *c += (target - have) * share;
    }
}

fn span_attr(b: &LayoutBox, name: &str) -> Option<usize> {
    match b.kind {
        BoxKind::Block(n) => n.node.element()?.attrs.get(name)?.trim().parse().ok(),
        _ => None,
    }
}

impl<'a> LayoutBox<'a> {
    fn row(&self, p: RowPath) -> &LayoutBox<'a> {
        match p {
            RowPath::Direct(i) => &self.children[i],
            RowPath::Grouped(g, i) => &self.children[g].children[i],
        }
    }

    fn row_mut(&mut self, p: RowPath) -> &mut LayoutBox<'a> {
        match p {
            RowPath::Direct(i) => &mut self.children[i],
            RowPath::Grouped(g, i) => &mut self.children[g].children[i],
        }
    }

    fn table_part(&self) -> Display {
        match self.kind {
            BoxKind::Block(n) => match n.style.display {
                d @ (Display::TableCaption | Display::TableRowGroup) => d,
                _ => Display::TableRow,
            },
            BoxKind::Anonymous { .. } => Display::TableRow,
        }
    }

    /// Place every cell in the grid, then work out each column's min, max
    /// and percentage width from the cells in it.
    fn table_columns(&self, cb_w: f32) -> TableCols {
        let s = self.style();
        let spacing = if s.border_collapse {
            [0.0; 2]
        } else {
            s.border_spacing
        };
        // Rows, with the index where each one's row group ends (rowspans
        // don't cross groups).
        let mut rows = Vec::new();
        let mut group_end = Vec::new();
        for (i, c) in self.children.iter().enumerate() {
            match c.table_part() {
                Display::TableCaption => {}
                Display::TableRowGroup => {
                    // Rows outside a group, just before this one, end here.
                    group_end.resize(rows.len(), rows.len());
                    rows.extend((0..c.children.len()).map(|j| RowPath::Grouped(i, j)));
                    group_end.resize(rows.len(), rows.len());
                }
                _ => rows.push(RowPath::Direct(i)),
            }
        }
        group_end.resize(rows.len(), rows.len());

        // `busy[c]`: the first row that column `c` is free again in.
        let mut busy: Vec<usize> = Vec::new();
        let mut cells = Vec::new();
        for (r, &p) in rows.iter().enumerate() {
            let mut col = 0;
            for (idx, cell) in self.row(p).children.iter().enumerate() {
                while col < busy.len() && busy[col] > r {
                    col += 1;
                }
                if col >= MAX_COLS {
                    break;
                }
                let colspan = span_attr(cell, "colspan").unwrap_or(1).clamp(1, MAX_COLS - col);
                let left = group_end[r] - r;
                let rowspan = match span_attr(cell, "rowspan") {
                    Some(0) => left,
                    Some(n) => n.min(left),
                    None => 1,
                }
                .max(1);
                if busy.len() < col + colspan {
                    busy.resize(col + colspan, 0);
                }
                busy[col..col + colspan].iter_mut().for_each(|b| *b = r + rowspan);
                cells.push(GridCell {
                    row: r,
                    col,
                    colspan,
                    rowspan,
                    idx,
                });
                col += colspan;
            }
        }

        let n = busy.len();
        let mut t = TableCols {
            rows,
            cells,
            min: vec![0.0; n],
            max: vec![0.0; n],
            pct: vec![None; n],
            spacing,
        };
        let widths: Vec<(f32, f32)> = t
            .cells
            .iter()
            .map(|c| self.row(t.rows[c.row]).children[c.idx].intrinsic_widths(cb_w))
            .collect();
        for (c, &(min, max)) in t.cells.iter().zip(&widths) {
            if c.colspan == 1 {
                t.min[c.col] = t.min[c.col].max(min);
                t.max[c.col] = t.max[c.col].max(max);
                if let Len::Percent(p) = self.row(t.rows[c.row]).children[c.idx].style().width {
                    t.pct[c.col] = Some(t.pct[c.col].map_or(p, |q| q.max(p)));
                }
            }
        }
        // Spanning cells, narrowest span first, widen the columns they cover.
        let mut spanning: Vec<usize> = (0..t.cells.len()).filter(|&i| t.cells[i].colspan > 1).collect();
        spanning.sort_by_key(|&i| t.cells[i].colspan);
        for i in spanning {
            let c = &t.cells[i];
            let r = c.col..c.col + c.colspan;
            let gaps = spacing[0] * (c.colspan - 1) as f32;
            let weights: Vec<f32> = t.max[r.clone()].to_vec();
            grow(&mut t.min[r.clone()], &weights, widths[i].0 - gaps);
            let weights: Vec<f32> = t.max[r.clone()].to_vec();
            grow(&mut t.max[r], &weights, widths[i].1 - gaps);
        }
        for c in 0..n {
            t.max[c] = t.max[c].max(t.min[c]);
        }
        t
    }

    /// Lay out captions and the cell grid inside this table's content box,
    /// whose width is already set.
    fn layout_table(&mut self, t: TableCols) {
        let content = self.dims.content;
        let [hs, vs] = t.spacing;
        let mut y = content.y;
        for c in &mut self.children {
            if c.table_part() == Display::TableCaption {
                c.layout_block(content.x, content.w, y, None);
                let mb = c.dims.margin_box();
                y = mb.y + mb.h;
            }
        }

        let widths = t.distribute(content.w - t.spacing_w());
        let mut col_x = Vec::with_capacity(widths.len());
        let mut x = content.x + hs;
        for w in &widths {
            col_x.push(x);
            x += w + hs;
        }
        let span_w =
            |c: &GridCell| widths[c.col..c.col + c.colspan].iter().sum::<f32>() + hs * (c.colspan - 1) as f32;

        // Lay each cell out at its width (at y = 0 for now) to get its height.
        let mut heights = Vec::with_capacity(t.cells.len());
        for c in &t.cells {
            let cell = &mut self.row_mut(t.rows[c.row]).children[c.idx];
            cell.layout_at(col_x[c.col], 0.0, span_w(c));
            heights.push(cell.dims.margin_box().h);
        }
        let mut row_h: Vec<f32> = t
            .rows
            .iter()
            .map(|&p| match self.row(p).style().height {
                Len::Px(h) if matches!(self.row(p).kind, BoxKind::Block(_)) => h,
                _ => 0.0,
            })
            .collect();
        for (c, &h) in t.cells.iter().zip(&heights) {
            if c.rowspan == 1 {
                row_h[c.row] = row_h[c.row].max(h);
            }
        }
        // A cell spanning rows makes the last of them taller if it needs to.
        for (c, &h) in t.cells.iter().zip(&heights) {
            if c.rowspan > 1 {
                let last = c.row + c.rowspan - 1;
                let have = row_h[c.row..=last].iter().sum::<f32>() + vs * (c.rowspan - 1) as f32;
                if h > have {
                    row_h[last] += h - have;
                }
            }
        }

        if !t.rows.is_empty() {
            y += vs;
        }
        let mut row_y = Vec::with_capacity(row_h.len());
        for h in &row_h {
            row_y.push(y);
            y += h + vs;
        }
        let grid_w = if widths.is_empty() {
            0.0
        } else {
            x - hs - (content.x + hs)
        };

        // Move cells into place, stretched to their rows' height, with their
        // content aligned vertically.
        for (c, &h) in t.cells.iter().zip(&heights) {
            let target = row_h[c.row..c.row + c.rowspan].iter().sum::<f32>() + vs * (c.rowspan - 1) as f32;
            let cell = &mut self.row_mut(t.rows[c.row]).children[c.idx];
            let top = cell.dims.margin_box().y;
            cell.translate(0.0, row_y[c.row] - top);
            let extra = (target - h).max(0.0);
            cell.dims.content.h += extra;
            let shift = match cell.style().vertical_align {
                VAlign::Top => 0.0,
                VAlign::Middle => extra / 2.0,
                VAlign::Bottom => extra,
            };
            if shift > 0.0 {
                cell.translate_contents(shift);
            }
        }
        for (r, &p) in t.rows.iter().enumerate() {
            let row = self.row_mut(p);
            row.dims.content = Rect {
                x: content.x + hs,
                y: row_y[r],
                w: grid_w,
                h: row_h[r],
            };
        }
        // Row groups cover their rows.
        let mut end = y;
        for g in &mut self.children {
            if g.table_part() != Display::TableRowGroup {
                continue;
            }
            let (top, bottom) = match (g.children.first(), g.children.last()) {
                (Some(a), Some(b)) => (a.dims.content.y, b.dims.content.y + b.dims.content.h),
                _ => (end, end),
            };
            g.dims.content = Rect {
                x: content.x + hs,
                y: top,
                w: grid_w,
                h: bottom - top,
            };
            end = bottom;
        }
        self.dims.content.h = y - content.y;
    }
}

impl LayoutBox<'_> {
    /// Baseline of the last line of text inside this box, if any.
    fn last_baseline(&self) -> Option<f32> {
        if let BoxKind::Anonymous { frags, .. } = &self.kind {
            if let Some(f) = frags.last() {
                return Some(f.baseline);
            }
        }
        self.children
            .iter()
            .rev()
            .filter(|c| !c.out_of_flow() && !c.is_float())
            .find_map(|c| c.last_baseline())
    }
}

/// Does UAX #14 allow a line break between characters `a` and `b`?
fn break_between(a: char, b: char) -> bool {
    let mut s = String::with_capacity(8);
    s.push(a);
    s.push(b);
    let found = unicode_linebreak::linebreaks(&s).any(|(i, _)| i == a.len_utf8());
    found
}

struct FlexItem {
    grow: f32,
    shrink: f32,
    basis: Len,
    align: Option<Align>,
    order: i32,
    auto_width: bool,
    auto_height: bool,
}

/// Where `n` things start, and the extra space between each, when `free`
/// space is shared out by `j`. With no space to share, everything starts at
/// the start (or overflows evenly when centred, or at the end).
fn spread(j: Justify, free: f32, n: usize) -> (f32, f32) {
    let n = n.max(1) as f32;
    match j {
        Justify::Start | Justify::Stretch => (0.0, 0.0),
        Justify::End => (free, 0.0),
        Justify::Center => (free / 2.0, 0.0),
        _ if free <= 0.0 => (0.0, 0.0),
        Justify::SpaceBetween if n > 1.0 => (0.0, free / (n - 1.0)),
        Justify::SpaceBetween => (0.0, 0.0),
        Justify::SpaceAround => (free / n / 2.0, free / n),
        Justify::SpaceEvenly => (free / (n + 1.0), free / (n + 1.0)),
    }
}

fn margins_h(b: &LayoutBox, cb_w: f32) -> f32 {
    let s = b.style();
    s.margin[1].or_zero(cb_w) + s.margin[3].or_zero(cb_w)
}

/// Content size of an image: its natural size, or the width/height given in
/// CSS or attributes, keeping its aspect ratio where only one is given;
/// limited by `max-width`. `forced_w` is a width imposed by the parent.
fn replaced_size(s: &ComputedStyle, natural: (u32, u32), cb_w: f32, forced_w: Option<f32>) -> (f32, f32) {
    let (nw, nh) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    let css_h = match s.height {
        Len::Px(h) => Some(h),
        _ => None,
    };
    let css_w = forced_w.or_else(|| s.width.resolve(cb_w));
    let (mut w, mut h) = match (css_w, css_h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w * nh / nw),
        (None, Some(h)) => (h * nw / nh, h),
        (None, None) => (nw, nh),
    };
    if let Some(max) = s.max_width.resolve(cb_w) {
        if w > max {
            if css_h.is_none() {
                h = h * max / w;
            }
            w = max;
        }
    }
    (w.max(0.0), h.max(0.0))
}

/// Ascent/descent of a line-height box for this style (CSS half-leading).
fn half_leading(s: &ComputedStyle) -> (f32, f32) {
    let (a, d) = font::vmetrics(font::index(s), s.font_size);
    let lead = (s.line_height - (a + d)) / 2.0;
    (a + lead, d + lead)
}

#[cfg(test)]
mod line_break_tests {
    use crate::css;
    use crate::html;
    use crate::layout::{layout_tree, BoxKind, LayoutBox};
    use crate::style::{self, StyleContext};

    /// Lay out `body` at `width` and return the text of each line.
    fn lines(body: &str, width: f32) -> Vec<String> {
        let dom = html::parse(&format!(
            "<body style='margin:0;font-family:sans-serif;font-size:16px'>{body}"
        ));
        let ua = css::parse(style::UA_CSS);
        let ctx = StyleContext {
            sheets: vec![(0, &ua)],
            viewport: (width, 600.0),
        };
        let styled = style::style_tree(&dom, &ctx);
        let root = layout_tree(&styled, width);
        let mut frags = Vec::new();
        fn walk(b: &LayoutBox, out: &mut Vec<(f32, f32, f32, String)>) {
            if let BoxKind::Anonymous { frags, .. } = &b.kind {
                out.extend(frags.iter().map(|f| (f.baseline, f.x, f.w, f.text.clone())));
            }
            for c in &b.children {
                walk(c, out);
            }
        }
        walk(&root, &mut frags);
        frags.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut out: Vec<(f32, String, f32)> = Vec::new();
        for (base, x, w, t) in frags {
            match out.last_mut() {
                Some((b, s, right)) if (*b - base).abs() < 0.5 => {
                    s.push_str(&t);
                    *right = right.max(x + w);
                }
                _ => out.push((base, t, x + w)),
            }
        }
        for (_, s, right) in &out {
            // A line may only overflow if it's a single unbreakable piece.
            assert!(
                *right <= width + 0.5 || !s.trim().contains(' '),
                "overflow: {s:?} ({right} > {width})"
            );
        }
        out.into_iter()
            .map(|(_, s, _)| s.trim_end().to_string())
            .collect()
    }

    #[test]
    fn cjk_wraps_between_characters() {
        let l = lines("猫咪是最好的浏览器吉祥物。我们喜欢它。", 120.0);
        assert!(l.len() > 1, "{l:?}");
        assert!(
            l.iter().all(|line| !line.starts_with('。')),
            "no line starts with 。: {l:?}"
        );
    }

    #[test]
    fn style_changes_dont_split_words() {
        // "foo" + bold "bar" is one word: it must stay together.
        let l = lines("aaaa bbbb foo<b>bar</b>", 95.0);
        assert!(l.iter().any(|line| line.contains("foobar")), "{l:?}");
    }

    #[test]
    fn punctuation_and_urls() {
        let l = lines("Hello world!", 50.0);
        assert!(l.iter().all(|line| line != "!"), "! stays with its word: {l:?}");
        let l = lines("see https://example.com/some/long/path/to/a/page now", 120.0);
        assert!(l.len() >= 3, "a long URL may break: {l:?}");
        let l = lines("well-known state-of-the-art", 80.0);
        assert!(
            l.iter().any(|line| line.ends_with('-')),
            "breaks after hyphens: {l:?}"
        );
    }
}

#[cfg(test)]
mod table_tests {
    use crate::css;
    use crate::html;
    use crate::layout::{layout_tree, BoxKind, LayoutBox, Rect};
    use crate::style::{self, StyleContext};

    /// Lay out `body` at `width`; return the border boxes of every element
    /// with the given tag, in document order.
    pub(super) fn boxes(body: &str, width: f32, tag: &str) -> Vec<Rect> {
        let dom = html::parse(&format!(
            "<body style='margin:0;font-family:sans-serif;font-size:16px'>{body}"
        ));
        let ua = css::parse(style::UA_CSS);
        let ctx = StyleContext {
            sheets: vec![(0, &ua)],
            viewport: (width, 600.0),
        };
        let styled = style::style_tree(&dom, &ctx);
        let root = layout_tree(&styled, width);
        fn walk(b: &LayoutBox, tag: &str, out: &mut Vec<Rect>) {
            if let BoxKind::Block(n) = b.kind {
                if n.node.element().is_some_and(|e| e.tag == tag) {
                    out.push(b.dims.border_box());
                }
            }
            for c in &b.children {
                walk(c, tag, out);
            }
        }
        let mut out = Vec::new();
        walk(&root, tag, &mut out);
        out
    }

    pub(super) fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn columns_line_up_across_rows() {
        let td = boxes(
            "<table><tr><td>a<td>bbbbbbbbbbbb<tr><td>cccccccccccc<td>d</table>",
            800.0,
            "td",
        );
        assert_eq!(td.len(), 4);
        assert!(close(td[0].x, td[2].x) && close(td[1].x, td[3].x), "{td:?}");
        assert!(close(td[0].w, td[2].w) && close(td[1].w, td[3].w), "{td:?}");
        // Default border-spacing is 2px.
        assert!(close(td[1].x, td[0].x + td[0].w + 2.0), "{td:?}");
        assert!(close(td[2].y, td[0].y + td[0].h + 2.0), "{td:?}");
    }

    #[test]
    fn auto_width_shrinks_and_percent_width_fills() {
        let t = boxes("<table><tr><td>short</table>", 800.0, "table");
        assert!(t[0].w < 100.0, "{t:?}");
        let t = boxes("<table width=100%><tr><td>short</table>", 800.0, "table");
        assert!(close(t[0].w, 800.0), "{t:?}");
        let t = boxes("<table width=400 align=center><tr><td>x</table>", 800.0, "table");
        assert!(close(t[0].w, 400.0) && close(t[0].x, 200.0), "{t:?}");
    }

    #[test]
    fn long_text_wraps_inside_the_viewport() {
        let words = "lorem ipsum dolor sit amet ".repeat(40);
        let td = boxes(&format!("<table><tr><td>{words}<td>{words}</table>"), 300.0, "td");
        let right = td[1].x + td[1].w;
        assert!(right <= 300.5, "{td:?}");
        assert!(td[0].h > 100.0, "the text wrapped: {td:?}");
    }

    #[test]
    fn colspan_covers_its_columns() {
        let td = boxes(
            "<table cellspacing=0><tr><td colspan=2>wide heading<tr><td>aaa<td>bbbbbb</table>",
            800.0,
            "td",
        );
        assert!(close(td[0].w, td[1].w + td[2].w), "{td:?}");
        assert!(close(td[0].x, td[1].x), "{td:?}");
    }

    #[test]
    fn rowspan_stretches_across_rows() {
        let td = boxes(
            "<table><tr><td rowspan=2>tall<td>one<tr><td>two</table>",
            800.0,
            "td",
        );
        let (tall, one, two) = (td[0], td[1], td[2]);
        assert!(close(tall.y, one.y), "{td:?}");
        assert!(close(tall.y + tall.h, two.y + two.h), "{td:?}");
        assert!(
            close(two.x, one.x),
            "the second row skips the spanned column: {td:?}"
        );
    }

    #[test]
    fn cells_stretch_to_the_row_and_center_vertically() {
        let td = boxes(
            "<table><tr><td>a<br>b<br>c<td>x<td valign=top>y</table>",
            800.0,
            "td",
        );
        assert!(close(td[0].h, td[1].h) && close(td[1].h, td[2].h), "{td:?}");
    }

    #[test]
    fn cellpadding_cellspacing_and_collapse() {
        let td = boxes(
            "<table cellspacing=0 cellpadding=10><tr><td>a<td>b</table>",
            800.0,
            "td",
        );
        assert!(close(td[1].x, td[0].x + td[0].w), "{td:?}");
        let inner = boxes("<table cellpadding=10><tr><td>a</table>", 800.0, "td")[0];
        let plain = boxes("<table cellpadding=0><tr><td>a</table>", 800.0, "td")[0];
        assert!(close(inner.w, plain.w + 20.0), "{inner:?} {plain:?}");
        let td = boxes(
            "<table style='border-collapse:collapse'><tr><td>a<td>b</table>",
            800.0,
            "td",
        );
        assert!(close(td[1].x, td[0].x + td[0].w), "{td:?}");
    }

    #[test]
    fn head_and_foot_move_to_the_edges() {
        let page = "<table><tfoot><tr><td>foot</tfoot><tbody><tr><td>body</tbody><thead><tr><td>head</thead></table>";
        let y = |tag| boxes(page, 800.0, tag)[0].y;
        assert!(y("thead") < y("tbody") && y("tbody") < y("tfoot"));
    }

    #[test]
    fn caption_sits_above_the_rows() {
        let cap = boxes(
            "<table><caption>Title</caption><tr><td>x</table>",
            800.0,
            "caption",
        );
        let td = boxes("<table><caption>Title</caption><tr><td>x</table>", 800.0, "td");
        assert!(cap[0].y + cap[0].h <= td[0].y, "{cap:?} {td:?}");
    }

    #[test]
    fn css_tables_without_rows() {
        let d = boxes(
            "<div style='display:table'><div style='display:table-cell'>a</div>\
             <div style='display:table-cell'>bbbb</div></div>",
            800.0,
            "div",
        );
        // An anonymous row holds both cells side by side.
        assert!(close(d[1].y, d[2].y) && d[2].x > d[1].x + d[1].w - 0.5, "{d:?}");
    }

    #[test]
    fn percent_tables_inside_percent_tables() {
        let t = boxes(
            "<table width=50%><tr><td><table width=100%><tr><td>x</table></table>",
            800.0,
            "table",
        );
        assert!(close(t[0].w, 400.0), "{t:?}");
        assert!(t[1].w < 400.0 && t[1].w > 380.0, "{t:?}");
    }

    #[test]
    fn nested_tables() {
        let t = boxes(
            "<table><tr><td><table><tr><td>inner a<td>inner b</table><td>outer</table>",
            800.0,
            "table",
        );
        assert_eq!(t.len(), 2);
        assert!(
            t[1].x >= t[0].x && t[1].x + t[1].w <= t[0].x + t[0].w + 0.5,
            "{t:?}"
        );
    }

    #[test]
    fn hostile_spans_are_bounded() {
        let td = boxes(
            "<table><tr><td colspan=99999999>x<td>y<tr><td rowspan=99999999>z</table>",
            800.0,
            "td",
        );
        assert_eq!(td.len(), 3);
        let many = "<td>x".repeat(5000);
        let td = boxes(&format!("<table><tr>{many}</table>"), 800.0, "td");
        assert_eq!(td.len(), 5000);
    }
}

#[cfg(test)]
mod float_tests {
    use super::table_tests::{boxes, close};
    use crate::css;
    use crate::html;
    use crate::layout::{layout_tree, BoxKind, LayoutBox};
    use crate::style::{self, StyleContext};

    /// Lay out `body` at `width`; return (x, right edge, baseline) of each
    /// text fragment that isn't a space.
    fn words(body: &str, width: f32) -> Vec<(f32, f32, f32, String)> {
        let dom = html::parse(&format!(
            "<body style='margin:0;font-family:sans-serif;font-size:16px'>{body}"
        ));
        let ua = css::parse(style::UA_CSS);
        let ctx = StyleContext {
            sheets: vec![(0, &ua)],
            viewport: (width, 600.0),
        };
        let styled = style::style_tree(&dom, &ctx);
        let root = layout_tree(&styled, width);
        fn walk(b: &LayoutBox, out: &mut Vec<(f32, f32, f32, String)>) {
            if let BoxKind::Anonymous { frags, .. } = &b.kind {
                out.extend(
                    frags
                        .iter()
                        .filter(|f| f.text.trim() != "")
                        .map(|f| (f.x, f.x + f.w, f.baseline, f.text.clone())),
                );
            }
            for c in &b.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(&root, &mut out);
        out
    }

    const TEXT: &str = "one two three four five six seven eight nine ten eleven twelve \
        thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";

    #[test]
    fn text_wraps_beside_a_left_float() {
        let page =
            format!("<div style='float:left;width:100px;height:60px'></div><p style='margin:0'>{TEXT}");
        let w = words(&page, 300.0);
        let beside: Vec<_> = w.iter().filter(|w| w.2 < 60.0).collect();
        let below: Vec<_> = w.iter().filter(|w| w.2 > 80.0).collect();
        assert!(!beside.is_empty() && !below.is_empty(), "{w:?}");
        assert!(beside.iter().all(|w| w.0 >= 100.0 - 0.5), "{beside:?}");
        assert!(
            below.iter().any(|w| w.0 < 1.0),
            "lines below the float start at the edge: {below:?}"
        );
    }

    #[test]
    fn text_wraps_beside_a_right_float() {
        let page = format!("<div style='float:right;width:100px;height:60px'></div>{TEXT}");
        let w = words(&page, 300.0);
        assert!(w.iter().filter(|w| w.2 < 60.0).all(|w| w.1 <= 200.5), "{w:?}");
        let d = boxes(&page, 300.0, "div");
        assert!(close(d[0].x, 200.0), "{d:?}");
    }

    #[test]
    fn floats_stack_side_by_side_then_drop() {
        let f = "<div style='float:left;width:120px;height:20px'></div>";
        let d = boxes(&f.repeat(3), 300.0, "div");
        assert!(close(d[0].x, 0.0) && close(d[1].x, 120.0), "{d:?}");
        assert!(close(d[1].y, d[0].y), "{d:?}");
        // The third doesn't fit beside the other two.
        assert!(close(d[2].x, 0.0) && close(d[2].y, 20.0), "{d:?}");
    }

    #[test]
    fn clear_moves_below_floats() {
        let d = boxes(
            "<div style='float:left;width:50px;height:80px'></div><div style='clear:both'>x</div>",
            300.0,
            "div",
        );
        assert!(d[1].y >= 80.0 - 0.5, "{d:?}");
        let w = words(
            "<div style='float:left;width:50px;height:80px'></div>a<br clear=all>b",
            300.0,
        );
        let b = w.iter().find(|w| w.3 == "b").unwrap();
        assert!(b.0 < 1.0 && b.2 > 80.0, "{w:?}");
    }

    #[test]
    fn only_new_formatting_contexts_contain_floats() {
        let float = "<div style='float:left;width:50px;height:80px'></div>";
        let plain = boxes(&format!("<section>{float}</section>"), 300.0, "section");
        assert!(close(plain[0].h, 0.0), "{plain:?}");
        let hidden = boxes(
            &format!("<section style='overflow:hidden'>{float}</section>"),
            300.0,
            "section",
        );
        assert!(close(hidden[0].h, 80.0), "{hidden:?}");
        // The root element is a formatting context, so the page is tall enough.
        let root = boxes(float, 300.0, "html");
        assert!(root[0].h >= 80.0 - 0.5, "{root:?}");
    }

    #[test]
    fn new_formatting_context_sits_beside_a_float() {
        let d = boxes(
            "<div style='float:left;width:100px;height:50px'></div><section style='overflow:hidden'>x</section>",
            300.0,
            "section",
        );
        assert!(close(d[0].x, 100.0) && close(d[0].w, 200.0), "{d:?}");
    }

    #[test]
    fn float_in_the_middle_of_a_line() {
        let w = words(
            "aaa <span style='float:right;width:40px;height:40px'></span>bbb",
            300.0,
        );
        // It fits on the first line, so both words stay there.
        assert!(close(w[0].2, w[1].2), "{w:?}");
        let s = boxes(
            "aaa <span style='float:right;width:40px;height:40px'></span>bbb",
            300.0,
            "span",
        );
        assert!(close(s[0].x, 260.0) && close(s[0].y, 0.0), "{s:?}");
    }

    #[test]
    fn shrink_to_fit_and_align_attribute() {
        let d = boxes("<div style='float:left'>short</div>", 300.0, "div");
        assert!(d[0].w < 100.0, "{d:?}");
        let t = boxes("<table align=right><tr><td>x</table>text", 300.0, "table");
        assert!(close(t[0].x + t[0].w, 300.0), "{t:?}");
    }

    #[test]
    fn many_floats_are_bounded() {
        let f = "<div style='float:left;width:3px;height:3px'></div>".repeat(3000);
        let d = boxes(&format!("{f}{TEXT}"), 300.0, "div");
        assert_eq!(d.len(), 3000);
    }
}

#[cfg(test)]
mod position_tests {
    use super::table_tests::{boxes, close};
    use crate::css;
    use crate::html;
    use crate::layout::{layout_tree, paint_order, BoxKind};
    use crate::style::{self, StyleContext};

    #[test]
    fn relative_shifts_without_moving_neighbours() {
        let page = "<div>a</div><div style='position:relative;left:10px;top:5px'>b</div><div>c</div>";
        let d = boxes(page, 300.0, "div");
        let plain = boxes("<div>a</div><div>b</div><div>c</div>", 300.0, "div");
        assert!(
            close(d[1].x, plain[1].x + 10.0) && close(d[1].y, plain[1].y + 5.0),
            "{d:?}"
        );
        assert!(close(d[2].y, plain[2].y), "the next block doesn't move: {d:?}");
        let r = boxes(
            "<div style='position:relative;right:10px;bottom:5px'>b</div>",
            300.0,
            "div",
        );
        assert!(close(r[0].x, -10.0) && close(r[0].y, -5.0), "{r:?}");
    }

    #[test]
    fn absolute_is_out_of_flow_and_placed_in_its_containing_block() {
        let page = "<div style='position:relative;margin-left:50px;width:200px;height:100px'>\
             <p style='position:absolute;top:10px;left:20px;margin:0'>x</p></div><div>after</div>";
        let d = boxes(page, 400.0, "div");
        let p = boxes(page, 400.0, "p");
        assert!(close(p[0].x, 70.0) && close(p[0].y, 10.0), "{p:?}");
        assert!(close(d[1].y, 100.0), "takes no space: {d:?}");
        assert!(p[0].w < 50.0, "shrinks to fit: {p:?}");
    }

    #[test]
    fn right_bottom_and_stretching() {
        let cb = "position:relative;width:300px;height:200px";
        let p = boxes(
            &format!("<div style='{cb}'><p style='position:absolute;right:10px;bottom:20px;width:50px;height:30px;margin:0'></p></div>"),
            800.0,
            "p",
        );
        assert!(close(p[0].x, 240.0) && close(p[0].y, 150.0), "{p:?}");
        let p = boxes(
            &format!("<div style='{cb}'><p style='position:absolute;inset:10px;margin:0'></p></div>"),
            800.0,
            "p",
        );
        assert!(close(p[0].w, 280.0) && close(p[0].h, 180.0), "{p:?}");
        let p = boxes(
            &format!("<div style='{cb}'><p style='position:absolute;left:0;right:0;width:100px;margin:0 auto'></p></div>"),
            800.0,
            "p",
        );
        assert!(close(p[0].x, 100.0), "auto margins center it: {p:?}");
    }

    #[test]
    fn static_position_when_insets_are_auto() {
        let p = boxes(
            "<div style='position:relative'>before<p style='position:absolute;margin:0'>x</p></div>",
            400.0,
            "p",
        );
        assert!(
            p[0].x > 30.0 && close(p[0].y, 0.0),
            "where it would have been: {p:?}"
        );
    }

    #[test]
    fn no_positioned_ancestor_uses_the_page() {
        let p = boxes(
            "<div style='margin:30px'><p style='position:absolute;top:0;left:0;margin:0'>x</p></div>",
            400.0,
            "p",
        );
        assert!(close(p[0].x, 0.0) && close(p[0].y, 0.0), "{p:?}");
        // Fixed ignores positioned ancestors.
        let p = boxes(
            "<div style='position:relative;margin:30px'><p style='position:fixed;top:0;left:0;margin:0'>x</p></div>",
            400.0,
            "p",
        );
        assert!(close(p[0].x, 0.0) && close(p[0].y, 0.0), "{p:?}");
    }

    #[test]
    fn nested_containing_blocks() {
        let page = "<div style='position:relative;left:100px;top:100px;width:200px;height:200px'>\
             <section style='position:absolute;top:10px;left:10px;width:100px;height:100px'>\
             <p style='position:absolute;bottom:0;right:0;width:10px;height:10px;margin:0'></p></section></div>";
        let p = boxes(page, 800.0, "p");
        // 100 (relative) + 10 (section) + 100 - 10.
        assert!(close(p[0].x, 200.0) && close(p[0].y, 200.0), "{p:?}");
    }

    #[test]
    fn paint_order_follows_z_index() {
        let page = "<div id=a style='position:relative;z-index:2'>a</div>\
             <div id=b style='position:relative;z-index:-1'>b</div>\
             <div id=c>c</div><div id=d style='float:left'>d</div><div id=e style='position:absolute'>e</div>";
        let dom = html::parse(page);
        let ua = css::parse(style::UA_CSS);
        let ctx = StyleContext {
            sheets: vec![(0, &ua)],
            viewport: (400.0, 600.0),
        };
        let styled = style::style_tree(&dom, &ctx);
        let root = layout_tree(&styled, 400.0);
        let ids: Vec<String> = paint_order(&root)
            .iter()
            .filter_map(|b| match b.kind {
                BoxKind::Block(n) => n.node.element()?.attrs.get("id").cloned(),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["b", "c", "d", "e", "a"]);
    }

    #[test]
    fn negative_z_index_parses() {
        let dom = html::parse("<div style='z-index:-3;position:relative'>x</div>");
        let ua = css::parse(style::UA_CSS);
        let ctx = StyleContext {
            sheets: vec![(0, &ua)],
            viewport: (400.0, 600.0),
        };
        let styled = style::style_tree(&dom, &ctx);
        fn find(n: &style::StyledNode) -> Option<i32> {
            if n.node.element().is_some_and(|e| e.tag == "div") {
                return n.style.z_index;
            }
            n.children.iter().find_map(find)
        }
        assert_eq!(find(&styled), Some(-3));
    }
}

#[cfg(test)]
mod flex_tests {
    use super::table_tests::{boxes, close};

    /// The `<i>` items of a flex `<div>` styled `style`, 300px wide.
    fn items(style: &str, items: &[&str]) -> Vec<crate::layout::Rect> {
        let html: String = items
            .iter()
            .map(|s| format!("<i style='display:block;{s}'>x</i>"))
            .collect();
        boxes(
            &format!("<div style='display:flex;width:300px;{style}'>{html}</div>"),
            800.0,
            "i",
        )
    }

    #[test]
    fn grow_fills_the_line() {
        let i = items("", &["width:50px", "flex:1", "flex-grow:2"]);
        assert!(close(i[0].w, 50.0), "{i:?}");
        // 250px left for the other two, shared 1:2 (each starts at 0px with `flex: 1`... and its content).
        assert!(close(i[0].w + i[1].w + i[2].w, 300.0), "{i:?}");
        assert!(i[2].w > i[1].w, "{i:?}");
        let i = items("", &["flex:1", "flex:1", "flex:1"]);
        assert!(close(i[0].w, 100.0) && close(i[2].x, 200.0), "{i:?}");
    }

    #[test]
    fn justify_content() {
        let w = "width:50px";
        let i = items("justify-content:space-between", &[w, w, w]);
        assert!(
            close(i[0].x, 0.0) && close(i[1].x, 125.0) && close(i[2].x, 250.0),
            "{i:?}"
        );
        let i = items("justify-content:center", &[w, w]);
        assert!(close(i[0].x, 100.0) && close(i[1].x, 150.0), "{i:?}");
        let i = items("justify-content:flex-end", &[w]);
        assert!(close(i[0].x, 250.0), "{i:?}");
        let i = items("justify-content:space-evenly", &[w, w]);
        assert!(close(i[0].x, 200.0 / 3.0), "{i:?}");
    }

    #[test]
    fn align_items_and_self() {
        let tall = "height:100px";
        let i = items("", &[tall, ""]);
        assert!(close(i[1].h, 100.0), "stretch by default: {i:?}");
        let i = items("align-items:center", &[tall, ""]);
        assert!(close(i[1].y + i[1].h / 2.0, 50.0), "{i:?}");
        let i = items("", &[tall, "align-self:flex-end"]);
        assert!(close(i[1].y + i[1].h, 100.0), "{i:?}");
        let i = items("height:200px;align-items:flex-start", &[""]);
        assert!(close(i[0].y, 0.0) && i[0].h < 50.0, "{i:?}");
    }

    #[test]
    fn wrapping() {
        let w = "width:100px;height:20px";
        let i = items("flex-wrap:wrap", &[w, w, w, w, w]);
        assert!(
            close(i[2].y, 0.0) && close(i[3].x, 0.0) && close(i[3].y, 20.0),
            "{i:?}"
        );
        let i = items("flex-wrap:wrap;gap:10px 5px", &[w, w, w]);
        assert!(
            close(i[1].x, 105.0) && close(i[2].x, 0.0) && close(i[2].y, 30.0),
            "{i:?}"
        );
        let d = boxes("<div style='display:flex;flex-wrap:wrap;width:300px'><i style='display:block;width:200px'>x</i><i style='display:block;width:200px'>y</i></div>", 800.0, "div");
        assert!(d[0].h > 30.0, "two lines tall: {d:?}");
    }

    #[test]
    fn shrinking_stops_at_the_content() {
        let long = "<i style='display:block'>unbreakablewordthatislong</i>";
        let i = boxes(
            &format!("<div style='display:flex;width:100px'>{long}<i style='display:block;width:200px'>b</i></div>"),
            800.0,
            "i",
        );
        assert!(i[0].w > 100.0, "the long word doesn't get squashed: {i:?}");
        let i = items("", &["width:200px;flex:none", "width:200px"]);
        assert!(close(i[0].w, 200.0) && close(i[1].w, 100.0), "{i:?}");
    }

    #[test]
    fn columns() {
        let i = items(
            "flex-direction:column;align-items:center",
            &["width:50px", "width:100px"],
        );
        assert!(
            close(i[0].x, 125.0) && close(i[1].x, 100.0) && i[1].y >= i[0].y + i[0].h - 0.5,
            "{i:?}"
        );
        let i = items("flex-direction:column;height:300px", &["flex:1", "height:50px"]);
        assert!(close(i[0].h, 250.0), "grows to fill the height: {i:?}");
        let i = items(
            "flex-direction:column;height:300px;justify-content:flex-end",
            &["height:50px"],
        );
        assert!(close(i[0].y, 250.0), "{i:?}");
        let i = items("flex-direction:column", &[""]);
        assert!(close(i[0].w, 300.0), "stretched across: {i:?}");
    }

    #[test]
    fn order_and_reverse() {
        let w = "width:50px";
        let i = items("flex-direction:row-reverse", &[w, w]);
        assert!(close(i[0].x, 250.0) && close(i[1].x, 200.0), "{i:?}");
        let i = items("", &["width:50px;order:2", w]);
        assert!(close(i[0].x, 50.0) && close(i[1].x, 0.0), "{i:?}");
    }

    #[test]
    fn inline_flex_sits_in_the_line() {
        let d = boxes(
            "before <span style='display:inline-flex;gap:4px'><b>a</b><b>b</b></span> after",
            800.0,
            "span",
        );
        assert!(d[0].x > 30.0 && d[0].w < 100.0, "{d:?}");
    }
}

#[cfg(test)]
mod calc_var_tests {
    use super::table_tests::{boxes, close};
    use crate::css;
    use crate::html;
    use crate::style::{self, StyleContext};

    fn div(style: &str) -> crate::layout::Rect {
        boxes(&format!("<div style='{style}'>x</div>"), 400.0, "div")[0]
    }

    #[test]
    fn calc_widths() {
        assert!(close(div("width:calc(100% - 50px)").w, 350.0));
        assert!(close(div("width:calc(50% + 2em)").w, 232.0));
        assert!(close(div("width:calc((100% - 20px) / 2)").w, 190.0));
        assert!(close(div("width:calc(3 * 10px)").w, 30.0));
        assert!(close(div("margin-left:calc(25%)").x, 100.0));
        // Invalid: mixing a length and a plain number.
        assert!(close(div("width:calc(100% + 3)").w, 400.0));
    }

    #[test]
    fn min_max_clamp() {
        assert!(close(div("width:min(100px, 300px)").w, 100.0));
        assert!(close(div("width:max(100px, 2em)").w, 100.0));
        assert!(close(div("width:clamp(50px, 10px, 200px)").w, 50.0));
        assert!(close(div("width:clamp(50px, 1000px, 200px)").w, 200.0));
    }

    #[test]
    fn custom_properties() {
        let page = "<div style='--w: 120px; --m: 10px 30px'>\
             <p style='width: var(--w); margin: var(--m)'>a</p>\
             <p style='width: var(--missing, 80px)'>b</p>\
             <p style='--w: calc(var(--w) + 30px); width: var(--w)'>c</p></div>";
        let p = boxes(page, 400.0, "p");
        assert!(
            close(p[0].w, 120.0) && close(p[0].x, 30.0),
            "inherited, and in a shorthand: {p:?}"
        );
        assert!(close(p[1].w, 80.0), "fallback: {p:?}");
        assert!(close(p[2].w, 150.0), "built from the inherited value: {p:?}");
    }

    /// The computed style of the first `<b>`, with `sheet` as the page's CSS.
    fn b_style(html_src: &str, sheet: &str) -> std::rc::Rc<style::ComputedStyle> {
        let dom = html::parse(html_src);
        let ua = css::parse(style::UA_CSS);
        let author = css::parse(sheet);
        let ctx = StyleContext {
            sheets: vec![(0, &ua), (1, &author)],
            viewport: (400.0, 600.0),
        };
        let styled = style::style_tree(&dom, &ctx);
        fn find(n: &style::StyledNode) -> Option<std::rc::Rc<style::ComputedStyle>> {
            if n.node.element().is_some_and(|e| e.tag == "b") {
                return Some(n.style.clone());
            }
            n.children.iter().find_map(find)
        }
        find(&styled).unwrap()
    }

    #[test]
    fn colors_from_variables_in_stylesheets() {
        let s = b_style("<b>x</b>", ":root { --fg: #123456 } b { color: var(--fg) }");
        assert_eq!((s.color.r, s.color.g, s.color.b), (0x12, 0x34, 0x56));
    }

    #[test]
    fn nested_variables_cant_blow_up() {
        // Each level doubles the last: 2^40 values if expanded fully.
        let mut css = String::from("--v0: aaaaaaaa;");
        for i in 1..=40 {
            css.push_str(&format!("--v{i}: var(--v{}) var(--v{});", i - 1, i - 1));
        }
        let start = std::time::Instant::now();
        let s = b_style(
            &format!("<div style='{css}'><b style='width: var(--v40)'>x</b></div>"),
            "",
        );
        assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
        assert!(s.vars.values().all(|v| v.len() <= 1024));
    }
}
