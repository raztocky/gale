//! Painting: layout tree -> display list -> pixels.

use crate::css::Color;
use crate::font;
use crate::layout::{BoxKind, LayoutBox, Rect};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCommand {
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
    },
}

pub type DisplayList = Vec<DisplayCommand>;

pub fn build_display_list(root: &LayoutBox) -> DisplayList {
    let mut list = Vec::new();
    render_box(&mut list, root);
    list
}

fn render_box(list: &mut DisplayList, b: &LayoutBox) {
    let s = b.style();
    if let BoxKind::Block(_) = b.kind {
        let bb = b.dims.border_box();
        if s.background.a > 0 {
            list.push(DisplayCommand::Rect {
                rect: bb,
                color: s.background,
            });
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
                list.push(DisplayCommand::Rect {
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
            list.push(DisplayCommand::Text {
                x: b.dims.content.x - w - s.font_size * 0.5,
                baseline,
                text: m.clone(),
                font: f,
                size: s.font_size,
                color: s.color,
                italic: false,
            });
        }
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
                list.push(DisplayCommand::Rect {
                    rect: Rect {
                        x: t.x,
                        y: t.baseline - a,
                        w: t.w,
                        h: a + d,
                    },
                    color: t.bg,
                });
            }
            if t.text != " " {
                list.push(DisplayCommand::Text {
                    x: t.x,
                    baseline: t.baseline,
                    text: t.text.clone(),
                    font: f,
                    size,
                    color: t.style.color,
                    italic: t.style.italic,
                });
            }
            let thick = (size / 14.0).max(1.0);
            if t.style.underline {
                list.push(DisplayCommand::Rect {
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
                list.push(DisplayCommand::Rect {
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
    for c in &b.children {
        render_box(list, c);
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

const GLYPH_CACHE_BYTES: usize = 32 * 1024 * 1024;

impl Canvas {
    pub fn new(width: usize, height: usize, bg: Color) -> Canvas {
        Canvas {
            pixels: vec![pack(bg); width * height],
            width,
            height,
        }
    }

    pub fn paint(&mut self, list: &DisplayList, scroll_y: f32, offset_y: f32, cache: &mut GlyphCache) {
        let dy = offset_y - scroll_y;
        for cmd in list {
            match cmd {
                DisplayCommand::Rect { rect, color } => self.fill(
                    Rect {
                        y: rect.y + dy,
                        ..*rect
                    },
                    *color,
                    offset_y,
                ),
                DisplayCommand::Text {
                    x,
                    baseline,
                    text,
                    font,
                    size,
                    color,
                    italic,
                } => {
                    let by = baseline + dy;
                    if by + size < offset_y || by - size * 1.5 > self.height as f32 {
                        continue;
                    }
                    self.text(*x, by, text, *font, *size, *color, *italic, cache, offset_y);
                }
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
        let f = &font::fonts()[font_idx];
        let mut pen = x;
        for ch in text.chars() {
            let size = size.clamp(0.0, crate::style::MAX_FONT_SIZE);
            let key = (font_idx, ch, (size * 4.0) as u32);
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
                let skew = if italic {
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

fn pack(c: Color) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32
}
