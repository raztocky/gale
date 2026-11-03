//! The wire protocol between the browser process and a sandboxed renderer.
//!
//! Frames are `u32` little-endian length + payload. The browser treats
//! everything a renderer sends as hostile: lengths are bounded, strings must be
//! UTF-8, numbers are sanitized and font indices are range-checked, so a
//! compromised renderer can't crash or confuse the browser through this
//! channel.

use crate::css::Color;
use crate::layout::Rect;
use crate::page::Rendered;
use crate::paint::DisplayCommand;
use crate::style::MAX_FONT_SIZE;
use std::io::{self, Read, Write};

/// Largest frame either side will accept.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Largest coordinate we accept from a renderer (keeps the painter's math sane).
const MAX_COORD: f32 = 10_000_000.0;
const FONT_COUNT: u8 = 6;

/// Browser -> renderer.
#[derive(Debug, Clone, PartialEq)]
pub enum ToRenderer {
    Load {
        url: String,
        html: String,
        blocker_css: String,
    },
    FetchReply(Result<String, String>),
    Render {
        width: f32,
        height: f32,
    },
    Dom,
}

/// Renderer -> browser.
#[derive(Debug, Clone, PartialEq)]
pub enum FromRenderer {
    Fetch { url: String },
    Loaded { title: String },
    Rendered(Rendered),
    Dom(String),
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Read one frame. `Ok(None)` means the other side closed the pipe cleanly.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = Vec::new();
    r.take(len as u64).read_to_end(&mut buf)?;
    if buf.len() != len {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated frame"));
    }
    Ok(Some(buf))
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn color(&mut self, c: Color) {
        self.0.extend_from_slice(&[c.r, c.g, c.b, c.a]);
    }
    fn rect(&mut self, r: Rect) {
        for v in [r.x, r.y, r.w, r.h] {
            self.f32(v);
        }
    }
}

struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
}

type DecResult<T> = Result<T, String>;

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> DecResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let s = self.buf.get(self.pos..end).ok_or("truncated message")?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> DecResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> DecResult<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// Finite and within ±MAX_COORD, or an error.
    fn f32(&mut self) -> DecResult<f32> {
        let v = f32::from_le_bytes(self.take(4)?.try_into().unwrap());
        if !v.is_finite() || v.abs() > MAX_COORD {
            return Err(format!("number out of range: {v}"));
        }
        Ok(v)
    }
    fn str(&mut self) -> DecResult<String> {
        let n = self.u32()? as usize;
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "invalid UTF-8".to_string())
    }
    fn color(&mut self) -> DecResult<Color> {
        let b = self.take(4)?;
        Ok(Color {
            r: b[0],
            g: b[1],
            b: b[2],
            a: b[3],
        })
    }
    fn rect(&mut self) -> DecResult<Rect> {
        Ok(Rect {
            x: self.f32()?,
            y: self.f32()?,
            w: self.f32()?,
            h: self.f32()?,
        })
    }
    /// A list count, sanity-checked against the bytes left (each item needs
    /// at least `min_item` bytes) so a forged count can't force a huge
    /// allocation.
    fn count(&mut self, min_item: usize) -> DecResult<usize> {
        let n = self.u32()? as usize;
        let left = self.buf.len() - self.pos;
        if n.saturating_mul(min_item) > left {
            return Err("list count exceeds message size".into());
        }
        Ok(n)
    }
    fn done(&self) -> DecResult<()> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err("trailing bytes in message".into())
        }
    }
}

impl ToRenderer {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        match self {
            ToRenderer::Load {
                url,
                html,
                blocker_css,
            } => {
                e.u8(1);
                e.str(url);
                e.str(html);
                e.str(blocker_css);
            }
            ToRenderer::FetchReply(r) => {
                e.u8(2);
                match r {
                    Ok(body) => {
                        e.u8(1);
                        e.str(body);
                    }
                    Err(err) => {
                        e.u8(0);
                        e.str(err);
                    }
                }
            }
            ToRenderer::Render { width, height } => {
                e.u8(3);
                e.f32(*width);
                e.f32(*height);
            }
            ToRenderer::Dom => e.u8(4),
        }
        e.0
    }

    pub fn decode(buf: &[u8]) -> DecResult<ToRenderer> {
        let mut d = Dec { buf, pos: 0 };
        let msg = match d.u8()? {
            1 => ToRenderer::Load {
                url: d.str()?,
                html: d.str()?,
                blocker_css: d.str()?,
            },
            2 => match d.u8()? {
                1 => ToRenderer::FetchReply(Ok(d.str()?)),
                _ => ToRenderer::FetchReply(Err(d.str()?)),
            },
            3 => ToRenderer::Render {
                width: d.f32()?,
                height: d.f32()?,
            },
            4 => ToRenderer::Dom,
            t => return Err(format!("unknown message type {t}")),
        };
        d.done()?;
        Ok(msg)
    }
}

impl FromRenderer {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        match self {
            FromRenderer::Fetch { url } => {
                e.u8(1);
                e.str(url);
            }
            FromRenderer::Loaded { title } => {
                e.u8(2);
                e.str(title);
            }
            FromRenderer::Rendered(r) => {
                e.u8(3);
                e.f32(r.height);
                e.color(r.background);
                e.u32(r.list.len() as u32);
                for cmd in &r.list {
                    match cmd {
                        DisplayCommand::Rect { rect, color } => {
                            e.u8(1);
                            e.rect(*rect);
                            e.color(*color);
                        }
                        DisplayCommand::Text {
                            x,
                            baseline,
                            text,
                            font,
                            size,
                            color,
                            italic,
                        } => {
                            e.u8(2);
                            e.f32(*x);
                            e.f32(*baseline);
                            e.str(text);
                            e.u8(*font as u8);
                            e.f32(*size);
                            e.color(*color);
                            e.u8(*italic as u8);
                        }
                    }
                }
                e.u32(r.links.len() as u32);
                for (rect, href) in &r.links {
                    e.rect(*rect);
                    e.str(href);
                }
            }
            FromRenderer::Dom(s) => {
                e.u8(4);
                e.str(s);
            }
        }
        e.0
    }

    /// Decode a message from a (possibly compromised) renderer.
    pub fn decode(buf: &[u8]) -> DecResult<FromRenderer> {
        let mut d = Dec { buf, pos: 0 };
        let msg = match d.u8()? {
            1 => FromRenderer::Fetch { url: d.str()? },
            2 => FromRenderer::Loaded { title: d.str()? },
            3 => {
                let height = d.f32()?.max(0.0);
                let background = d.color()?;
                let n = d.count(21)?;
                let mut list = Vec::with_capacity(n);
                for _ in 0..n {
                    list.push(match d.u8()? {
                        1 => DisplayCommand::Rect {
                            rect: d.rect()?,
                            color: d.color()?,
                        },
                        2 => {
                            let x = d.f32()?;
                            let baseline = d.f32()?;
                            let text = d.str()?;
                            let font = d.u8()?;
                            if font >= FONT_COUNT {
                                return Err(format!("bad font index {font}"));
                            }
                            let size = d.f32()?.clamp(0.0, MAX_FONT_SIZE);
                            let color = d.color()?;
                            let italic = d.u8()? != 0;
                            DisplayCommand::Text {
                                x,
                                baseline,
                                text,
                                font: font as usize,
                                size,
                                color,
                                italic,
                            }
                        }
                        t => return Err(format!("unknown display command {t}")),
                    });
                }
                let n = d.count(20)?;
                let mut links = Vec::with_capacity(n);
                for _ in 0..n {
                    links.push((d.rect()?, d.str()?));
                }
                FromRenderer::Rendered(Rendered {
                    list,
                    height,
                    background,
                    links,
                })
            }
            4 => FromRenderer::Dom(d.str()?),
            t => return Err(format!("unknown message type {t}")),
        };
        d.done()?;
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Rendered {
        Rendered {
            list: vec![
                DisplayCommand::Rect {
                    rect: Rect {
                        x: 1.0,
                        y: 2.0,
                        w: 3.0,
                        h: 4.0,
                    },
                    color: Color::rgb(1, 2, 3),
                },
                DisplayCommand::Text {
                    x: 5.0,
                    baseline: 6.0,
                    text: "héllo".into(),
                    font: 3,
                    size: 16.0,
                    color: Color::BLACK,
                    italic: true,
                },
            ],
            height: 99.0,
            background: Color::WHITE,
            links: vec![(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 10.0,
                },
                "/a".into(),
            )],
        }
    }

    #[test]
    fn round_trips() {
        let msgs = vec![
            FromRenderer::Fetch {
                url: "https://a/b.css".into(),
            },
            FromRenderer::Loaded { title: "T".into() },
            FromRenderer::Rendered(sample()),
            FromRenderer::Dom("<html>".into()),
        ];
        for m in msgs {
            assert_eq!(FromRenderer::decode(&m.encode()).unwrap(), m);
        }
        let msgs = vec![
            ToRenderer::Load {
                url: "u".into(),
                html: "<p>".into(),
                blocker_css: ".ad { display: none !important }".into(),
            },
            ToRenderer::FetchReply(Ok("css".into())),
            ToRenderer::FetchReply(Err("nope".into())),
            ToRenderer::Render {
                width: 800.0,
                height: 600.0,
            },
            ToRenderer::Dom,
        ];
        for m in msgs {
            assert_eq!(ToRenderer::decode(&m.encode()).unwrap(), m);
        }
    }

    #[test]
    fn rejects_hostile_messages() {
        let good = FromRenderer::Rendered(sample()).encode();
        // Every truncation is an error, never a panic.
        for i in 0..good.len() {
            assert!(FromRenderer::decode(&good[..i]).is_err());
        }
        // Forged huge list count.
        let mut bad = good.clone();
        bad[9..13].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(FromRenderer::decode(&bad).is_err());
        // NaN coordinates.
        let mut bad = good.clone();
        bad[1..5].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(FromRenderer::decode(&bad).is_err());
        // Out-of-range font index.
        let mut e = Enc::default();
        e.u8(3);
        e.f32(1.0);
        e.color(Color::WHITE);
        e.u32(1);
        e.u8(2);
        e.f32(0.0);
        e.f32(0.0);
        e.str("x");
        e.u8(200);
        e.f32(10.0);
        e.color(Color::BLACK);
        e.u8(0);
        e.u32(0);
        assert!(FromRenderer::decode(&e.0).unwrap_err().contains("font"));
        // Oversized frame header.
        let mut r: &[u8] = &(u32::MAX).to_le_bytes();
        assert!(read_frame(&mut r).is_err());
    }
}
