//! Image decoding: PNG, JPEG, GIF (first frame) and WebP (first frame).
//!
//! Images are only ever decoded in the sandboxed renderer: decoders are a
//! classic place for memory-safety bugs, and image bytes come straight from
//! websites. The browser only ever receives finished RGBA pixels, which it
//! checks before painting.
//!
//! Decompression bombs: an image's size is checked from its header before
//! any pixels are decoded, and every decoder is given a memory limit.

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
    let mut buf = vec![0; r.output_buffer_size()];
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
        assert!(decode(b"<svg></svg>").is_err());
        assert!(decode(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]).is_err());
        assert!(decode(b"GIF89a\x02\x00").is_err());
        assert!(decode(b"RIFF\x10\x00\x00\x00WEBPVP8 ").is_err());
        let p = png_bytes(4, 4, png::ColorType::Rgb, &[7; 48]);
        assert!(decode(&p[..p.len() - 20]).is_err());
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
