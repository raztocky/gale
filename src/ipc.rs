//! The wire protocol between the browser process and a sandboxed renderer.
//!
//! Frames are `u32` little-endian length + payload. The browser treats
//! everything a renderer sends as hostile: lengths are bounded, strings must be
//! UTF-8, numbers are sanitized and font indices are range-checked, so a
//! compromised renderer can't crash or confuse the browser through this
//! channel.

use crate::css::Color;
use crate::forms::Submission;
use crate::layout::Rect;
use crate::net::{Credentials, Dest, ScriptRequest};
use crate::network::ScriptResponse;
use crate::page::Key;
use crate::page::Rendered;
use crate::paint::{DisplayCommand, GlyphBitmap, PlacedGlyph, MAX_ATLAS_BYTES, MAX_GLYPH_SIDE};
use std::io::{self, Read, Write};

/// Largest frame either side will accept.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Largest coordinate we accept from a renderer (keeps the painter's math sane).
const MAX_COORD: f32 = 10_000_000.0;
/// Most display commands / placed glyphs a renderer may send for one page,
/// so a hostile renderer can't bog down the browser's painter.
const MAX_COMMANDS: usize = 500_000;
const MAX_GLYPHS: usize = 4_000_000;
/// Console lines a renderer may report per message.
const MAX_CONSOLE: usize = 200;
/// Headers in one script request or response.
const MAX_HEADERS: usize = 64;

/// Browser -> renderer.
#[derive(Debug, Clone, PartialEq)]
pub enum ToRenderer {
    Load {
        url: String,
        html: String,
        blocker_css: String,
        /// Run the page's JavaScript (you allowed it for this site).
        scripts: bool,
        /// Window size, for scripts.
        viewport: (f32, f32),
        /// `document.cookie` to start with (empty unless scripts run).
        cookies: String,
    },
    /// The answer to a script's request, and `document.cookie` after it.
    Response {
        result: Result<ScriptResponse, String>,
        cookies: String,
    },
    /// `document.cookie` after a script set a cookie.
    Cookies(String),
    FetchReply(Result<String, String>),
    /// You clicked the page at this page-space point.
    Click {
        x: f32,
        y: f32,
    },
    /// You typed this into the focused field.
    Type(String),
    /// You pressed a key with a field focused.
    Key(Key),
    Render {
        width: f32,
        height: f32,
    },
    Dom,
}

/// Renderer -> browser.
#[derive(Debug, Clone, PartialEq)]
pub enum FromRenderer {
    Fetch {
        url: String,
        dest: Dest,
    },
    Loaded {
        title: String,
        /// A navigation a script asked for (the browser applies its checks).
        navigate: Option<String>,
        console: Vec<String>,
    },
    Rendered(Rendered),
    Dom(String),
    /// A script's `fetch`/`XMLHttpRequest`.
    Request {
        url: String,
        req: ScriptRequest,
    },
    /// A script set `document.cookie`.
    SetCookie(String),
    Clicked {
        /// A script called `preventDefault()`: don't follow the link.
        prevented: bool,
        /// The page changed; render again.
        changed: bool,
        navigate: Option<String>,
        console: Vec<String>,
        /// A form was submitted (the browser checks it like any navigation).
        submit: Option<Submission>,
        /// A field that takes typing has the focus.
        focus: bool,
    },
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
    fn opt_str(&mut self, s: &Option<String>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    fn pairs(&mut self, v: &[(String, String)]) {
        self.u32(v.len() as u32);
        for (a, b) in v {
            self.str(a);
            self.str(b);
        }
    }
    fn strs(&mut self, v: &[String]) {
        self.u32(v.len() as u32);
        for s in v {
            self.str(s);
        }
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
    fn bool(&mut self) -> DecResult<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(format!("invalid boolean {v}")),
        }
    }
    fn opt_str(&mut self) -> DecResult<Option<String>> {
        Ok(if self.bool()? { Some(self.str()?) } else { None })
    }
    fn pairs(&mut self, max: usize) -> DecResult<Vec<(String, String)>> {
        let n = self.count(8)?;
        if n > max {
            return Err("too many headers".into());
        }
        (0..n).map(|_| Ok((self.str()?, self.str()?))).collect()
    }
    fn strs(&mut self, max: usize) -> DecResult<Vec<String>> {
        let n = self.count(4)?;
        if n > max {
            return Err("list too long".into());
        }
        (0..n).map(|_| self.str()).collect()
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

/// Height of a paint-budget band, in px.
const BAND: f32 = 256.0;
/// Each band may be painted over this many times its area...
const MAX_OVERDRAW: f32 = 32.0;
/// ...measured against a page this wide (generous: normal pages are narrower).
const BUDGET_WIDTH: f32 = 4096.0;

/// The caps above limit how much a renderer sends, not how expensive it is
/// to paint: 500k rectangles stacked on the first screen would freeze the
/// window on every repaint. So the page is cut into horizontal bands and the
/// painted area overlapping each band is capped. A window shows only a few
/// bands, so this bounds the cost of any single frame.
fn check_paint_budget(list: &[DisplayCommand], atlas: &[GlyphBitmap], height: f32) -> DecResult<()> {
    let bottom = height + 4096.0; // content may hang a little below the page
    let bands = (bottom / BAND).ceil() as usize + 1;
    let limit = MAX_OVERDRAW * BUDGET_WIDTH * BAND;
    let mut used = vec![0f32; bands];
    let mut add = |x: f32, y: f32, w: f32, h: f32| -> DecResult<()> {
        // Only the part that can ever be on screen costs anything.
        let x0 = x.max(0.0);
        let x1 = (x + w).min(16_384.0);
        let y0 = y.max(0.0);
        let y1 = (y + h).min(bottom);
        if x1 <= x0 || y1 <= y0 {
            return Ok(());
        }
        let width = x1 - x0;
        let first = (y0 / BAND) as usize;
        let last = ((y1 / BAND) as usize).min(bands - 1);
        for (b, slot) in used.iter_mut().enumerate().take(last + 1).skip(first) {
            let top = (b as f32 * BAND).max(y0);
            let bot = ((b + 1) as f32 * BAND).min(y1);
            *slot += width * (bot - top).max(0.0);
            if *slot > limit {
                return Err(format!("too much painting around y={}", b as f32 * BAND));
            }
        }
        Ok(())
    };
    for cmd in list {
        match cmd {
            DisplayCommand::Rect { rect, .. } => add(rect.x, rect.y, rect.w, rect.h)?,
            DisplayCommand::Glyphs { glyphs, .. } => {
                for g in glyphs {
                    let bmp = &atlas[g.id as usize];
                    add(g.x, g.y, bmp.w as f32, bmp.h as f32)?;
                }
            }
        }
    }
    Ok(())
}

impl ToRenderer {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        match self {
            ToRenderer::Load {
                url,
                html,
                blocker_css,
                scripts,
                viewport,
                cookies,
            } => {
                e.u8(1);
                e.str(url);
                e.str(html);
                e.str(blocker_css);
                e.u8(*scripts as u8);
                e.f32(viewport.0);
                e.f32(viewport.1);
                e.str(cookies);
            }
            ToRenderer::Response { result, cookies } => {
                e.u8(6);
                match result {
                    Ok(r) => {
                        e.u8(1);
                        e.u32(r.status as u32);
                        e.str(&r.url);
                        e.u8(r.redirected as u8);
                        e.pairs(&r.headers);
                        e.str(&r.body);
                    }
                    Err(err) => {
                        e.u8(0);
                        e.str(err);
                    }
                }
                e.str(cookies);
            }
            ToRenderer::Cookies(c) => {
                e.u8(7);
                e.str(c);
            }
            ToRenderer::Click { x, y } => {
                e.u8(5);
                e.f32(*x);
                e.f32(*y);
            }
            ToRenderer::Type(t) => {
                e.u8(8);
                e.str(t);
            }
            ToRenderer::Key(k) => {
                e.u8(9);
                e.u8(match k {
                    Key::Backspace => 1,
                    Key::Enter => 2,
                    Key::Tab => 3,
                    Key::Escape => 4,
                });
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
                scripts: d.bool()?,
                viewport: (d.f32()?, d.f32()?),
                cookies: d.str()?,
            },
            6 => ToRenderer::Response {
                result: if d.bool()? {
                    Ok(ScriptResponse {
                        status: d.u32()?.min(999) as u16,
                        url: d.str()?,
                        redirected: d.bool()?,
                        headers: d.pairs(256)?,
                        body: d.str()?,
                    })
                } else {
                    Err(d.str()?)
                },
                cookies: d.str()?,
            },
            7 => ToRenderer::Cookies(d.str()?),
            8 => ToRenderer::Type(d.str()?),
            9 => ToRenderer::Key(match d.u8()? {
                1 => Key::Backspace,
                2 => Key::Enter,
                3 => Key::Tab,
                4 => Key::Escape,
                k => return Err(format!("unknown key {k}")),
            }),
            5 => ToRenderer::Click {
                x: d.f32()?,
                y: d.f32()?,
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
            FromRenderer::Fetch { url, dest } => {
                e.u8(1);
                e.str(url);
                e.u8(match dest {
                    Dest::Script => 2,
                    _ => 1,
                });
            }
            FromRenderer::Loaded {
                title,
                navigate,
                console,
            } => {
                e.u8(2);
                e.str(title);
                e.opt_str(navigate);
                e.strs(console);
            }
            FromRenderer::Request { url, req } => {
                e.u8(6);
                e.str(url);
                e.str(&req.method);
                e.pairs(&req.headers);
                e.opt_str(&req.body);
                e.u8(match req.credentials {
                    Credentials::Omit => 0,
                    Credentials::SameOrigin => 1,
                    Credentials::Include => 2,
                });
            }
            FromRenderer::SetCookie(v) => {
                e.u8(7);
                e.str(v);
            }
            FromRenderer::Clicked {
                prevented,
                changed,
                navigate,
                console,
                submit,
                focus,
            } => {
                e.u8(5);
                e.u8(*prevented as u8);
                e.u8(*changed as u8);
                e.opt_str(navigate);
                e.strs(console);
                match submit {
                    Some(s) => {
                        e.u8(1);
                        e.str(&s.url);
                        e.u8(s.post as u8);
                        e.opt_str(&s.body);
                        e.opt_str(&s.content_type);
                    }
                    None => e.u8(0),
                }
                e.u8(*focus as u8);
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
                        DisplayCommand::Glyphs { color, glyphs } => {
                            e.u8(2);
                            e.color(*color);
                            e.u32(glyphs.len() as u32);
                            for g in glyphs {
                                e.f32(g.x);
                                e.f32(g.y);
                                e.u32(g.id);
                            }
                        }
                    }
                }
                e.u32(r.links.len() as u32);
                for (rect, href) in &r.links {
                    e.rect(*rect);
                    e.str(href);
                }
                e.u32(r.atlas.len() as u32);
                for g in &r.atlas {
                    e.0.extend_from_slice(&g.w.to_le_bytes());
                    e.0.extend_from_slice(&g.h.to_le_bytes());
                    e.0.extend_from_slice(&g.data);
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
            1 => FromRenderer::Fetch {
                url: d.str()?,
                // Only the kinds a page may ask for; everything else is a
                // protocol violation.
                dest: match d.u8()? {
                    1 => Dest::Stylesheet,
                    2 => Dest::Script,
                    t => return Err(format!("invalid request type {t}")),
                },
            },
            2 => FromRenderer::Loaded {
                title: d.str()?,
                navigate: d.opt_str()?,
                console: d.strs(MAX_CONSOLE)?,
            },
            6 => FromRenderer::Request {
                url: d.str()?,
                req: ScriptRequest {
                    method: d.str()?,
                    headers: d.pairs(MAX_HEADERS)?,
                    body: d.opt_str()?,
                    credentials: match d.u8()? {
                        0 => Credentials::Omit,
                        1 => Credentials::SameOrigin,
                        2 => Credentials::Include,
                        c => return Err(format!("invalid credentials mode {c}")),
                    },
                },
            },
            7 => FromRenderer::SetCookie(d.str()?),
            5 => FromRenderer::Clicked {
                prevented: d.bool()?,
                changed: d.bool()?,
                navigate: d.opt_str()?,
                console: d.strs(MAX_CONSOLE)?,
                submit: if d.bool()? {
                    Some(Submission {
                        url: d.str()?,
                        post: d.bool()?,
                        body: d.opt_str()?,
                        content_type: d.opt_str()?,
                    })
                } else {
                    None
                },
                focus: d.bool()?,
            },
            3 => {
                let height = d.f32()?.max(0.0);
                let background = d.color()?;
                let n = d.count(9)?;
                if n > MAX_COMMANDS {
                    return Err("too many display commands".into());
                }
                let mut glyph_total = 0usize;
                let mut list = Vec::with_capacity(n);
                for _ in 0..n {
                    list.push(match d.u8()? {
                        1 => DisplayCommand::Rect {
                            rect: d.rect()?,
                            color: d.color()?,
                        },
                        2 => {
                            let color = d.color()?;
                            let n = d.count(12)?;
                            glyph_total += n;
                            if glyph_total > MAX_GLYPHS {
                                return Err("too many glyphs".into());
                            }
                            let mut glyphs = Vec::with_capacity(n);
                            for _ in 0..n {
                                glyphs.push(PlacedGlyph {
                                    x: d.f32()?,
                                    y: d.f32()?,
                                    id: d.u32()?,
                                });
                            }
                            DisplayCommand::Glyphs { color, glyphs }
                        }
                        t => return Err(format!("unknown display command {t}")),
                    });
                }
                let n = d.count(20)?;
                let mut links = Vec::with_capacity(n);
                for _ in 0..n {
                    links.push((d.rect()?, d.str()?));
                }
                let n = d.count(4)?;
                let mut atlas = Vec::with_capacity(n);
                let mut atlas_bytes = 0usize;
                for _ in 0..n {
                    let w = u16::from_le_bytes(d.take(2)?.try_into().unwrap());
                    let h = u16::from_le_bytes(d.take(2)?.try_into().unwrap());
                    if w > MAX_GLYPH_SIDE || h > MAX_GLYPH_SIDE {
                        return Err(format!("glyph bitmap too large: {w}x{h}"));
                    }
                    let len = w as usize * h as usize;
                    atlas_bytes += len;
                    if atlas_bytes > MAX_ATLAS_BYTES * 2 {
                        return Err("glyph atlas too large".into());
                    }
                    atlas.push(GlyphBitmap {
                        w,
                        h,
                        data: d.take(len)?.to_vec(),
                    });
                }
                // Every placed glyph must refer to a bitmap that exists.
                for cmd in &list {
                    if let DisplayCommand::Glyphs { glyphs, .. } = cmd {
                        if glyphs.iter().any(|g| g.id as usize >= atlas.len()) {
                            return Err("glyph id out of range".into());
                        }
                    }
                }
                check_paint_budget(&list, &atlas, height)?;
                FromRenderer::Rendered(Rendered {
                    atlas,
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
                DisplayCommand::Glyphs {
                    color: Color::BLACK,
                    glyphs: vec![
                        PlacedGlyph {
                            x: 5.0,
                            y: 6.0,
                            id: 0,
                        },
                        PlacedGlyph {
                            x: 9.0,
                            y: 6.0,
                            id: 1,
                        },
                    ],
                },
            ],
            atlas: vec![
                GlyphBitmap {
                    w: 2,
                    h: 2,
                    data: vec![0, 128, 255, 64],
                },
                GlyphBitmap {
                    w: 1,
                    h: 3,
                    data: vec![1, 2, 3],
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
                dest: Dest::Stylesheet,
            },
            FromRenderer::Fetch {
                url: "https://a/b.js".into(),
                dest: Dest::Script,
            },
            FromRenderer::Loaded {
                title: "T".into(),
                navigate: Some("https://b/".into()),
                console: vec!["[log] hi".into()],
            },
            FromRenderer::Request {
                url: "https://a/api".into(),
                req: ScriptRequest {
                    method: "POST".into(),
                    headers: vec![("content-type".into(), "text/plain".into())],
                    body: Some("hi".into()),
                    credentials: Credentials::Include,
                },
            },
            FromRenderer::SetCookie("x=1".into()),
            FromRenderer::Clicked {
                prevented: true,
                changed: false,
                navigate: None,
                console: vec![],
                submit: None,
                focus: true,
            },
            FromRenderer::Clicked {
                prevented: false,
                changed: true,
                navigate: None,
                console: vec!["[log] x".into()],
                submit: Some(Submission {
                    url: "https://a/s".into(),
                    post: true,
                    body: Some("q=1".into()),
                    content_type: Some("application/x-www-form-urlencoded".into()),
                }),
                focus: false,
            },
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
                scripts: true,
                viewport: (1000.0, 700.0),
                cookies: "a=1".into(),
            },
            ToRenderer::Response {
                result: Ok(ScriptResponse {
                    status: 200,
                    url: "https://a/x".into(),
                    redirected: false,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: "{}".into(),
                }),
                cookies: String::new(),
            },
            ToRenderer::Response {
                result: Err("CORS".into()),
                cookies: "b=2".into(),
            },
            ToRenderer::Cookies("c=3".into()),
            ToRenderer::Click { x: 10.0, y: 20.0 },
            ToRenderer::Type("héllo".into()),
            ToRenderer::Key(Key::Enter),
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
        // A glyph id outside the atlas.
        let mut r = sample();
        r.list.push(DisplayCommand::Glyphs {
            color: Color::BLACK,
            glyphs: vec![PlacedGlyph {
                x: 0.0,
                y: 0.0,
                id: 7,
            }],
        });
        assert!(FromRenderer::decode(&FromRenderer::Rendered(r).encode())
            .unwrap_err()
            .contains("out of range"));
        // An oversized glyph bitmap header.
        let mut r = sample();
        r.atlas.push(GlyphBitmap {
            w: 5000,
            h: 1,
            data: vec![0; 5000],
        });
        assert!(FromRenderer::decode(&FromRenderer::Rendered(r).encode())
            .unwrap_err()
            .contains("too large"));
        // Valid but ruinous: thousands of window-sized rectangles stacked on
        // the first screen.
        let mut r = sample();
        r.list = (0..5000)
            .map(|_| DisplayCommand::Rect {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1200.0,
                    h: 900.0,
                },
                color: Color::BLACK,
            })
            .collect();
        assert!(FromRenderer::decode(&FromRenderer::Rendered(r).encode())
            .unwrap_err()
            .contains("too much painting"));
        // The same rectangles spread down a long page are fine.
        let mut r = sample();
        r.height = 5000.0 * 900.0;
        r.list = (0..5000)
            .map(|i| DisplayCommand::Rect {
                rect: Rect {
                    x: 0.0,
                    y: i as f32 * 900.0,
                    w: 1200.0,
                    h: 900.0,
                },
                color: Color::BLACK,
            })
            .collect();
        assert!(FromRenderer::decode(&FromRenderer::Rendered(r).encode()).is_ok());
        // A forged glyph count larger than the message.
        let mut e = Enc::default();
        e.u8(3);
        e.f32(1.0);
        e.color(Color::WHITE);
        e.u32(1);
        e.u8(2);
        e.color(Color::BLACK);
        e.u32(u32::MAX);
        assert!(FromRenderer::decode(&e.0).is_err());
        // Oversized frame header.
        let mut r: &[u8] = &(u32::MAX).to_le_bytes();
        assert!(read_frame(&mut r).is_err());
    }
}
