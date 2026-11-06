//! Image decoding: PNG, JPEG, GIF (first frame), WebP (first frame) and
//! SVG (shapes and text in Brickle's own fonts; no embedded images, nothing
//! external).
//!
//! With sandboxing enabled, images are decoded in disposable sandboxed workers: decoders are a
//! classic place for memory-safety bugs, and image bytes come straight from
//! websites. The browser only ever receives finished RGBA pixels, which it
//! checks before painting.
//!
//! Decompression bombs: an image's size is checked from its header before
//! any pixels are decoded, and every decoder is given a memory limit.

use crate::font;
use std::io::Cursor;
use std::sync::Arc;

/// Largest width or height accepted from an image header.
pub const MAX_SIDE: u32 = 16_384;
/// Largest pixel count accepted from an image header (about 160 MB decoded).
pub const MAX_PIXELS: u64 = 40_000_000;
/// Decoded images are scaled down to fit this, so a page's images can't
/// take up all the renderer's memory.
pub const STORE_SIDE: u32 = 2048;
/// Largest bitmap side sent to the browser; images shown larger are
/// enlarged by the browser when painted.
pub const MAX_DRAW_SIDE: u32 = STORE_SIDE;

/// RGBA pixels, 8 bits per channel, not premultiplied.
#[derive(Debug, Clone, PartialEq)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// A decoded image.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// The image's own size in CSS pixels, for layout.
    pub natural: (u32, u32),
    /// The pixels, scaled to fit [`STORE_SIDE`].
    pub bitmap: Arc<Bitmap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    WebP,
    Svg,
}

/// Which format the bytes are, from their signature (not the server's
/// `Content-Type`, which is often wrong).
pub fn sniff(b: &[u8]) -> Option<Format> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(Format::WebP)
    } else if looks_like_svg(b) {
        Some(Format::Svg)
    } else {
        None
    }
}

fn check_size(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("image has no pixels".into());
    }
    if w > MAX_SIDE || h > MAX_SIDE || w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("image is too large ({w}×{h})"));
    }
    Ok(())
}

/// Bytes a decoder may allocate for an image of this many pixels, with
/// room for its working buffers.
fn budget(w: u32, h: u32) -> usize {
    (w as usize * h as usize * 4) * 2 + (1 << 20)
}

/// Decode an image.
pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let bitmap = match sniff(bytes) {
        // Drawn straight at the size it's kept at, so it stays sharp.
        Some(Format::Svg) => return decode_svg(bytes),
        Some(Format::Png) => decode_png(bytes),
        Some(Format::Jpeg) => decode_jpeg(bytes),
        Some(Format::Gif) => decode_gif(bytes),
        Some(Format::WebP) => decode_webp(bytes),
        None => Err("not an image format Brickle can show".into()),
    }?;
    debug_assert_eq!(bitmap.rgba.len(), bitmap.w as usize * bitmap.h as usize * 4);
    let natural = (bitmap.w, bitmap.h);
    let (w, h) = fit(bitmap.w, bitmap.h, STORE_SIDE);
    let bitmap = if (w, h) == natural {
        bitmap
    } else {
        resize(&bitmap, w, h)
    };
    Ok(Decoded {
        natural,
        bitmap: Arc::new(bitmap),
    })
}

/// `(w, h)` scaled down, keeping the aspect ratio, so neither side is over `max`.
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let s = max as f64 / w.max(h) as f64;
    (
        ((w as f64 * s).round() as u32).clamp(1, max),
        ((h as f64 * s).round() as u32).clamp(1, max),
    )
}

/// XML whose first element (after any declaration, comments and
/// doctype) is `<svg`.
fn looks_like_svg(b: &[u8]) -> bool {
    let head = &b[..b.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let mut t = text.trim_start_matches('\u{feff}').trim_start();
    loop {
        if let Some(rest) = t.strip_prefix("<?") {
            t = rest.split_once("?>").map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = t.strip_prefix("<!--") {
            t = rest.split_once("-->").map_or("", |(_, r)| r).trim_start();
        } else if t.starts_with("<!") {
            t = t.split_once('>').map_or("", |(_, r)| r).trim_start();
        } else {
            break;
        }
    }
    t.starts_with("<svg") && t[4..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
}

/// Largest SVG file read.
const MAX_SVG_BYTES: usize = 4 * 1024 * 1024;
/// Most XML nodes in an SVG.
const MAX_SVG_NODES: u32 = 200_000;
/// Most elements an SVG may stand for once references (`<use>`,
/// `url(#…)`) are followed: a few nested references can otherwise stand for
/// billions.
const MAX_SVG_EXPANDED: u64 = 250_000;

/// How many elements `doc` stands for with every reference followed, or
/// None if that's over the limit (or a reference loops).
fn svg_expanded_size(doc: &roxmltree::Document) -> Option<u64> {
    use std::collections::HashMap;
    let ids: HashMap<&str, roxmltree::NodeId> = doc
        .descendants()
        .filter_map(|n| Some((n.attribute("id")?, n.id())))
        .collect();
    // The ids an element refers to: `href="#x"`, and `url(#x)` anywhere.
    fn refs<'a>(n: roxmltree::Node<'a, '_>) -> Vec<&'a str> {
        let mut out = vec![];
        for a in n.attributes() {
            let v = a.value();
            if a.name() == "href" {
                if let Some(id) = v.trim().strip_prefix('#') {
                    out.push(id);
                }
            }
            let mut rest = v;
            while let Some(i) = rest.find("url(") {
                rest = &rest[i + 4..];
                let inner = rest
                    .split(')')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'');
                if let Some(id) = inner.strip_prefix('#') {
                    out.push(id);
                }
            }
        }
        out
    }
    // cost(n) = 1 + its children's + what it refers to; None = too much.
    // Iterative, so deep documents can't overflow the stack.
    let mut cost: HashMap<roxmltree::NodeId, u64> = HashMap::new();
    let mut visiting = std::collections::HashSet::new();
    let mut stack = vec![(doc.root_element(), false)];
    while let Some((n, expanded)) = stack.pop() {
        if cost.contains_key(&n.id()) {
            continue;
        }
        let deps: Vec<roxmltree::Node> = n
            .children()
            .filter(|c| c.is_element())
            .chain(
                refs(n)
                    .into_iter()
                    .filter_map(|id| ids.get(id).map(|&i| doc.get_node(i).unwrap())),
            )
            .collect();
        if !expanded {
            if !visiting.insert(n.id()) {
                return None; // a reference loop
            }
            stack.push((n, true));
            for d in deps {
                if !cost.contains_key(&d.id()) {
                    if visiting.contains(&d.id()) {
                        return None;
                    }
                    stack.push((d, false));
                }
            }
        } else {
            visiting.remove(&n.id());
            let mut total = 1u64;
            for d in deps {
                total = total.saturating_add(*cost.get(&d.id())?);
            }
            if total > MAX_SVG_EXPANDED {
                return None;
            }
            cost.insert(n.id(), total);
        }
    }
    cost.get(&doc.root_element().id()).copied()
}

/// Brickle's fonts for SVG text (loaded on first use), and their ids by
/// [`font::index_of`] index.
type SvgFonts = (Arc<usvg::fontdb::Database>, Vec<Option<usvg::fontdb::ID>>);

fn svg_fonts() -> &'static SvgFonts {
    static FONTS: std::sync::OnceLock<SvgFonts> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        let ids = (0..font::COUNT)
            .map(|i| {
                let src = usvg::fontdb::Source::Binary(Arc::new(font::data(i)));
                db.load_font_source(src).first().copied()
            })
            .collect();
        (Arc::new(db), ids)
    })
}

/// The bundled font for an SVG `font-family` list: named families as in
/// HTML (Arial is Liberation Sans...), generic ones directly, and serif
/// when nothing matches, as in browsers.
fn svg_font(f: &usvg::Font, _: &mut Arc<usvg::fontdb::Database>) -> Option<usvg::fontdb::ID> {
    use crate::style::{family_of, Family};
    let family = f
        .families()
        .iter()
        .find_map(|fam| match fam {
            usvg::FontFamily::Named(n) => family_of(&n.to_ascii_lowercase()),
            usvg::FontFamily::Serif => Some(Family::Serif),
            usvg::FontFamily::Monospace => Some(Family::Mono),
            usvg::FontFamily::SansSerif | usvg::FontFamily::Cursive | usvg::FontFamily::Fantasy => {
                Some(Family::Sans)
            }
        })
        .unwrap_or(Family::Serif);
    let italic = f.style() != usvg::FontStyle::Normal;
    *svg_fonts()
        .1
        .get(font::index_of(family, f.weight() >= 600, italic))?
}

fn decode_svg(bytes: &[u8]) -> Result<Decoded, String> {
    if bytes.len() > MAX_SVG_BYTES {
        return Err("SVG: file too large".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "SVG: not UTF-8")?;
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            // Many SVGs have a doctype; roxmltree limits entity expansion.
            allow_dtd: true,
            nodes_limit: MAX_SVG_NODES,
            // External entities are never loaded (no resolver).
            ..Default::default()
        },
    )
    .map_err(|e| format!("SVG: {e}"))?;
    if doc.root_element().tag_name().name() != "svg" {
        return Err("SVG: not an <svg> document".into());
    }
    svg_expanded_size(&doc).ok_or("SVG: too complex")?;
    // Nothing outside the file is ever used: no linked or embedded images
    // (and no nested SVGs), no files. Text is drawn with Brickle's own
    // fonts, never the system's.
    let has_text = doc.descendants().any(|n| n.has_tag_name("text"));
    let opt = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: if has_text {
            svg_fonts().0.clone()
        } else {
            Default::default()
        },
        font_resolver: usvg::FontResolver {
            select_font: Box::new(svg_font),
            select_fallback: usvg::FontResolver::default_fallback_selector(),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_xmltree(&doc, &opt).map_err(|e| format!("SVG: {e}"))?;
    let size = tree.size();
    // Absurd geometry (a stroke billions of pixels wide) trips up the
    // rasterizer (found by fuzzing); no real drawing reaches this far.
    let reach = MAX_SIDE as f32 * 64.0;
    let scale = STORE_SIDE as f32 / size.width().min(size.height()).max(1e-3);
    // (A filtered group's layer box is its filter region, which leaves its
    // strokes out, so both are checked.)
    let root = tree.root();
    let fits = |b: usvg::Rect| {
        [b.left(), b.top(), b.right(), b.bottom()]
            .iter()
            .all(|v| v.is_finite() && (v * scale.max(1.0)).abs() <= reach)
    };
    let group_fits =
        |g: &usvg::Group| fits(g.abs_layer_bounding_box().to_rect()) && fits(g.abs_stroke_bounding_box());
    // Clip paths and masks are drawn too, but no bounding box covers them.
    let clips_fit = tree.clip_paths().iter().all(|c| group_fits(c.root()));
    let masks_fit = tree
        .masks()
        .iter()
        .all(|m| fits(m.rect().to_rect()) && group_fits(m.root()));
    if !group_fits(root) || !clips_fit || !masks_fit {
        return Err("SVG: drawing far too large".into());
    }
    let natural = (
        (size.width().ceil() as u32).clamp(1, MAX_SIDE),
        (size.height().ceil() as u32).clamp(1, MAX_SIDE),
    );
    let (w, h) = fit(natural.0, natural.1, STORE_SIDE);
    check_size(w, h)?;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("SVG: can't make a canvas")?;
    let transform = tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Ok(Decoded {
        natural,
        bitmap: Arc::new(Bitmap { w, h, rgba }),
    })
}

fn decode_png(bytes: &[u8]) -> Result<Bitmap, String> {
    let err = |e: png::DecodingError| format!("PNG: {e}");
    // First just the header, to check the size before allocating.
    let (w, h) = {
        let d = png::Decoder::new(Cursor::new(bytes));
        let r = d.read_info().map_err(err)?;
        (r.info().width, r.info().height)
    };
    check_size(w, h)?;
    let mut d = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: budget(w, h) });
    d.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = d.read_info().map_err(err)?;
    // None if the size doesn't fit in memory at all.
    let size = r.output_buffer_size().ok_or("PNG: image too large")?;
    let mut buf = vec![0; size];
    let out = r.next_frame(&mut buf).map_err(err)?;
    buf.truncate(out.buffer_size());
    let px = w as usize * h as usize;
    let rgba = match out.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("PNG: palette not expanded".into()),
    };
    if rgba.len() != px * 4 {
        return Err("PNG: truncated image".into());
    }
    Ok(Bitmap { w, h, rgba })
}

fn decode_jpeg(bytes: &[u8]) -> Result<Bitmap, String> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .set_max_width(MAX_SIDE as usize)
        .set_max_height(MAX_SIDE as usize)
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    d.decode_headers().map_err(|e| format!("JPEG: {e:?}"))?;
    let info = d.info().ok_or("JPEG: no header")?;
    let (w, h) = (info.width as u32, info.height as u32);
    check_size(w, h)?;
    let rgba = d.decode().map_err(|e| format!("JPEG: {e:?}"))?;
    if rgba.len() != w as usize * h as usize * 4 {
        return Err("JPEG: unexpected output size".into());
    }
    Ok(Bitmap { w, h, rgba })
}

fn decode_gif(bytes: &[u8]) -> Result<Bitmap, String> {
    let err = |e: gif::DecodingError| format!("GIF: {e}");
    let mut o = gif::DecodeOptions::new();
    o.set_color_output(gif::ColorOutput::RGBA);
    let limit = std::num::NonZeroU64::new(budget(MAX_SIDE, MAX_SIDE) as u64).expect("nonzero");
    o.set_memory_limit(gif::MemoryLimit::Bytes(limit));
    let mut d = o.read_info(Cursor::new(bytes)).map_err(err)?;
    let (w, h) = (d.width() as u32, d.height() as u32);
    check_size(w, h)?;
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    // The first frame only (no animation), drawn where it says on the canvas.
    if let Some(f) = d.read_next_frame().map_err(err)? {
        let (fl, ft, fw, fh) = (
            f.left as usize,
            f.top as usize,
            f.width as usize,
            f.height as usize,
        );
        if f.buffer.len() >= fw * fh * 4 {
            for row in 0..fh {
                let y = ft + row;
                if y >= h as usize {
                    break;
                }
                for col in 0..fw {
                    let x = fl + col;
                    if x >= w as usize {
                        break;
                    }
                    let s = (row * fw + col) * 4;
                    if f.buffer[s + 3] != 0 {
                        let o = (y * w as usize + x) * 4;
                        rgba[o..o + 4].copy_from_slice(&f.buffer[s..s + 4]);
                    }
                }
            }
        }
    }
    Ok(Bitmap { w, h, rgba })
}

fn decode_webp(bytes: &[u8]) -> Result<Bitmap, String> {
    let err = |e: image_webp::DecodingError| format!("WebP: {e}");
    let mut d = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(err)?;
    let (w, h) = d.dimensions();
    check_size(w, h)?;
    d.set_memory_limit(budget(w, h));
    let size = d.output_buffer_size().ok_or("WebP: image is too large")?;
    let mut buf = vec![0u8; size];
    d.read_image(&mut buf).map_err(err)?;
    let px = w as usize * h as usize;
    let rgba = if d.has_alpha() {
        buf
    } else {
        buf.as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect()
    };
    if rgba.len() != px * 4 {
        return Err("WebP: unexpected output size".into());
    }
    Ok(Bitmap { w, h, rgba })
}

/// Scale a bitmap to `w`×`h`: averaging when shrinking, linear when
/// enlarging. Done with premultiplied alpha so transparent edges don't go dark.
/// The pixels of `src` in columns `x0..x1` and rows `y0..y1` (clamped).
pub fn crop(src: &Bitmap, x0: u32, y0: u32, x1: u32, y1: u32) -> Bitmap {
    let (x1, y1) = (x1.min(src.w), y1.min(src.h));
    let (x0, y0) = (x0.min(x1), y0.min(y1));
    let (w, h) = (x1 - x0, y1 - y0);
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in y0..y1 {
        let row = (y as usize * src.w as usize + x0 as usize) * 4;
        rgba.extend_from_slice(&src.rgba[row..row + w as usize * 4]);
    }
    Bitmap { w, h, rgba }
}

pub fn resize(src: &Bitmap, w: u32, h: u32) -> Bitmap {
    let (w, h) = (w.max(1), h.max(1));
    if (w, h) == (src.w, src.h) {
        return src.clone();
    }
    // Shrinking a lot: average whole blocks first (cheap, and keeps the
    // working memory proportional to the result, not the source).
    let (kx, ky) = ((src.w / w).max(1), (src.h / h).max(1));
    if kx > 1 || ky > 1 {
        let reduced = box_reduce(src, kx, ky);
        if (reduced.w, reduced.h) == (w, h) {
            return reduced;
        }
        return resize(&reduced, w, h);
    }
    let premul: Vec<f32> = src
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]
        })
        .collect();
    // Horizontal pass, then vertical.
    let tmp = resample(&premul, src.w as usize, src.h as usize, w as usize, true);
    let out = resample(&tmp, w as usize, src.h as usize, h as usize, false);
    let rgba = out
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = p[3].clamp(0.0, 255.0);
            if a < 0.5 {
                return [0, 0, 0, 0];
            }
            let k = 255.0 / a;
            [
                (p[0] * k).round().clamp(0.0, 255.0) as u8,
                (p[1] * k).round().clamp(0.0, 255.0) as u8,
                (p[2] * k).round().clamp(0.0, 255.0) as u8,
                a.round() as u8,
            ]
        })
        .collect();
    Bitmap { w, h, rgba }
}

/// Average each `kx`×`ky` block of pixels into one (premultiplied, so
/// transparent pixels don't darken their neighbours). Leftover edge pixels
/// are dropped.
fn box_reduce(src: &Bitmap, kx: u32, ky: u32) -> Bitmap {
    let (w, h) = ((src.w / kx).max(1), (src.h / ky).max(1));
    let (kx, ky) = (kx.min(src.w) as usize, ky.min(src.h) as usize);
    let sw = src.w as usize;
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    let mut acc = vec![0u64; w as usize * 4];
    let n = (kx * ky) as u64;
    for oy in 0..h as usize {
        acc.iter_mut().for_each(|v| *v = 0);
        for y in oy * ky..oy * ky + ky {
            let row = &src.rgba[y * sw * 4..(y + 1) * sw * 4];
            for ox in 0..w as usize {
                let a = &mut acc[ox * 4..ox * 4 + 4];
                for p in row[ox * kx * 4..(ox * kx + kx) * 4].as_chunks::<4>().0.iter() {
                    let al = p[3] as u64;
                    a[0] += p[0] as u64 * al;
                    a[1] += p[1] as u64 * al;
                    a[2] += p[2] as u64 * al;
                    a[3] += al;
                }
            }
        }
        for a in acc.as_chunks::<4>().0.iter() {
            // Colour weighted by alpha; fully transparent stays black.
            let c = |v: u64| v.checked_div(a[3]).unwrap_or(0) as u8;
            rgba.extend_from_slice(&[c(a[0]), c(a[1]), c(a[2]), ((a[3] + n / 2) / n) as u8]);
        }
    }
    Bitmap { w, h, rgba }
}

/// Resample one axis of a `sw`×`sh` 4-channel image to `n` samples along
/// that axis (`horizontal`: width becomes `n`; otherwise height does).
fn resample(src: &[f32], sw: usize, sh: usize, n: usize, horizontal: bool) -> Vec<f32> {
    let (len, lines) = if horizontal { (sw, sh) } else { (sh, sw) };
    let (ow, oh) = if horizontal { (n, sh) } else { (sw, n) };
    let mut out = vec![0f32; ow * oh * 4];
    let at = |line: usize, i: usize| -> usize {
        if horizontal {
            (line * sw + i) * 4
        } else {
            (i * sw + line) * 4
        }
    };
    let put = |line: usize, i: usize| -> usize {
        if horizontal {
            (line * ow + i) * 4
        } else {
            (i * ow + line) * 4
        }
    };
    let scale = len as f32 / n as f32;
    for line in 0..lines {
        for i in 0..n {
            let mut acc = [0f32; 4];
            if scale > 1.0 {
                // Average the source pixels this one covers, weighted by overlap.
                let (a, b) = (i as f32 * scale, (i + 1) as f32 * scale);
                let mut j = a.floor() as usize;
                let mut total = 0.0;
                while (j as f32) < b && j < len {
                    let wgt = ((j + 1) as f32).min(b) - (j as f32).max(a);
                    let s = at(line, j);
                    for c in 0..4 {
                        acc[c] += src[s + c] * wgt;
                    }
                    total += wgt;
                    j += 1;
                }
                if total > 0.0 {
                    for v in &mut acc {
                        *v /= total;
                    }
                }
            } else {
                let x = ((i as f32 + 0.5) * scale - 0.5).max(0.0);
                let j = (x.floor() as usize).min(len - 1);
                let k = (j + 1).min(len - 1);
                let t = x - j as f32;
                let (s, u) = (at(line, j), at(line, k));
                for c in 0..4 {
                    acc[c] = src[s + c] * (1.0 - t) + src[u + c] * t;
                }
            }
            let o = put(line, i);
            out[o..o + 4].copy_from_slice(&acc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(color);
            e.set_depth(png::BitDepth::Eight);
            let mut wr = e.write_header().unwrap();
            wr.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn decodes_png() {
        let d = decode(&png_bytes(2, 1, png::ColorType::Rgb, &[255, 0, 0, 0, 0, 255])).unwrap();
        assert_eq!(d.natural, (2, 1));
        assert_eq!(d.bitmap.rgba, vec![255, 0, 0, 255, 0, 0, 255, 255]);
        let g = decode(&png_bytes(1, 1, png::ColorType::GrayscaleAlpha, &[10, 20])).unwrap();
        assert_eq!(g.bitmap.rgba, vec![10, 10, 10, 20]);
    }

    #[test]
    fn decodes_gif_first_frame() {
        let mut out = Vec::new();
        {
            let mut e = gif::Encoder::new(&mut out, 2, 2, &[0, 0, 0, 255, 255, 255]).unwrap();
            let mut f = gif::Frame::default();
            (f.width, f.height) = (2, 2);
            f.buffer = std::borrow::Cow::Owned(vec![0, 1, 1, 0]);
            e.write_frame(&f).unwrap();
        }
        let d = decode(&out).unwrap();
        assert_eq!(d.natural, (2, 2));
        assert_eq!(&d.bitmap.rgba[0..8], &[0, 0, 0, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn refuses_huge_headers_before_decoding() {
        // A PNG header claiming 100000×100000 pixels, with no pixel data.
        let mut b = png_bytes(1, 1, png::ColorType::Grayscale, &[0]);
        b[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        b[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        // Fix the IHDR checksum so the decoder gets as far as the size.
        let crc = crc32(&b[12..29]);
        b[29..33].copy_from_slice(&crc.to_be_bytes());
        let e = decode(&b).unwrap_err();
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(decode(b"").is_err());
        assert!(decode(b"<svg").is_err());
        assert!(decode(b"<html><svg></svg></html>").is_err());
        assert!(decode(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]).is_err());
        assert!(decode(b"GIF89a\x02\x00").is_err());
        assert!(decode(b"RIFF\x10\x00\x00\x00WEBPVP8 ").is_err());
        let p = png_bytes(4, 4, png::ColorType::Rgb, &[7; 48]);
        assert!(decode(&p[..p.len() - 20]).is_err());
    }

    const NS: &str = "xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink'";

    #[test]
    fn svg_shapes_are_drawn() {
        let svg = format!(
            "<?xml version='1.0'?><!-- logo --><!DOCTYPE svg><svg {NS} width='4' height='2'>\
             <rect width='2' height='2' fill='red'/><rect x='2' width='2' height='2' fill='#00f' opacity='0.5'/></svg>"
        );
        assert_eq!(sniff(svg.as_bytes()), Some(Format::Svg));
        let d = decode(svg.as_bytes()).unwrap();
        assert_eq!(d.natural, (4, 2));
        assert_eq!(&d.bitmap.rgba[0..4], &[255, 0, 0, 255]);
        let blue = &d.bitmap.rgba[3 * 4..4 * 4];
        assert_eq!((blue[0], blue[2]), (0, 255));
        assert!((120..=135).contains(&blue[3]), "{blue:?}");
        // Big drawings are drawn at the stored size, not enlarged later.
        let big = format!(
            "<svg {NS} viewBox='0 0 10 10' width='5000' height='2500'><circle cx='5' cy='5' r='5'/></svg>"
        );
        let d = decode(big.as_bytes()).unwrap();
        assert_eq!(d.natural, (5000, 2500));
        assert_eq!((d.bitmap.w, d.bitmap.h), (STORE_SIDE, STORE_SIDE / 2));
    }

    #[test]
    fn svg_never_reaches_outside_the_file() {
        // Linked and embedded images and nested SVGs are all left out; the
        // rest is still drawn.
        let svg = format!(
            "<svg {NS} width='2' height='2'>\
             <image href='file:///etc/passwd' width='2' height='2'/>\
             <image xlink:href='https://tracker.example/x.png' width='2' height='2'/>\
             <image href='data:image/svg+xml;base64,PHN2Zy8+' width='2' height='2'/>\
             <script>alert(1)</script>\
             <a href='https://example.com'><rect width='1' height='1' fill='lime'/></a></svg>"
        );
        let d = decode(svg.as_bytes()).unwrap();
        assert_eq!(&d.bitmap.rgba[0..4], &[0, 255, 0, 255]);
        assert_eq!(d.bitmap.rgba[3 * 4 + 3], 0, "nothing else drawn");
    }

    #[test]
    fn svg_text_uses_the_bundled_fonts() {
        let ink = |family: &str| {
            let svg = format!(
                "<svg {NS} width='120' height='40'><text x='2' y='30' font-size='28' \
                 font-family=\"{family}\" font-weight='bold'>Hello</text></svg>"
            );
            let d = decode(svg.as_bytes()).unwrap();
            // Columns with any ink: how wide the word came out.
            let w = d.bitmap.w as usize;
            (0..w)
                .filter(|x| (0..d.bitmap.h as usize).any(|y| d.bitmap.rgba[(y * w + x) * 4 + 3] > 128))
                .count()
        };
        let (arial, courier, unknown) = (ink("Arial, sans-serif"), ink("'Courier New'"), ink("NoSuchFont"));
        assert!(arial > 40, "text is drawn: {arial}");
        // Arial is Liberation Sans, Courier New is Liberation Mono (wider),
        // and an unknown family is serif, as in browsers.
        assert!(courier > arial, "{courier} vs {arial}");
        assert!(unknown > 20 && unknown != arial, "{unknown}");
    }

    #[test]
    fn svg_bombs_are_refused() {
        // Ten levels of ten <use>s: 10^10 elements.
        let mut s = format!("<svg {NS} width='10' height='10'><defs><rect id='l0' width='1' height='1'/>");
        for i in 1..=10 {
            s.push_str(&format!("<g id='l{i}'>"));
            for _ in 0..10 {
                s.push_str(&format!("<use href='#l{}'/>", i - 1));
            }
            s.push_str("</g>");
        }
        s.push_str("</defs><use href='#l10'/></svg>");
        assert!(decode(s.as_bytes()).unwrap_err().contains("too complex"));
        // A blurred path with a stroke billions of pixels wide panicked the
        // rasterizer (found by fuzzing).
        let wide = format!(
            "<svg {NS}><defs><filter id='f'><feGaussianBlur/></filter><path id='p' d='M8 2 1 1Z'/></defs>\
             <use href='#p' stroke='#000' stroke-width='5555555555' filter='url(#f)'/></svg>"
        );
        assert!(decode(wide.as_bytes()).unwrap_err().contains("far too large"));
        // So did a clip path or mask that size, which no bounding box covers.
        for defs in [
            "<clipPath id='c'><circle cx='8' cy='8' r='5555555555555555555555557'/></clipPath>",
            "<mask id='c'><circle cx='8' cy='8' r='5555555555555555555555557' fill='#fff'/></mask>",
        ] {
            let kind = if defs.starts_with("<clip") {
                "clip-path"
            } else {
                "mask"
            };
            let svg = format!(
                "<svg {NS} width='16' height='16'><defs>{defs}</defs>\
                 <rect width='16' height='16' fill='red' {kind}='url(#c)'/></svg>"
            );
            assert!(
                decode(svg.as_bytes()).unwrap_err().contains("far too large"),
                "{kind}"
            );
        }
        // Through fill references too.
        let fill = format!(
            "<svg {NS}><linearGradient id='a' href='#b'/><linearGradient id='b' href='#a'/>\
             <rect width='1' height='1' fill='url(#a)'/></svg>"
        );
        assert!(decode(fill.as_bytes()).is_err(), "loop");
        let me = format!("<svg {NS}><g id='a'><use href='#a'/></g></svg>");
        assert!(decode(me.as_bytes()).is_err(), "self-reference");
        // XML entity expansion.
        let lol = format!(
            "<?xml version='1.0'?><!DOCTYPE svg [<!ENTITY a 'aaaaaaaaaa'><!ENTITY b '&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;'>\
             <!ENTITY c '&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;'><!ENTITY d '&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;'>\
             <!ENTITY e '&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;'><!ENTITY f '&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;'>\
             <!ENTITY g '&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;'><!ENTITY h '&g;&g;&g;&g;&g;&g;&g;&g;&g;&g;'>]>\
             <svg {NS}><desc>&h;</desc></svg>"
        );
        let start = std::time::Instant::now();
        let _ = decode(lol.as_bytes());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        // Too many nodes.
        let many = format!("<svg {NS}>{}</svg>", "<g/>".repeat(MAX_SVG_NODES as usize));
        assert!(decode(many.as_bytes()).is_err());
    }

    #[test]
    fn big_images_are_stored_smaller() {
        let w = STORE_SIDE * 2;
        let d = decode(&png_bytes(
            w,
            2,
            png::ColorType::Grayscale,
            &vec![128; w as usize * 2],
        ))
        .unwrap();
        assert_eq!(d.natural, (w, 2));
        assert_eq!((d.bitmap.w, d.bitmap.h), (STORE_SIDE, 1));
        assert!(d.bitmap.rgba.chunks(4).all(|p| p == [128, 128, 128, 255]));
    }

    #[test]
    fn resize_keeps_colors_and_transparent_edges() {
        // Half transparent, half opaque red: shrinking must not darken the red.
        let src = Bitmap {
            w: 2,
            h: 1,
            rgba: vec![0, 0, 0, 0, 255, 0, 0, 255],
        };
        let r = resize(&src, 1, 1);
        assert_eq!(&r.rgba[0..3], &[255, 0, 0]);
        assert!((120..=135).contains(&r.rgba[3]));
        let up = resize(&src, 8, 4);
        assert_eq!(up.rgba.len(), 8 * 4 * 4);
        assert!(up.rgba.chunks(4).all(|p| p[3] == 0 || p[0] == 255));
        assert_eq!(fit(4000, 1000, 2000), (2000, 500));
        assert_eq!(fit(10, 10, 2000), (10, 10));
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    fn px(b: &Bitmap, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * b.w + x) * 4) as usize;
        b.rgba[i..i + 4].try_into().unwrap()
    }

    fn near(a: [u8; 4], b: [u8; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 12)
    }

    #[test]
    fn decodes_jpeg() {
        let d = decode(include_bytes!("../tests/data/tiny.jpg")).unwrap();
        assert_eq!(d.natural, (16, 8));
        assert!(
            near(px(&d.bitmap, 2, 4), [200, 40, 40, 255]),
            "{:?}",
            px(&d.bitmap, 2, 4)
        );
        assert!(
            near(px(&d.bitmap, 13, 4), [40, 40, 200, 255]),
            "{:?}",
            px(&d.bitmap, 13, 4)
        );
    }

    #[test]
    fn decodes_webp_lossy_and_lossless_with_alpha() {
        let d = decode(include_bytes!("../tests/data/tiny-lossy.webp")).unwrap();
        assert_eq!(d.natural, (16, 8));
        assert!(
            near(px(&d.bitmap, 2, 4), [200, 40, 40, 255]),
            "{:?}",
            px(&d.bitmap, 2, 4)
        );
        let a = decode(include_bytes!("../tests/data/tiny-alpha.webp")).unwrap();
        assert_eq!(a.natural, (4, 4));
        assert_eq!(px(&a.bitmap, 1, 1), [0, 200, 0, 128]);
    }

    #[test]
    fn every_truncation_is_an_error_not_a_crash() {
        for data in [
            &include_bytes!("../tests/data/tiny.jpg")[..],
            include_bytes!("../tests/data/tiny-lossy.webp"),
            include_bytes!("../tests/data/tiny-alpha.webp"),
        ] {
            for n in 0..data.len() {
                let _ = decode(&data[..n]);
            }
        }
    }
}
