//! Bundled fonts and text measurement.
//!
//! Sixteen primary fonts: Liberation Sans, Serif and Mono (the same letter
//! widths as Arial, Times New Roman and Courier New, so pages lay out as
//! their authors expect) and Inter (for `system-ui` and friends), each in
//! regular, bold, italic and bold italic. Every copy of Brickle has exactly
//! these fonts and never looks at the system's, so fonts can't tell users
//! apart.
//!
//! Characters they lack fall back, in order, to DejaVu Sans (Greek, Hebrew,
//! Arabic, many symbols), then Noto subsets for Chinese/Japanese, Korean,
//! Devanagari, Thai, (monochrome) emoji and symbols; see
//! `assets/fonts/fallback/build.sh`. Fonts are parsed lazily, so a page only
//! pays for the CJK font (about 250 ms) if it actually contains CJK text.

use crate::style::{ComputedStyle, Family};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlineGlyphCollection, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use std::sync::OnceLock;

/// Number of fonts: indices 0..PRIMARY are the styled families (four per
/// family: regular, bold, italic, bold italic), the rest are fallbacks.
pub const COUNT: usize = 24;
pub const PRIMARY: usize = 16;
/// DejaVu Sans, the first fallback (regular; bold follows it).
const DEJAVU: usize = PRIMARY;

static FONT_DATA: [&[u8]; COUNT] = [
    include_bytes!("../assets/fonts/LiberationSans-Regular.ttf"),
    include_bytes!("../assets/fonts/LiberationSans-Bold.ttf"),
    include_bytes!("../assets/fonts/LiberationSans-Italic.ttf"),
    include_bytes!("../assets/fonts/LiberationSans-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-Regular.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-Bold.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-Italic.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-Regular.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-Bold.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-Italic.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../assets/fonts/Inter-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/DejaVuSans.ttf"),
    include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansCJK-Subset.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansKR-Subset.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansDevanagari.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansThai.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoEmoji.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansSymbols2.ttf"),
];

/// A parsed font: its tables, read with skrifa (Google's font-reading
/// crates), and what measuring and drawing need from them.
struct Face {
    charmap: skrifa::charmap::Charmap<'static>,
    outlines: OutlineGlyphCollection<'static>,
    /// Advance widths in font units.
    metrics: skrifa::metrics::GlyphMetrics<'static>,
    units_per_em: f32,
    /// `hhea` ascender and descender, in font units (descender negative).
    ascender: f32,
    descender: f32,
}

static FACES: [OnceLock<Face>; COUNT] = [const { OnceLock::new() }; COUNT];

/// The font at `i` (parsed on first use). Out-of-range indices give font 0.
fn face(i: usize) -> &'static Face {
    let i = if i < COUNT { i } else { 0 };
    FACES[i].get_or_init(|| {
        let font = FontRef::new(FONT_DATA[i]).expect("bundled font");
        let (ascender, descender) = font
            .hhea()
            .map(|h| (h.ascender().to_i16() as f32, h.descender().to_i16() as f32))
            .unwrap_or((800.0, -200.0));
        let units_per_em = font
            .head()
            .map(|h| h.units_per_em() as f32)
            .unwrap_or(1000.0)
            .max(1.0);
        Face {
            charmap: font.charmap(),
            outlines: font.outline_glyphs(),
            metrics: font.glyph_metrics(Size::unscaled(), LocationRef::default()),
            units_per_em,
            ascender,
            descender,
        }
    })
}

impl Face {
    fn glyph(&self, ch: char) -> GlyphId {
        self.charmap.map(ch).unwrap_or(GlyphId::NOTDEF)
    }

    fn advance(&self, g: GlyphId, size: f32) -> f32 {
        self.metrics.advance_width(g).unwrap_or(0.0) * size / self.units_per_em
    }
}

/// Where a rasterized glyph goes, like fontdue's: its bitmap is `width` x
/// `height`, its bottom-left corner `xmin` right of the pen and `ymin`
/// above the baseline (y up), and the pen moves on by `advance_width`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    pub xmin: i32,
    pub ymin: i32,
    pub width: usize,
    pub height: usize,
    pub advance_width: f32,
}

/// Largest glyph bitmap side drawn; bigger ones (only possible at absurd
/// sizes) come back empty.
const MAX_BITMAP_SIDE: usize = 2048;

/// Draw `ch` in font `i` at `size` px: its metrics, and its coverage
/// (0-255, row by row from the top).
pub fn rasterize(i: usize, ch: char, size: f32) -> (Metrics, Vec<u8>) {
    let f = face(i);
    let g = f.glyph(ch);
    let mut m = Metrics {
        advance_width: f.advance(g, size),
        ..Metrics::default()
    };
    if !(size.is_finite() && size > 0.0) {
        return (m, Vec::new());
    }
    let mut path = Path::default();
    let drawn = f.outlines.get(g).is_some_and(|o| {
        o.draw(
            DrawSettings::unhinted(Size::new(size), LocationRef::default()),
            &mut path,
        )
        .is_ok()
    });
    let Some((x0, y0, x1, y1)) = path.bounds().filter(|_| drawn) else {
        return (m, Vec::new());
    };
    let (xmin, ymin) = (x0.floor(), y0.floor());
    let (w, h) = ((x1.ceil() - xmin) as usize, (y1.ceil() - ymin) as usize);
    if w == 0 || h == 0 || w > MAX_BITMAP_SIDE || h > MAX_BITMAP_SIDE {
        return (m, Vec::new());
    }
    let mut r = Raster::new(w, h);
    // Into bitmap coordinates: left edge at 0, top at 0, y down.
    let top = ymin + h as f32;
    let at = |(x, y): (f32, f32)| (x - xmin, top - y);
    for (a, b) in path.lines() {
        r.line(at(a), at(b));
    }
    (m.xmin, m.ymin, m.width, m.height) = (xmin as i32, ymin as i32, w, h);
    (m, r.coverage())
}

/// A glyph outline, flattened to straight lines as it's drawn.
#[derive(Default)]
struct Path {
    points: Vec<(f32, f32)>,
    /// Where each contour starts in `points`.
    starts: Vec<usize>,
}

impl Path {
    fn last(&self) -> (f32, f32) {
        self.points.last().copied().unwrap_or((0.0, 0.0))
    }

    fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let finite = self.points.iter().all(|(x, y)| x.is_finite() && y.is_finite());
        (finite && !self.points.is_empty()).then(|| {
            self.points.iter().fold(
                (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                |(a, b, c, d), &(x, y)| (a.min(x), b.min(y), c.max(x), d.max(y)),
            )
        })
    }

    /// Every edge, each contour closed.
    fn lines(&self) -> impl Iterator<Item = ((f32, f32), (f32, f32))> + '_ {
        let ends = self.starts.iter().skip(1).copied().chain([self.points.len()]);
        self.starts.iter().zip(ends).flat_map(move |(&s, e)| {
            let c = &self.points[s..e];
            (0..c.len()).map(move |k| (c[k], c[(k + 1) % c.len()]))
        })
    }

    /// Points along a curve from the current point, `n` segments.
    fn flatten(&mut self, n: usize, at: impl Fn(f32) -> (f32, f32)) {
        for k in 1..=n {
            self.points.push(at(k as f32 / n as f32));
        }
    }
}

/// Segments for a curve whose control points stray `dev` px from a straight
/// line: enough that the error stays under about a tenth of a pixel.
fn segments(dev: f32) -> usize {
    if dev.is_finite() {
        ((dev * 10.0).sqrt().ceil() as usize).clamp(1, 64)
    } else {
        1
    }
}

impl OutlinePen for Path {
    fn move_to(&mut self, x: f32, y: f32) {
        self.starts.push(self.points.len());
        self.points.push((x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.points.push((x, y));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (x0, y0) = self.last();
        let dev = ((x0 - 2.0 * cx + x).hypot(y0 - 2.0 * cy + y)) / 4.0;
        self.flatten(segments(dev), |t| {
            let u = 1.0 - t;
            (
                u * u * x0 + 2.0 * u * t * cx + t * t * x,
                u * u * y0 + 2.0 * u * t * cy + t * t * y,
            )
        });
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (x0, y0) = self.last();
        let dev = (x0 - 2.0 * c1x + c2x).hypot(y0 - 2.0 * c1y + c2y)
            + (c1x - 2.0 * c2x + x).hypot(c1y - 2.0 * c2y + y);
        self.flatten(segments(dev * 0.75), |t| {
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            (
                a * x0 + b * c1x + c * c2x + d * x,
                a * y0 + b * c1y + c * c2y + d * y,
            )
        });
    }
    fn close(&mut self) {}
}

/// Anti-aliased coverage by signed-area accumulation (the method of
/// Raph Levien's font-rs, which fontdue also used): each edge adds the area
/// it covers to the cells it crosses, and a running sum along each row
/// gives every pixel's coverage.
struct Raster {
    w: usize,
    h: usize,
    acc: Vec<f32>,
}

impl Raster {
    fn new(w: usize, h: usize) -> Raster {
        // Room past the end for an edge on the bitmap's right side.
        Raster {
            w,
            h,
            acc: vec![0.0; w * h + w + 4],
        }
    }

    fn add(&mut self, i: isize, v: f32) {
        if let Some(a) = usize::try_from(i).ok().and_then(|i| self.acc.get_mut(i)) {
            *a += v;
        }
    }

    fn line(&mut self, p0: (f32, f32), p1: (f32, f32)) {
        if (p0.1 - p1.1).abs() <= f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 {
            (1.0, p0, p1)
        } else {
            (-1.0, p1, p0)
        };
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let mut x = p0.0;
        if p0.1 < 0.0 {
            x -= p0.1 * dxdy;
        }
        let y_end = (p1.1.ceil().max(0.0) as usize).min(self.h);
        for y in (p0.1.max(0.0) as usize)..y_end {
            let row = (y * self.w) as isize;
            let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
            let x_next = x + dxdy * dy;
            let d = dy * dir;
            let (xa, xb) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let (xa_floor, xb_ceil) = (xa.floor(), xb.ceil());
            let (ia, ib) = (xa_floor as isize, xb_ceil as isize);
            if ib <= ia + 1 {
                // Within one pixel column.
                let mid = 0.5 * (x + x_next) - xa_floor;
                self.add(row + ia, d - d * mid);
                self.add(row + ia + 1, d * mid);
            } else {
                let s = (xb - xa).recip();
                let fa = xa - xa_floor;
                let a0 = 0.5 * s * (1.0 - fa) * (1.0 - fa);
                let fb = xb - xb_ceil + 1.0;
                let am = 0.5 * s * fb * fb;
                self.add(row + ia, d * a0);
                if ib == ia + 2 {
                    self.add(row + ia + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - fa);
                    self.add(row + ia + 1, d * (a1 - a0));
                    for xi in ia + 2..ib - 1 {
                        self.add(row + xi, d * s);
                    }
                    let a2 = a1 + (ib - ia - 3) as f32 * s;
                    self.add(row + ib - 1, d * (1.0 - a2 - am));
                }
                self.add(row + ib, d * am);
            }
            x = x_next;
        }
    }

    fn coverage(&self) -> Vec<u8> {
        let mut sum = 0.0f32;
        self.acc[..self.w * self.h]
            .iter()
            .map(|a| {
                sum += a;
                (sum.abs().min(1.0) * 255.0).round() as u8
            })
            .collect()
    }
}

/// Parse the most used fonts now (the renderer does this before its
/// sandbox is sealed): regular and bold Sans and Serif. The rest are
/// parsed when a page first needs them.
pub fn preload() {
    for i in [0, 1, 4, 5] {
        face(i);
    }
}

/// Character coverage of the fallback fonts, read straight from their
/// `cmap` tables: deciding which font a character needs doesn't require
/// parsing the (large) font itself.
fn has_glyph(i: usize, ch: char) -> bool {
    use skrifa::raw::tables::cmap::Cmap;
    static CMAPS: [OnceLock<Option<Cmap<'static>>>; COUNT] = [const { OnceLock::new() }; COUNT];
    CMAPS[i]
        .get_or_init(|| FontRef::new(FONT_DATA[i]).ok().and_then(|f| f.cmap().ok()))
        .as_ref()
        .and_then(|c| c.map_codepoint(ch))
        .is_some_and(|g| g.to_u32() != 0)
}

/// The font that should draw `ch` when the style asks for `primary`.
pub fn resolve(primary: usize, ch: char) -> usize {
    if (ch as u32) < 0x80 || ch.is_whitespace() || primary >= PRIMARY {
        return primary;
    }
    if face(primary).glyph(ch) != GlyphId::NOTDEF {
        return primary;
    }
    // DejaVu Sans, keeping bold text bold.
    let dejavu = DEJAVU + (primary & 1);
    if has_glyph(dejavu, ch) {
        return dejavu;
    }
    (DEJAVU + 2..COUNT).find(|&i| has_glyph(i, ch)).unwrap_or(primary)
}

/// Whether italic text drawn in font `i` needs slanting by hand: the
/// fallbacks have no italic versions.
pub fn fake_italic(i: usize) -> bool {
    i >= PRIMARY
}

pub fn index(style: &ComputedStyle) -> usize {
    let fam = match style.family {
        Family::Sans => 0,
        Family::Serif => 4,
        Family::Mono => 8,
        Family::System => 12,
    };
    fam + 2 * style.italic as usize + style.bold as usize
}

pub fn measure(font_idx: usize, size: f32, text: &str) -> f32 {
    text.chars()
        .map(|c| {
            let f = face(resolve(font_idx, c));
            f.advance(f.glyph(c), size)
        })
        .sum()
}

/// (ascent, descent) in px, both positive, from the font's `hhea` table.
pub fn vmetrics(font_idx: usize, size: f32) -> (f32, f32) {
    let f = face(font_idx);
    let k = size / f.units_per_em;
    (f.ascender * k, -f.descender * k)
}

/// Does font `i` have a glyph for `ch`?
pub fn has(i: usize, ch: char) -> bool {
    face(i).glyph(ch) != GlyphId::NOTDEF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_picks_a_font_that_has_the_glyph() {
        assert_eq!(resolve(4, 'a'), 4);
        assert_eq!(resolve(0, 'ж'), 0, "Liberation has Cyrillic");
        assert_eq!(resolve(4, 'ש'), 4, "Liberation Serif has Hebrew");
        assert_eq!(resolve(1, 'ش'), 17, "bold Arabic from bold DejaVu Sans");
        for (ch, want) in [
            ('中', 18),
            ('の', 18),
            ('한', 19),
            ('क', 20),
            ('ก', 21),
            ('🐈', 22),
            ('⌖', 23),
        ] {
            let got = resolve(0, ch);
            assert_eq!(got, want, "{ch}");
            assert!(has(got, ch), "{ch} has a glyph");
        }
        // Any family missing a glyph gets the same fallback chain.
        assert_eq!(resolve(15, '中'), 18);
    }

    #[test]
    fn rasterizing_is_safe_at_any_size() {
        for size in [
            0.0,
            -3.0,
            f32::NAN,
            f32::INFINITY,
            1e-40,
            0.3,
            1.0,
            16.0,
            300.0,
            5000.0,
        ] {
            for i in 0..COUNT {
                for ch in ['A', 'g', '中', '🐈', '\u{0}', '\u{10ffff}'] {
                    let (m, bmp) = rasterize(i, ch, size);
                    assert_eq!(bmp.len(), m.width * m.height, "font {i} {ch:?} at {size}");
                    assert!(m.width <= MAX_BITMAP_SIDE && m.height <= MAX_BITMAP_SIDE);
                }
            }
        }
    }

    #[test]
    fn glyphs_look_like_glyphs() {
        // An "l" at 40px is a solid vertical bar: full coverage down its
        // middle, nothing around it.
        let (m, bmp) = rasterize(0, 'l', 40.0);
        assert!(m.height > 20 && m.width >= 2 && m.width < 10, "{m:?}");
        let mid = m.width / 2;
        assert!((2..m.height - 2).all(|y| bmp[y * m.width + mid] > 200));
        // An "o" has a hole in the middle.
        let (m, bmp) = rasterize(0, 'o', 40.0);
        let centre = bmp[(m.height / 2) * m.width + m.width / 2];
        assert!(centre < 30, "{centre} {m:?}");
        assert!(bmp.iter().map(|&v| v as u32).sum::<u32>() > 255 * 50);
        // Advances match the font's own widths: Liberation Sans is
        // metric-compatible with Arial, where "W" is 1933/2048 em.
        assert!((measure(0, 2048.0, "W") - 1933.0).abs() < 0.5);
    }

    #[test]
    fn every_style_has_its_own_font() {
        let mut s = ComputedStyle::root((800.0, 600.0));
        let mut seen = std::collections::HashSet::new();
        for family in [Family::Sans, Family::Serif, Family::Mono, Family::System] {
            for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                s.family = family;
                s.bold = bold;
                s.italic = italic;
                let i = index(&s);
                assert!(i < PRIMARY && !fake_italic(i));
                assert!(seen.insert(i));
                assert!(has(i, 'g'));
            }
        }
    }
}
