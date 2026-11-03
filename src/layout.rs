//! Layout: turns the styled tree into positioned boxes.
//!
//! Supports normal flow (block + inline formatting contexts), the CSS box
//! model, auto margins, max-width, sibling margin collapsing, text wrapping
//! and alignment, list markers, inline-blocks, a simple flexbox and tables
//! (the CSS 2.1 automatic table layout, in the separated-borders model).

use crate::css::Color;
use crate::dom::NodeType;
use crate::font;
use crate::style::{ComputedStyle, Display, Len, ListStyle, StyledNode, TextAlign, VAlign};
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
            Display::Inline | Display::InlineBlock | Display::InlineTable | Display::TableCell
                if node.style.display == Display::Flex && child.node.element().is_some() =>
            {
                flush_run(&mut b, &mut run);
                b.children.push(build_block(child, link));
            }
            // A cell outside a row is approximated as an inline-block.
            Display::Inline | Display::InlineBlock | Display::InlineTable | Display::TableCell => {
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
            if matches!(
                node.display(),
                Display::InlineBlock | Display::InlineTable | Display::TableCell
            ) || e.tag == "img"
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
                push(anon, InlineItem::Atomic(idx));
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

pub fn layout_tree<'a>(root: &'a StyledNode<'a>, viewport_w: f32) -> LayoutBox<'a> {
    let mut b = build_layout_tree(root);
    b.layout(0.0, viewport_w, 0.0);
    b
}

impl<'a> LayoutBox<'a> {
    fn layout(&mut self, cb_x: f32, cb_w: f32, y: f32) {
        match self.kind {
            BoxKind::Block(_) => self.layout_block(cb_x, cb_w, y, None),
            BoxKind::Anonymous { .. } => {
                self.dims.content = Rect {
                    x: cb_x,
                    y,
                    w: cb_w,
                    h: 0.0,
                };
                self.layout_inline();
            }
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

    /// `forced_w`: border-box width imposed by a flex/inline-block parent.
    fn layout_block(&mut self, cb_x: f32, cb_w: f32, y: f32, forced_w: Option<f32>) {
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

        if let Some(t) = table {
            self.layout_table(t);
        } else if s.display == Display::Flex && s.flex_row {
            self.layout_flex_row();
        } else {
            self.layout_block_children();
        }

        let s = self.style().clone();
        let d = &mut self.dims;
        if let Len::Px(h) = s.height {
            d.content.h = h;
        }
        if let Len::Px(h) = s.min_height {
            d.content.h = d.content.h.max(h);
        }
    }

    fn layout_block_children(&mut self) {
        let content = self.dims.content;
        let mut cursor = content.y;
        let mut prev_mb = 0.0f32;
        for child in &mut self.children {
            let mt = match child.kind {
                BoxKind::Block(n) => n.style.margin[0].or_zero(content.w),
                _ => 0.0,
            };
            // Collapse adjacent vertical margins (positive margins only).
            let collapse = prev_mb.min(mt).max(0.0);
            child.layout(content.x, content.w, cursor - collapse);
            cursor = child.dims.margin_box().y + child.dims.margin_box().h;
            prev_mb = if matches!(child.kind, BoxKind::Block(_)) {
                child.dims.margin.b
            } else {
                0.0
            };
        }
        self.dims.content.h = cursor - content.y;
    }

    fn layout_flex_row(&mut self) {
        let content = self.dims.content;
        let gap = self.style().gap;
        let n = self.children.len();
        if n == 0 {
            return;
        }
        let avail = content.w - gap * (n.saturating_sub(1)) as f32;
        let prefs: Vec<f32> = self
            .children
            .iter()
            .map(|c| c.max_content_width(content.w))
            .collect();
        let total: f32 = prefs.iter().sum();
        let scale = if total > avail && total > 0.0 {
            avail / total
        } else {
            1.0
        };
        let mut x = content.x;
        let mut h = 0.0f32;
        for (child, pref) in self.children.iter_mut().zip(prefs) {
            let w = (pref * scale).max(0.0);
            child.layout_at(x, content.y, w);
            let mb = child.dims.margin_box();
            x = mb.x + mb.w + gap;
            h = h.max(mb.h);
        }
        self.dims.content.h = h;
    }

    /// Lay out at a forced border-box width `w`, as a flex item or table cell.
    fn layout_at(&mut self, x: f32, y: f32, w: f32) {
        match self.kind {
            BoxKind::Block(_) => self.layout_block(x, w, y, Some(w)),
            BoxKind::Anonymous { .. } => {
                self.dims.content = Rect { x, y, w, h: 0.0 };
                self.layout_inline();
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
            if s.display != Display::TableCell {
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
                        InlineItem::Atomic(i) => {
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
                let row = s.display == Display::Flex && s.flex_row;
                let ws = self.children.iter().map(|c| c.intrinsic_widths(cb_w));
                if row {
                    let gaps = s.gap * self.children.len().saturating_sub(1) as f32;
                    let (a, b) = ws.fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
                    (a + gaps, b + gaps)
                } else {
                    ws.fold((0.0, 0.0), |(a, b), (x, y)| (f32::max(a, x), f32::max(b, y)))
                }
            }
        };
        let (mut min, mut max) = (min + hz, max + hz);
        if let (BoxKind::Block(_), Some(w)) = (&self.kind, s.width.resolve(cb_w)) {
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

    fn layout_inline(&mut self) {
        let content = self.dims.content;
        let avail = content.w;
        let (items, container_style) = match &self.kind {
            BoxKind::Anonymous { items, style, .. } => (items.clone(), style.clone()),
            _ => return,
        };

        // Lay out atomic inline boxes at the origin first so we know their size.
        for item in &items {
            if let InlineItem::Atomic(i) = item {
                let child = &mut self.children[*i];
                let pref = child.max_content_width(avail);
                let w = pref.min(avail);
                child.layout_block(0.0, w, 0.0, Some(w - margins_h(child, avail)));
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
            }
        }

        // Break into lines.
        enum Placed {
            Text(TextFrag),
            Atomic(usize, f32),
        }
        let mut lines: Vec<(Vec<Placed>, f32, Option<Rc<ComputedStyle>>)> = Vec::new();
        let mut cur: Vec<Placed> = Vec::new();
        let mut x = 0.0f32;
        let finish =
            |cur: &mut Vec<Placed>, x: &mut f32, lines: &mut Vec<_>, br: Option<Rc<ComputedStyle>>| {
                // Drop trailing spaces.
                while let Some(Placed::Text(t)) = cur.last() {
                    if t.text == " " {
                        *x -= t.w;
                        cur.pop();
                    } else {
                        break;
                    }
                }
                lines.push((std::mem::take(cur), *x, br));
                *x = 0.0;
            };
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
                        finish(&mut cur, &mut x, &mut lines, None);
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
                Piece::Break(s) => finish(&mut cur, &mut x, &mut lines, Some(s)),
                Piece::Atomic(i, w) => {
                    if x + w > avail && !cur.is_empty() {
                        finish(&mut cur, &mut x, &mut lines, None);
                    }
                    cur.push(Placed::Atomic(i, x));
                    x += w;
                }
            }
        }
        if !cur.is_empty() {
            finish(&mut cur, &mut x, &mut lines, None);
        }

        // Vertical alignment of each line, then horizontal alignment.
        let mut y = content.y;
        let mut frags = Vec::new();
        for (line, width, br) in lines {
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
                let s = br.as_deref().unwrap_or(&container_style);
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
                        t.x += content.x + shift;
                        t.baseline = baseline;
                        frags.push(t);
                    }
                    Placed::Atomic(i, local_x) => {
                        // Laid out at the origin earlier; move onto the line,
                        // sitting on the baseline.
                        let child = &mut self.children[i];
                        let mb = child.dims.margin_box();
                        let a = child.last_baseline().map(|b| b - mb.y).unwrap_or(mb.h);
                        child.translate(content.x + shift + local_x - mb.x, baseline - a - mb.y);
                    }
                }
            }
            y += asc + desc;
        }
        self.dims.content.h = y - content.y;
        if let BoxKind::Anonymous { frags: f, .. } = &mut self.kind {
            *f = frags;
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
        for c in 0..n {
            if let Some(p) = self.pct[c] {
                w[c] = (avail * p / 100.0).max(self.min[c]);
                rem -= w[c];
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
        self.children.iter().rev().find_map(|c| c.last_baseline())
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

fn margins_h(b: &LayoutBox, cb_w: f32) -> f32 {
    let s = b.style();
    s.margin[1].or_zero(cb_w) + s.margin[3].or_zero(cb_w)
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
    fn boxes(body: &str, width: f32, tag: &str) -> Vec<Rect> {
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

    fn close(a: f32, b: f32) -> bool {
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
