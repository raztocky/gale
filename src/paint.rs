//! Painting: layout tree -> display list -> pixels.

use crate::css::Color;
use crate::font;
use crate::image::{self, Bitmap};
use crate::layout::{BoxKind, LayoutBox, Rect};
use crate::style::{BgSize, Len, Position};
use std::collections::HashMap;
use std::sync::Arc;

/// What layout produces: rectangles and text runs. Text is turned into glyph
/// bitmaps by [`rasterize`] inside the (sandboxed) renderer, so the browser
/// process never runs the font rasterizer on anything a page controls.
#[derive(Debug, Clone, PartialEq)]
pub enum DrawOp {
    Rect {
        rect: Rect,
        color: Color,
    },
    Text {
        x: f32,
        baseline: f32,
        text: String,
        font: usize,
        size: f32,
        color: Color,
        italic: bool,
        /// Glyphs whose centre is outside this aren't drawn (`overflow`).
        clip: Option<Rect>,
    },
    /// An image (as decoded) drawn into `rect`, cropped to `clip`.
    Image {
        rect: Rect,
        image: Arc<Bitmap>,
        clip: Option<Rect>,
    },
}

/// What the browser paints: rectangles, pre-rasterized glyphs and images.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCommand {
    Rect {
        rect: Rect,
        color: Color,
    },
    /// Glyphs from the atlas, each placed by its top-left pixel.
    Glyphs {
        color: Color,
        glyphs: Vec<PlacedGlyph>,
    },
    /// RGBA pixels stretched over `rect` (normally already its size).
    Image {
        rect: Rect,
        image: Arc<Bitmap>,
    },
}

/// Image pixels sent with one frame, in bytes. Past this, images are sent
/// small and the browser enlarges them.
pub const MAX_FRAME_IMAGE_BYTES: usize = 40 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    pub x: f32,
    pub y: f32,
    pub id: u32,
}

/// An 8-bit coverage bitmap for one glyph at one size.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphBitmap {
    pub w: u16,
    pub h: u16,
    pub data: Vec<u8>,
}

pub type DisplayList = Vec<DisplayCommand>;

/// Largest glyph bitmap side (a 256px glyph plus italic skew fits easily).
pub const MAX_GLYPH_SIDE: u16 = 1024;
/// Stop adding glyphs to a page's atlas beyond this many bytes.
pub const MAX_ATLAS_BYTES: usize = 24 * 1024 * 1024;

/// (font, character, size in quarter pixels, italic)
type AtlasKey = (usize, char, u32, bool);

/// Turn text runs into glyph placements plus a bitmap atlas.
fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

pub fn rasterize(ops: Vec<DrawOp>) -> (DisplayList, Vec<GlyphBitmap>) {
    let (list, atlas, _) = rasterize_indexed(ops);
    (list, atlas)
}

/// `rasterize`, also returning where each op's commands start in the list
/// (and, last, the list's length).
pub fn rasterize_indexed(ops: Vec<DrawOp>) -> (DisplayList, Vec<GlyphBitmap>, Vec<usize>) {
    let mut starts = Vec::with_capacity(ops.len() + 1);
    let mut atlas: Vec<GlyphBitmap> = Vec::new();
    let mut atlas_bytes = 0usize;
    let mut ids: HashMap<AtlasKey, Option<(u32, fontdue::Metrics)>> = HashMap::new();
    let mut list = Vec::with_capacity(ops.len());
    // Images scaled to the size they're drawn at, so the browser gets no
    // more pixels than it shows. The same image at the same size is sent once.
    let mut scaled: HashMap<(usize, u32, u32), Arc<Bitmap>> = HashMap::new();
    let mut image_bytes = 0usize;
    for op in ops {
        starts.push(list.len());
        match op {
            DrawOp::Rect { rect, color } => list.push(DisplayCommand::Rect { rect, color }),
            DrawOp::Image { rect, image, clip } => {
                if !(rect.w >= 1.0 && rect.h >= 1.0) || clip.is_some_and(|c| rect.intersect(c).is_empty()) {
                    continue;
                }
                // Only ever shrunk here: enlarging is left to the browser's
                // painter, so no bitmap is bigger than the decoded image.
                let (w, h) = (
                    (rect.w.round() as u32).clamp(1, image.w),
                    (rect.h.round() as u32).clamp(1, image.h),
                );
                let key = (Arc::as_ptr(&image) as usize, w, h);
                let bmp = match scaled.get(&key) {
                    Some(b) => b.clone(),
                    None => {
                        let (w, h) = if image_bytes + w as usize * h as usize * 4 > MAX_FRAME_IMAGE_BYTES {
                            image::fit(w, h, 64)
                        } else {
                            (w, h)
                        };
                        if image_bytes + w as usize * h as usize * 4 > MAX_FRAME_IMAGE_BYTES {
                            continue;
                        }
                        let b = if (w, h) == (image.w, image.h) {
                            image.clone()
                        } else {
                            Arc::new(image::resize(&image, w, h))
                        };
                        image_bytes += b.rgba.len();
                        scaled.insert(key, b.clone());
                        b
                    }
                };
                match clip {
                    Some(c) if !contains(c, rect) => {
                        // Crop to the pixels inside the clip, and draw just those.
                        let v = rect.intersect(c);
                        let (sx, sy) = (bmp.w as f32 / rect.w, bmp.h as f32 / rect.h);
                        let x0 = ((v.x - rect.x) * sx).floor().max(0.0) as u32;
                        let y0 = ((v.y - rect.y) * sy).floor().max(0.0) as u32;
                        let x1 = ((v.x + v.w - rect.x) * sx).ceil() as u32;
                        let y1 = ((v.y + v.h - rect.y) * sy).ceil() as u32;
                        let part = image::crop(&bmp, x0, y0, x1, y1);
                        if part.w == 0 || part.h == 0 {
                            continue;
                        }
                        let dest = Rect {
                            x: rect.x + x0 as f32 / sx,
                            y: rect.y + y0 as f32 / sy,
                            w: part.w as f32 / sx,
                            h: part.h as f32 / sy,
                        };
                        list.push(DisplayCommand::Image {
                            rect: dest,
                            image: Arc::new(part),
                        });
                    }
                    _ => list.push(DisplayCommand::Image { rect, image: bmp }),
                }
            }
            DrawOp::Text {
                x,
                baseline,
                text,
                font,
                size,
                color,
                italic,
                clip,
            } => {
                if !(MIN_TEXT_SIZE..=crate::style::MAX_FONT_SIZE).contains(&size) || font >= font::COUNT {
                    continue;
                }
                let mut pen = x;
                let mut glyphs = Vec::new();
                for ch in text.chars() {
                    // Per character: the style's font, or a fallback that has it.
                    let fi = font::resolve(font, ch);
                    let f = font::font(fi);
                    let key = (fi, ch, (size * 4.0) as u32, italic);
                    let entry = ids.entry(key).or_insert_with(|| {
                        if atlas_bytes > MAX_ATLAS_BYTES {
                            return None;
                        }
                        let (m, bitmap) = f.rasterize(ch, size);
                        let bmp = if italic && font::fake_italic(fi) {
                            skew(&m, &bitmap)
                        } else {
                            GlyphBitmap {
                                w: m.width as u16,
                                h: m.height as u16,
                                data: bitmap,
                            }
                        };
                        if bmp.w == 0 || bmp.h == 0 || bmp.w > MAX_GLYPH_SIDE || bmp.h > MAX_GLYPH_SIDE {
                            return Some((u32::MAX, m));
                        }
                        atlas_bytes += bmp.data.len();
                        atlas.push(bmp);
                        Some(((atlas.len() - 1) as u32, m))
                    });
                    let Some((id, m)) = *entry else { continue };
                    if id != u32::MAX {
                        let gx = (pen + m.xmin as f32).round();
                        let gy = (baseline - (m.height as i32 + m.ymin) as f32).round();
                        let (cx, cy) = (gx + m.width as f32 / 2.0, gy + m.height as f32 / 2.0);
                        if clip.is_none_or(|c| c.contains(cx, cy)) {
                            glyphs.push(PlacedGlyph { x: gx, y: gy, id });
                        }
                    }
                    pen += m.advance_width;
                }
                if !glyphs.is_empty() {
                    list.push(DisplayCommand::Glyphs { color, glyphs });
                }
            }
        }
    }
    starts.push(list.len());
    (list, atlas, starts)
}

/// Synthetic italics: shear the bitmap to the right, more at the top.
fn skew(m: &fontdue::Metrics, bitmap: &[u8]) -> GlyphBitmap {
    let extra = (m.height as f32 * 0.2).ceil() as usize;
    let w = m.width + extra;
    let mut data = vec![0u8; w * m.height];
    for row in 0..m.height {
        let shift = ((m.height - row) as f32 * 0.2) as usize;
        for col in 0..m.width {
            data[row * w + col + shift] = bitmap[row * m.width + col];
        }
    }
    GlyphBitmap {
        w: w as u16,
        h: m.height as u16,
        data,
    }
}

pub fn build_display_list(root: &LayoutBox) -> Vec<DrawOp> {
    build_layered(root, &|_| None).0
}

/// Looks up a decoded background image by its `url()` as written.
pub type Backgrounds<'b> = dyn Fn(&str) -> Option<crate::image::Decoded> + 'b;

/// Most tiles of a repeating background along each side; a smaller image is
/// stretched across instead.
const MAX_TILES_PER_SIDE: f32 = 32.0;

/// Most bitmap bytes a page's gradients may use; past that they're drawn
/// as one colour.
const MAX_GRADIENT_BYTES: usize = 24 * 1024 * 1024;

/// A box's background gradient, filling its padding box.
fn background_gradient(
    list: &mut Vec<DrawOp>,
    b: &LayoutBox,
    g: &crate::gradient::Gradient,
    used: &mut usize,
) {
    let area = b.dims.padding_box();
    if area.is_empty() {
        return;
    }
    let side = |v: f32| (v.ceil().max(1.0) as usize).min(crate::gradient::MAX_SIDE as usize);
    let bytes = side(area.w) * side(area.h) * 4;
    if *used + bytes > MAX_GRADIENT_BYTES {
        list.push(DrawOp::Rect {
            rect: area,
            color: crate::gradient::average(g),
        });
        return;
    }
    if let Some(bmp) = crate::gradient::render(g, area.w, area.h) {
        *used += bmp.rgba.len();
        list.push(DrawOp::Image {
            rect: area,
            image: Arc::new(bmp),
            clip: None,
        });
    }
}

/// A box's background image: sized, positioned and tiled in its padding
/// box, and clipped to it.
fn background_image(list: &mut Vec<DrawOp>, b: &LayoutBox, img: &crate::image::Decoded) {
    let s = b.style();
    let area = b.dims.padding_box();
    let (nw, nh) = (img.natural.0 as f32, img.natural.1 as f32);
    if nw < 1.0 || nh < 1.0 || area.is_empty() {
        return;
    }
    let (w, h) = match s.background_size {
        BgSize::Auto => (nw, nh),
        BgSize::Cover | BgSize::Contain => {
            let (kx, ky) = (area.w / nw, area.h / nh);
            let k = if s.background_size == BgSize::Cover {
                kx.max(ky)
            } else {
                kx.min(ky)
            };
            (nw * k, nh * k)
        }
        BgSize::Len(lw, lh) => match (lw.resolve(area.w), lh.resolve(area.h)) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * nh / nw),
            (None, Some(h)) => (h * nw / nh, h),
            (None, None) => (nw, nh),
        },
    };
    if !(w >= 0.5 && h >= 0.5 && w.is_finite() && h.is_finite()) {
        return;
    }
    // Percentages line up that point of the image with that point of the box.
    let at = |l: Len, free: f32| match l {
        Len::Percent(p) => free * p / 100.0,
        Len::Calc(px, p) => px + free * p / 100.0,
        l => l.or_zero(free),
    };
    let x = area.x + at(s.background_position[0], area.w - w);
    let y = area.y + at(s.background_position[1], area.h - h);
    let tiles = |repeat: bool, start: f32, size: f32, lo: f32, len: f32| -> Vec<(f32, f32)> {
        if !repeat {
            return vec![(start, size)];
        }
        let first = start - ((start - lo) / size).ceil() * size;
        let n = ((lo + len - first) / size).ceil();
        if n > MAX_TILES_PER_SIDE {
            vec![(lo, len)]
        } else {
            (0..n as usize).map(|i| (first + i as f32 * size, size)).collect()
        }
    };
    let (rx, ry) = s.background_repeat;
    let xs = tiles(rx, x, w, area.x, area.w);
    for (ty, th) in tiles(ry, y, h, area.y, area.h) {
        for &(tx, tw) in &xs {
            list.push(DrawOp::Image {
                rect: Rect {
                    x: tx,
                    y: ty,
                    w: tw,
                    h: th,
                },
                image: img.bitmap.clone(),
                clip: Some(area),
            });
        }
    }
}

/// How a layer moves as the page scrolls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayerKind {
    /// `position: fixed`: stays where it is in the window.
    Fixed,
    /// `position: sticky`: scrolls with the page until its top would go
    /// above `top`, then stays there, but moves at most `max_dy` (to the end
    /// of its containing block). `y0` is where it was laid out.
    Sticky { top: f32, y0: f32, max_dy: f32 },
}

/// A fixed or sticky box with everything in it: the display commands
/// `start..end`, which the browser paints on top of the page at a place
/// that depends on scrolling. `bounds` is the box's border box as laid out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    pub kind: LayerKind,
    pub bounds: Rect,
    pub start: usize,
    pub end: usize,
}

impl Layer {
    /// How far down from where it was laid out the layer is drawn, at
    /// `scroll`. Fixed layers are laid out in window coordinates.
    pub fn shift(&self, scroll: f32) -> f32 {
        match self.kind {
            LayerKind::Fixed => scroll,
            LayerKind::Sticky { top, y0, max_dy } => (scroll + top - y0).clamp(0.0, max_dy.max(0.0)),
        }
    }
}

/// Fixed and sticky boxes, by address, as layer roots: every box inside one
/// maps to the index of its outermost layer.
fn layer_map(root: &LayoutBox) -> (HashMap<usize, usize>, Vec<(LayerKind, Rect)>) {
    fn walk(
        b: &LayoutBox,
        layer: Option<usize>,
        map: &mut HashMap<usize, usize>,
        kinds: &mut Vec<(LayerKind, Rect)>,
    ) {
        for c in &b.children {
            let mut id = layer;
            if id.is_none() && kinds.len() < MAX_LAYERS {
                if let BoxKind::Block(n) = c.kind {
                    let s = &n.style;
                    let bb = c.dims.border_box();
                    let kind = match (s.position, s.inset[0].resolve(0.0)) {
                        (Position::Fixed, _) => Some(LayerKind::Fixed),
                        (Position::Sticky, Some(top)) => {
                            let cb = b.dims.content;
                            Some(LayerKind::Sticky {
                                top,
                                y0: bb.y,
                                max_dy: (cb.y + cb.h) - (bb.y + bb.h),
                            })
                        }
                        _ => None,
                    };
                    if let Some(k) = kind {
                        kinds.push((k, bb));
                        id = Some(kinds.len() - 1);
                    }
                }
            }
            if let Some(i) = id {
                map.insert(crate::layout::box_addr(c), i);
            }
            walk(c, id, map, kinds);
        }
    }
    let (mut map, mut kinds) = (HashMap::new(), Vec::new());
    walk(root, None, &mut map, &mut kinds);
    (map, kinds)
}

/// Most fixed and sticky layers per page.
pub const MAX_LAYERS: usize = 64;

/// The display list, and each fixed or sticky layer in it as a range of
/// ops (a layer's boxes are painted one after another).
/// A layer as a range of draw ops, before rasterizing.
pub type LayerOps = (LayerKind, Rect, std::ops::Range<usize>);

pub fn build_layered(root: &LayoutBox, backgrounds: &Backgrounds) -> (Vec<DrawOp>, Vec<LayerOps>) {
    let clips = crate::layout::clip_map(root);
    let (in_layer, kinds) = layer_map(root);
    let mut list = Vec::new();
    let mut layers: Vec<LayerOps> = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    let mut gradient_bytes = 0;
    for b in crate::layout::paint_order(root) {
        let id = in_layer.get(&crate::layout::box_addr(b)).copied();
        if open.map(|(i, _)| i) != id {
            if let Some((i, start)) = open.take() {
                layers.push((kinds[i].0, kinds[i].1, start..list.len()));
            }
            open = id.map(|i| (i, list.len()));
        }
        let start = list.len();
        render_box(&mut list, b, backgrounds, &mut gradient_bytes);
        if let Some(&c) = clips.get(&crate::layout::box_addr(b)) {
            clip_ops(&mut list, start, c);
        }
    }
    if let Some((i, start)) = open {
        layers.push((kinds[i].0, kinds[i].1, start..list.len()));
    }
    (list, layers)
}

/// Clip the ops from `start` on to `c`: rectangles exactly, text and images
/// when they're rasterized.
fn clip_ops(list: &mut Vec<DrawOp>, start: usize, c: Rect) {
    for op in &mut list[start..] {
        match op {
            DrawOp::Rect { rect, .. } => *rect = rect.intersect(c),
            DrawOp::Text { clip, .. } | DrawOp::Image { clip, .. } => {
                *clip = Some(clip.map_or(c, |x| x.intersect(c)))
            }
        }
    }
    let mut i = start;
    list.retain(|op| {
        i += 1;
        i <= start || !matches!(op, DrawOp::Rect { rect, .. } if rect.is_empty())
    });
}

/// Paint one box (not its children).
fn render_box(list: &mut Vec<DrawOp>, b: &LayoutBox, backgrounds: &Backgrounds, gradient_bytes: &mut usize) {
    let s = b.style();
    if let BoxKind::Block(_) = b.kind {
        let bb = b.dims.border_box();
        if s.background.a > 0 {
            list.push(DrawOp::Rect {
                rect: bb,
                color: s.background,
            });
        }
        // The image goes over the colour and under the borders.
        if let Some(img) = s.background_image.as_deref().and_then(backgrounds) {
            background_image(list, b, &img);
        } else if let Some(g) = &s.background_gradient {
            background_gradient(list, b, g, gradient_bytes);
        }
        let bw = s.border_width;
        let bc = s.border_color;
        let sides = [
            Rect {
                x: bb.x,
                y: bb.y,
                w: bb.w,
                h: bw[0],
            },
            Rect {
                x: bb.x + bb.w - bw[1],
                y: bb.y,
                w: bw[1],
                h: bb.h,
            },
            Rect {
                x: bb.x,
                y: bb.y + bb.h - bw[2],
                w: bb.w,
                h: bw[2],
            },
            Rect {
                x: bb.x,
                y: bb.y,
                w: bw[3],
                h: bb.h,
            },
        ];
        for (i, r) in sides.into_iter().enumerate() {
            if bw[i] > 0.0 && bc[i].a > 0 {
                list.push(DrawOp::Rect {
                    rect: r,
                    color: bc[i],
                });
            }
        }
        if let Some(m) = &b.marker {
            let f = font::index(s);
            let w = font::measure(f, s.font_size, m);
            let (a, d) = font::vmetrics(f, s.font_size);
            let baseline = b.dims.content.y + (s.line_height - (a + d)) / 2.0 + a;
            list.push(DrawOp::Text {
                x: b.dims.content.x - w - s.font_size * 0.5,
                baseline,
                text: m.clone(),
                font: f,
                size: s.font_size,
                color: s.color,
                italic: false,
                clip: None,
            });
        }
    }
    if let Some(img) = &b.image {
        list.push(DrawOp::Image {
            rect: b.dims.content,
            image: img.bitmap.clone(),
            clip: None,
        });
    }
    if let BoxKind::Anonymous { frags, .. } = &b.kind {
        for t in frags {
            if !t.style.visible {
                continue;
            }
            let f = font::index(&t.style);
            let size = t.style.font_size;
            let (a, d) = font::vmetrics(f, size);
            if t.bg.a > 0 {
                list.push(DrawOp::Rect {
                    rect: Rect {
                        x: t.x,
                        y: t.baseline - a,
                        w: t.w,
                        h: a + d,
                    },
                    color: t.bg,
                });
            }
            if t.text != " " && !t.text.is_empty() {
                list.push(DrawOp::Text {
                    x: t.x,
                    baseline: t.baseline,
                    text: t.text.clone(),
                    font: f,
                    size,
                    color: t.style.color,
                    italic: t.style.italic,
                    clip: None,
                });
            }
            let thick = (size / 14.0).max(1.0);
            if t.style.underline {
                list.push(DrawOp::Rect {
                    rect: Rect {
                        x: t.x,
                        y: t.baseline + size * 0.1,
                        w: t.w,
                        h: thick,
                    },
                    color: t.style.color,
                });
            }
            if t.style.strike {
                list.push(DrawOp::Rect {
                    rect: Rect {
                        x: t.x,
                        y: t.baseline - a * 0.35,
                        w: t.w,
                        h: thick,
                    },
                    color: t.style.color,
                });
            }
        }
    }
}

/// An 0RGB pixel buffer (the format minifb wants).
pub struct Canvas {
    pub pixels: Vec<u32>,
    pub width: usize,
    pub height: usize,
}

type GlyphKey = (usize, char, u32);

/// Rasterized glyphs. Bounded so a page can't grow it without limit.
#[derive(Default)]
pub struct GlyphCache {
    map: HashMap<GlyphKey, (fontdue::Metrics, Vec<u8>)>,
    bytes: usize,
}

/// Smallest text size the painter will rasterize, in px.
pub const MIN_TEXT_SIZE: f32 = 1.0;

const GLYPH_CACHE_BYTES: usize = 32 * 1024 * 1024;

impl Canvas {
    pub fn new(width: usize, height: usize, bg: Color) -> Canvas {
        Canvas {
            pixels: vec![pack(bg); width * height],
            width,
            height,
        }
    }

    /// Paint a display list. Glyph ids must index `atlas` (the IPC decoder
    /// checks this); out-of-range ids are skipped anyway.
    pub fn paint(&mut self, list: &DisplayList, atlas: &[GlyphBitmap], scroll_y: f32, offset_y: f32) {
        for cmd in list {
            self.paint_cmd(cmd, atlas, offset_y - scroll_y, offset_y);
        }
    }

    /// Paint one command shifted down by `dy`, clipped above `clip_top`.
    pub fn paint_cmd(&mut self, cmd: &DisplayCommand, atlas: &[GlyphBitmap], dy: f32, clip_top: f32) {
        match cmd {
            DisplayCommand::Rect { rect, color } => self.fill(
                Rect {
                    y: rect.y + dy,
                    ..*rect
                },
                *color,
                clip_top,
            ),
            DisplayCommand::Glyphs { color, glyphs } => {
                for g in glyphs {
                    let Some(bmp) = atlas.get(g.id as usize) else {
                        continue;
                    };
                    self.blit(g.x, g.y + dy, bmp, *color, clip_top);
                }
            }
            DisplayCommand::Image { rect, image } => self.draw_image(
                Rect {
                    y: rect.y + dy,
                    ..*rect
                },
                image,
                clip_top,
            ),
        }
    }

    /// Draw RGBA pixels stretched over `r` (nearest pixel), alpha-blended
    /// and clipped to the canvas.
    pub fn draw_image(&mut self, r: Rect, img: &Bitmap, clip_top: f32) {
        let (iw, ih) = (img.w as usize, img.h as usize);
        if iw == 0 || ih == 0 || img.rgba.len() < iw * ih * 4 || !(r.w > 0.0 && r.h > 0.0) {
            return;
        }
        let (x0, y0) = (r.x.round(), r.y.round());
        let (w, h) = (r.w.round().max(1.0), r.h.round().max(1.0));
        let ys = (y0.max(clip_top).max(0.0) as usize).min(self.height);
        let ye = ((y0 + h).max(0.0) as usize).min(self.height);
        let xs = (x0.max(0.0) as usize).min(self.width);
        let xe = ((x0 + w).max(0.0) as usize).min(self.width);
        for y in ys..ye {
            let sy = (((y as f32 - y0) * ih as f32 / h) as usize).min(ih - 1);
            for x in xs..xe {
                let sx = (((x as f32 - x0) * iw as f32 / w) as usize).min(iw - 1);
                let p = &img.rgba[(sy * iw + sx) * 4..][..4];
                if p[3] != 0 {
                    let c = Color {
                        r: p[0],
                        g: p[1],
                        b: p[2],
                        a: 255,
                    };
                    self.blend(x, y, c, p[3]);
                }
            }
        }
    }

    /// Alpha-blend a coverage bitmap at (x, y), clipped to the canvas.
    pub fn blit(&mut self, x: f32, y: f32, bmp: &GlyphBitmap, color: Color, clip_top: f32) {
        let (w, h) = (bmp.w as i64, bmp.h as i64);
        let (x0, y0) = (x as i64, y as i64);
        if y0 + h < clip_top as i64 || y0 >= self.height as i64 || x0 + w < 0 || x0 >= self.width as i64 {
            return;
        }
        if bmp.data.len() < (w * h) as usize {
            return;
        }
        for row in 0..h {
            let py = y0 + row;
            if py < clip_top as i64 || py < 0 || py >= self.height as i64 {
                continue;
            }
            for col in 0..w {
                let px = x0 + col;
                if px < 0 || px >= self.width as i64 {
                    continue;
                }
                let cov = bmp.data[(row * w + col) as usize];
                if cov == 0 {
                    continue;
                }
                let a = (cov as u32 * color.a as u32 / 255) as u8;
                self.blend(px as usize, py as usize, color, a);
            }
        }
    }

    pub fn fill(&mut self, r: Rect, color: Color, clip_top: f32) {
        let x0 = r.x.round().max(0.0) as usize;
        let y0 = r.y.round().max(clip_top).max(0.0) as usize;
        let x1 = ((r.x + r.w).round().max(0.0) as usize).min(self.width);
        let y1 = ((r.y + r.h).round().max(0.0) as usize).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend(x, y, color, color.a);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        x: f32,
        baseline: f32,
        text: &str,
        font_idx: usize,
        size: f32,
        color: Color,
        italic: bool,
        cache: &mut GlyphCache,
        clip_top: f32,
    ) {
        // fontdue corrupts memory when rasterizing at tiny (subnormal) sizes
        // (found by fuzzing). Text under MIN_TEXT_SIZE is invisible anyway.
        if !(MIN_TEXT_SIZE..=crate::style::MAX_FONT_SIZE).contains(&size) || font_idx >= font::COUNT {
            return;
        }
        let mut pen = x;
        for ch in text.chars() {
            let fi = font::resolve(font_idx, ch);
            let f = font::font(fi);
            let key = (fi, ch, (size * 4.0) as u32);
            if !cache.map.contains_key(&key) {
                if cache.bytes > GLYPH_CACHE_BYTES {
                    cache.map.clear();
                    cache.bytes = 0;
                }
                let g = f.rasterize(ch, size);
                cache.bytes += g.1.len() + 64;
                cache.map.insert(key, g);
            }
            let (m, bitmap) = &cache.map[&key];
            let gx = (pen + m.xmin as f32).round() as i64;
            let gy = (baseline - (m.height as i32 + m.ymin) as f32).round() as i64;
            for row in 0..m.height {
                let skew = if italic && font::fake_italic(fi) {
                    ((m.height - row) as f32 * 0.2) as i64
                } else {
                    0
                };
                let y = gy + row as i64;
                if y < clip_top as i64 || y < 0 || y >= self.height as i64 {
                    continue;
                }
                for col in 0..m.width {
                    let cov = bitmap[row * m.width + col];
                    if cov == 0 {
                        continue;
                    }
                    let px = gx + col as i64 + skew;
                    if px < 0 || px >= self.width as i64 {
                        continue;
                    }
                    let a = (cov as u32 * color.a as u32 / 255) as u8;
                    self.blend(px as usize, y as usize, color, a);
                }
            }
            pen += m.advance_width;
        }
    }

    fn blend(&mut self, x: usize, y: usize, c: Color, a: u8) {
        let i = y * self.width + x;
        if a == 255 {
            self.pixels[i] = pack(c);
            return;
        }
        let d = self.pixels[i];
        let mix = |src: u8, dst: u32| -> u32 { (src as u32 * a as u32 + dst * (255 - a as u32)) / 255 };
        let r = mix(c.r, (d >> 16) & 0xff);
        let g = mix(c.g, (d >> 8) & 0xff);
        let b = mix(c.b, d & 0xff);
        self.pixels[i] = (r << 16) | (g << 8) | b;
    }

    pub fn write_png(&self, path: &str) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut enc = png::Encoder::new(
            std::io::BufWriter::new(file),
            self.width as u32,
            self.height as u32,
        );
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(std::io::Error::other)?;
        let data: Vec<u8> = self
            .pixels
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8])
            .collect();
        w.write_image_data(&data).map_err(std::io::Error::other)?;
        Ok(())
    }
}

/// Height of a page tile, in px.
pub const TILE_H: usize = 512;
/// Memory the tile cache may use before evicting tiles far from view.
const TILE_CACHE_BYTES: usize = 96 * 1024 * 1024;

/// A page painted into horizontal tiles of finished pixels. Each tile is
/// painted once, the first time it scrolls into view; after that, scrolling
/// only copies pixels, so a page's display list (however heavy) isn't
/// replayed on every frame.
pub struct TileCache {
    width: usize,
    bg: Color,
    /// Vertical extent (top, bottom) of each display command, so a tile only
    /// paints the commands that overlap it.
    extents: Vec<(f32, f32)>,
    tiles: HashMap<usize, Vec<u32>>,
    count: usize,
    max_tiles: usize,
}

impl TileCache {
    pub fn new(
        list: &DisplayList,
        atlas: &[GlyphBitmap],
        width: usize,
        page_height: f32,
        bg: Color,
    ) -> TileCache {
        let extents = list
            .iter()
            .map(|cmd| match cmd {
                DisplayCommand::Rect { rect, .. } | DisplayCommand::Image { rect, .. } => {
                    (rect.y, rect.y + rect.h)
                }
                DisplayCommand::Glyphs { glyphs, .. } => {
                    glyphs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), g| {
                        let h = atlas.get(g.id as usize).map(|b| b.h as f32).unwrap_or(0.0);
                        (lo.min(g.y), hi.max(g.y + h))
                    })
                }
            })
            .collect();
        // Content may hang below the page; cover a screenful extra.
        let count = ((page_height.max(0.0) + 4096.0) / TILE_H as f32).ceil() as usize;
        let tile_bytes = (width * TILE_H * 4).max(1);
        TileCache {
            width,
            bg,
            extents,
            tiles: HashMap::new(),
            count,
            max_tiles: (TILE_CACHE_BYTES / tile_bytes).max(4),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    fn paint_tile(&self, i: usize, list: &DisplayList, atlas: &[GlyphBitmap]) -> Vec<u32> {
        let top = (i * TILE_H) as f32;
        let bottom = top + TILE_H as f32;
        let mut c = Canvas::new(self.width, TILE_H, self.bg);
        for (cmd, &(lo, hi)) in list.iter().zip(&self.extents) {
            if hi >= top && lo <= bottom {
                c.paint_cmd(cmd, atlas, -top, 0.0);
            }
        }
        c.pixels
    }

    /// Composite the page, scrolled by `scroll`, into `canvas` from row
    /// `top` down. Missing tiles are painted on demand.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        list: &DisplayList,
        atlas: &[GlyphBitmap],
        scroll: f32,
        top: usize,
    ) {
        let scroll = scroll.max(0.0).round() as usize;
        let view_h = canvas.height.saturating_sub(top);
        let first = scroll / TILE_H;
        let last = ((scroll + view_h) / TILE_H).min(self.count.saturating_sub(1));
        if first >= self.count {
            return;
        }
        for i in first..=last {
            if !self.tiles.contains_key(&i) {
                if self.tiles.len() >= self.max_tiles {
                    // Evict the tile farthest from what's on screen.
                    if let Some(&far) = self.tiles.keys().max_by_key(|&&k| k.abs_diff(first)) {
                        self.tiles.remove(&far);
                    }
                }
                let t = self.paint_tile(i, list, atlas);
                self.tiles.insert(i, t);
            }
            let tile = &self.tiles[&i];
            let w = self.width.min(canvas.width);
            for row in 0..TILE_H {
                let page_y = i * TILE_H + row;
                if page_y < scroll {
                    continue;
                }
                let y = top + (page_y - scroll);
                if y >= canvas.height {
                    break;
                }
                let dst = y * canvas.width;
                canvas.pixels[dst..dst + w].copy_from_slice(&tile[row * self.width..row * self.width + w]);
            }
        }
    }
}

fn pack(c: Color) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_and_odd_text_sizes_are_safe() {
        // Found by fuzzing: subnormal sizes made fontdue write out of bounds.
        let mut c = Canvas::new(32, 32, Color::WHITE);
        let mut cache = GlyphCache::default();
        for size in [1e-45, 9.18e-41, 1e-7, 0.5, 0.0, -3.0, f32::NAN, 1e9] {
            c.text(
                1.0,
                20.0,
                "Xa\u{1}\u{10}g",
                0,
                size,
                Color::BLACK,
                true,
                &mut cache,
                0.0,
            );
        }
        c.text(1.0, 20.0, "x", 99, 12.0, Color::BLACK, false, &mut cache, 0.0);

        // The renderer-side rasterizer skips the same sizes.
        let ops: Vec<DrawOp> = [1e-45f32, 0.5, f32::NAN, 1e9, 16.0]
            .iter()
            .map(|&size| DrawOp::Text {
                x: 0.0,
                baseline: 20.0,
                text: "Hi".into(),
                font: 0,
                size,
                color: Color::BLACK,
                italic: true,
                clip: None,
            })
            .collect();
        let (list, atlas) = rasterize(ops);
        assert_eq!(list.len(), 1, "only the 16px run is drawn");
        assert_eq!(atlas.len(), 2);
        c.paint(&list, &atlas, 0.0, 0.0);
    }

    #[test]
    fn tiles_match_direct_painting() {
        // Rects and text straddling tile boundaries, painted both ways.
        let mut ops = Vec::new();
        for i in 0..40 {
            let y = i as f32 * 97.0;
            ops.push(DrawOp::Rect {
                rect: Rect {
                    x: (i * 13 % 200) as f32,
                    y,
                    w: 300.0,
                    h: 140.0,
                },
                color: Color {
                    r: (i * 6) as u8,
                    g: 100,
                    b: 200,
                    a: if i % 3 == 0 { 128 } else { 255 },
                },
            });
            ops.push(DrawOp::Text {
                x: 20.0,
                baseline: y + 30.0,
                text: format!("line {i} across a tile edge"),
                font: (i % 6) as usize,
                size: 14.0 + (i % 5) as f32 * 7.0,
                color: Color::BLACK,
                italic: i % 2 == 0,
                clip: None,
            });
        }
        let (list, atlas) = rasterize(ops);
        let (w, h, top) = (420usize, 700usize, 40usize);
        let bg = Color::rgb(250, 250, 240);
        let mut tiles = TileCache::new(&list, &atlas, w, 4000.0, bg);
        for scroll in [0usize, 1, 300, 511, 512, 513, 1500, 3333] {
            let mut direct = Canvas::new(w, h, bg);
            direct.paint(&list, &atlas, scroll as f32, top as f32);
            let mut tiled = Canvas::new(w, h, bg);
            tiles.draw(&mut tiled, &list, &atlas, scroll as f32, top);
            assert!(
                direct.pixels[top * w..] == tiled.pixels[top * w..],
                "scroll {scroll}"
            );
        }
    }

    #[test]
    fn atlas_deduplicates_glyphs() {
        let op = |t: &str| DrawOp::Text {
            x: 0.0,
            baseline: 20.0,
            text: t.into(),
            font: 0,
            size: 14.0,
            color: Color::BLACK,
            italic: false,
            clip: None,
        };
        let (list, atlas) = rasterize(vec![op("banana"), op("nab")]);
        assert_eq!(atlas.len(), 3, "b, a, n once each");
        let placed: usize = list
            .iter()
            .map(|c| match c {
                DisplayCommand::Glyphs { glyphs, .. } => glyphs.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(placed, 9);
    }
}
