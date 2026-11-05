//! The renderer process: parses, styles and lays out one page inside the
//! sandbox, and answers the browser over stdin/stdout. It has no network or
//! filesystem access of its own; stylesheets are requested from the browser,
//! which applies the security policy.

use crate::ipc::{read_frame, write_frame, FromRenderer, ToRenderer};
use crate::js::{self, Runtime};
use crate::net::{Dest, ScriptRequest};
use crate::network::ScriptResponse;
use crate::page::{pump, Host, Page, ScriptOutput};
use std::io::{self, Read, Write};
use std::time::Instant;

pub fn run() -> Result<(), String> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    serve(&mut input, &mut output)
}

fn send(out: &mut impl Write, msg: FromRenderer) -> Result<(), String> {
    write_frame(out, &msg.encode()).map_err(|e| format!("renderer: write: {e}"))
}

fn recv(input: &mut impl Read) -> Result<Option<ToRenderer>, String> {
    match read_frame(input).map_err(|e| format!("renderer: read: {e}"))? {
        Some(frame) => ToRenderer::decode(&frame).map(Some),
        None => Ok(None),
    }
}

/// The browser, as the renderer sees it: every request goes over the pipe
/// and waits for its answer (the protocol is strictly request/response).
struct IpcHost<'a, R: Read, W: Write> {
    input: &'a mut R,
    output: &'a mut W,
    cookies: String,
    /// A protocol error: once set, everything fails and `serve` reports it.
    broken: Option<String>,
}

impl<R: Read, W: Write> IpcHost<'_, R, W> {
    fn ask(&mut self, msg: FromRenderer) -> Result<ToRenderer, String> {
        if let Some(e) = &self.broken {
            return Err(e.clone());
        }
        let r = send(self.output, msg)
            .and_then(|_| recv(self.input)?.ok_or_else(|| "browser went away".to_string()));
        if let Err(e) = &r {
            self.broken = Some(e.clone());
        }
        r
    }

    fn file(&mut self, url: &str, dest: Dest, added: bool, csp: &[Vec<String>]) -> Result<String, String> {
        match self.ask(FromRenderer::Fetch {
            url: url.to_string(),
            dest,
            added,
            csp: csp.to_vec(),
        })? {
            ToRenderer::FetchReply(r) => r,
            _ => {
                self.broken = Some("protocol error: expected a fetch reply".into());
                Err("protocol error".into())
            }
        }
    }

    fn picture(&mut self, url: &str, added: bool) -> Result<Vec<u8>, String> {
        match self.ask(FromRenderer::Fetch {
            url: url.to_string(),
            dest: Dest::Image,
            added,
            csp: vec![],
        })? {
            ToRenderer::ImageReply(r) => r,
            _ => {
                self.broken = Some("protocol error: expected an image".into());
                Err("protocol error".into())
            }
        }
    }
}

impl<R: Read, W: Write> Host for IpcHost<'_, R, W> {
    fn fetch(&mut self, url: &str, dest: Dest) -> Result<String, String> {
        self.file(url, dest, false, &[])
    }

    fn fetch_added(&mut self, url: &str, dest: Dest) -> Result<String, String> {
        self.file(url, dest, true, &[])
    }

    fn fetch_script(&mut self, url: &str, added: bool, csp: &[Vec<String>]) -> Result<String, String> {
        self.file(url, Dest::Script, added, csp)
    }

    fn image(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.picture(url, false)
    }

    fn image_added(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.picture(url, true)
    }

    fn request(&mut self, url: &str, req: ScriptRequest) -> Result<ScriptResponse, String> {
        match self.ask(FromRenderer::Request {
            url: url.to_string(),
            req,
        })? {
            ToRenderer::Response { result, cookies } => {
                self.cookies = cookies;
                result
            }
            _ => {
                self.broken = Some("protocol error: expected a response".into());
                Err("protocol error".into())
            }
        }
    }

    fn set_cookie(&mut self, value: &str) {
        match self.ask(FromRenderer::SetCookie(value.to_string())) {
            Ok(ToRenderer::Cookies(c)) => self.cookies = c,
            Ok(_) => self.broken = Some("protocol error: expected cookies".into()),
            Err(_) => {}
        }
    }

    fn cookies(&mut self) -> String {
        self.cookies.clone()
    }
}

pub fn serve(input: &mut impl Read, output: &mut impl Write) -> Result<(), String> {
    let mut page: Option<Page> = None;
    // The page's scripts, if it has any (only for sites you allowed).
    let mut rt: Option<Runtime> = None;
    let mut cookies = String::new();
    while let Some(msg) = recv(input)? {
        match msg {
            ToRenderer::Load {
                url,
                html,
                blocker_css,
                scripts,
                viewport,
                cookies: start_cookies,
                csp,
            } => {
                cookies = start_cookies;
                let mut host = IpcHost {
                    input: &mut *input,
                    output: &mut *output,
                    cookies: cookies.clone(),
                    broken: None,
                };
                let (mut p, new_rt, out) = if scripts {
                    Page::load_with_scripts(&url, &html, &blocker_css, viewport, &csp, &mut host)
                } else {
                    let p = Page::parse(&url, &html, &blocker_css, &mut |u, d| host.fetch(u, d));
                    (p, None, ScriptOutput::default())
                };
                p.load_images(&mut |u, added| {
                    if added {
                        host.image_added(u)
                    } else {
                        host.image(u)
                    }
                });
                if let Some(e) = host.broken {
                    return Err(e);
                }
                cookies = host.cookies;
                send(
                    output,
                    FromRenderer::Loaded {
                        title: p.title.clone(),
                        navigate: out.navigate.or_else(|| p.redirect.clone()),
                        console: out.console,
                    },
                )?;
                page = Some(p);
                rt = new_rt;
            }
            ToRenderer::Render {
                width,
                height,
                text_scale,
                title_px,
            } => {
                let p = page.as_mut().ok_or("render before load")?;
                p.text_scale = text_scale;
                p.title_px = if title_px.is_finite() {
                    title_px.clamp(0.0, 64.0)
                } else {
                    0.0
                };
                let width = width.clamp(1.0, 16_000.0);
                let rendered = p.render(width, height.clamp(1.0, 16_000.0));
                if let Some(rt) = rt.as_mut() {
                    rt.set_boxes(p.boxes());
                }
                send(output, FromRenderer::Rendered(rendered))?;
            }
            msg @ (ToRenderer::Click { .. } | ToRenderer::Type(_) | ToRenderer::Key(_)) => {
                let p = page.as_mut().ok_or("input before load")?;
                let until = Instant::now() + js::EVENT_BUDGET;
                let before = rt.as_ref().map(|r| r.version());
                let mut done = match msg {
                    ToRenderer::Click { x, y } => p.click_at(rt.as_mut(), x, y, until),
                    ToRenderer::Type(t) => p.type_text(rt.as_mut(), &t),
                    ToRenderer::Key(k) => p.key(rt.as_mut(), k),
                    _ => unreachable!(),
                };
                let (mut navigate, mut console) = (None, vec![]);
                if let Some(rt) = rt.as_mut() {
                    // Handlers may have made requests or set timers.
                    let mut host = IpcHost {
                        input: &mut *input,
                        output: &mut *output,
                        cookies: cookies.clone(),
                        broken: None,
                    };
                    pump(rt, &mut host, until);
                    if Some(rt.version()) != before {
                        p.refresh(rt);
                        done.changed = true;
                        // Scripts may have added images.
                        p.load_images(&mut |u, added| {
                            if added {
                                host.image_added(u)
                            } else {
                                host.image(u)
                            }
                        });
                    }
                    if let Some(e) = host.broken {
                        return Err(e);
                    }
                    cookies = host.cookies;
                    navigate = rt.take_navigate();
                    console = rt.take_console();
                }
                send(
                    output,
                    FromRenderer::Clicked {
                        prevented: done.prevented,
                        changed: done.changed,
                        navigate,
                        console,
                        submit: done.submit,
                        focus: done.focus,
                        copied: done.copied,
                    },
                )?;
            }
            ToRenderer::Dom => {
                let mut s = String::new();
                if let Some(p) = &page {
                    p.dom.dump(0, &mut s);
                }
                send(output, FromRenderer::Dom(s))?;
            }
            ToRenderer::FetchReply(_)
            | ToRenderer::ImageReply(_)
            | ToRenderer::Response { .. }
            | ToRenderer::Cookies(_) => return Err("protocol error: unexpected reply".into()),
        }
    }
    Ok(())
}
