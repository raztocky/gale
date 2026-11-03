//! Layout: turns the styled tree into positioned boxes.
//!
//! Supports normal flow (block + inline formatting contexts), the CSS box
//! model, auto margins, max-width, sibling margin collapsing, text wrapping
//! and alignment, list markers, inline-blocks and a simple flexbox.

use crate::css::Color;
use crate::dom::NodeType;
use crate::font;
use crate::image::Decoded;
use crate::style::{ComputedStyle, Display, Len, ListStyle, StyledNode, TextAlign};
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
}

// ---------------------------------------------------------------------------
// Box tree construction
// ---------------------------------------------------------------------------

pub fn build_layout_tree<'a>(root: &'a StyledNode<'a>) -> LayoutBox<'a> {
    build_block(root, None)
}

fn build_block<'a>(node: &'a StyledNode<'a>, link: Option<String>) -> LayoutBox<'a> {
    let mut b = LayoutBox::new(BoxKind::Block(node));
    b.link = link.clone();
    let mut run: Option<LayoutBox<'a>> = None;
    let mut li_index = 0;
    for child in &node.children {
        let link = child_link(child, &link);
        match child.display() {
            Display::None => {}
            Display::Block | Display::ListItem | Display::Flex => {
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
            Display::Inline | Display::InlineBlock
                if node.style.display == Display::Flex && child.node.element().is_some() =>
            {
                flush_run(&mut b, &mut run);
                b.children.push(build_block(child, link));
            }
            Display::Inline | Display::InlineBlock => {
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
            if node.display() == Display::InlineBlock || e.tag == "img" {
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
                    Display::Block | Display::ListItem | Display::Flex => {
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

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

pub fn layout_tree<'a>(root: &'a StyledNode<'a>, viewport_w: f32) -> LayoutBox<'a> {
    layout_tree_with_images(root, viewport_w, &HashMap::new())
}

/// Lay out with decoded images for `<img>` elements, by node.
pub fn layout_tree_with_images<'a>(
    root: &'a StyledNode<'a>,
    viewport_w: f32,
    images: &HashMap<u32, Decoded>,
) -> LayoutBox<'a> {
    let mut b = build_layout_tree(root);
    if !images.is_empty() {
        attach_images(&mut b, images);
    }
    b.layout(0.0, viewport_w, 0.0);
    b
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
        let d = &mut self.dims;
        let hz = d.padding.l + d.padding.r + d.border.l + d.border.r;

        let mut width = s.width.resolve(cb_w);
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
            match child.kind {
                BoxKind::Block(_) => child.layout_block(x, w, content.y, Some(w)),
                _ => {
                    child.dims.content = Rect {
                        x,
                        y: content.y,
                        w,
                        h: 0.0,
                    };
                    child.layout_inline();
                }
            }
            let mb = child.dims.margin_box();
            x = mb.x + mb.w + gap;
            h = h.max(mb.h);
        }
        self.dims.content.h = h;
    }

    /// Shrink-to-fit preferred width (border box, including margins).
    fn max_content_width(&self, cb_w: f32) -> f32 {
        let s = self.style();
        let edges = |l: &[Len; 4]| l[1].or_zero(cb_w) + l[3].or_zero(cb_w);
        let hz = match self.kind {
            BoxKind::Block(_) => edges(&s.padding) + edges(&s.margin) + s.border_width[1] + s.border_width[3],
            _ => 0.0,
        };
        if let Some(i) = &self.image {
            let (w, _) = replaced_size(s, i.natural, cb_w, None);
            return w + hz;
        }
        if let (BoxKind::Block(_), Some(w)) = (&self.kind, s.width.resolve(cb_w)) {
            return w + hz;
        }
        let inner = match &self.kind {
            BoxKind::Anonymous { items, .. } => {
                let mut best = 0.0f32;
                let mut line = 0.0f32;
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
                            for (i, part) in txt.split('\n').enumerate() {
                                if i > 0 {
                                    best = best.max(line);
                                    line = 0.0;
                                }
                                line += font::measure(f, style.font_size, part);
                                if !style.pre && text.ends_with(|c: char| c.is_ascii_whitespace()) {
                                    line += font::measure(f, style.font_size, " ");
                                }
                            }
                        }
                        InlineItem::Break(_) => {
                            best = best.max(line);
                            line = 0.0;
                        }
                        InlineItem::Atomic(i) => line += self.children[*i].max_content_width(cb_w),
                    }
                }
                best.max(line)
            }
            BoxKind::Block(_) => {
                let row = s.display == Display::Flex && s.flex_row;
                let ws = self.children.iter().map(|c| c.max_content_width(cb_w));
                if row {
                    ws.sum::<f32>() + s.gap * self.children.len().saturating_sub(1) as f32
                } else {
                    ws.fold(0.0, f32::max)
                }
            }
        };
        let w = inner + hz;
        match s.max_width.resolve(cb_w) {
            Some(m) => w.min(m + hz),
            None => w,
        }
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
