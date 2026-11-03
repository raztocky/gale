//! The renderer process: parses, styles and lays out one page inside the
//! sandbox, and answers the browser over stdin/stdout. It has no network or
//! filesystem access of its own; stylesheets are requested from the browser,
//! which applies the security policy.

use crate::ipc::{read_frame, write_frame, FromRenderer, ToRenderer};
use crate::js::{self, ClickResult, Runtime};
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
}

impl<R: Read, W: Write> Host for IpcHost<'_, R, W> {
    fn fetch(&mut self, url: &str, dest: Dest) -> Result<String, String> {
        match self.ask(FromRenderer::Fetch {
            url: url.to_string(),
            dest,
        })? {
            ToRenderer::FetchReply(r) => r,
            _ => {
                self.broken = Some("protocol error: expected a fetch reply".into());
                Err("protocol error".into())
            }
        }
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
            } => {
                cookies = start_cookies;
                let mut host = IpcHost {
                    input: &mut *input,
                    output: &mut *output,
                    cookies: cookies.clone(),
                    broken: None,
                };
                let (p, new_rt, out) = if scripts {
                    Page::load_with_scripts(&url, &html, &blocker_css, viewport, &mut host)
                } else {
                    let p = Page::parse(&url, &html, &blocker_css, &mut |u, d| host.fetch(u, d));
                    (p, None, ScriptOutput::default())
                };
                if let Some(e) = host.broken {
                    return Err(e);
                }
                cookies = host.cookies;
                send(
                    output,
                    FromRenderer::Loaded {
                        title: p.title.clone(),
                        navigate: out.navigate,
                        console: out.console,
                    },
                )?;
                page = Some(p);
                rt = new_rt;
            }
            ToRenderer::Render { width, height } => {
                let p = page.as_ref().ok_or("render before load")?;
                let width = width.clamp(1.0, 16_000.0);
                let rendered = p.render(width, height.clamp(1.0, 16_000.0));
                if let Some(rt) = rt.as_mut() {
                    rt.set_boxes(p.boxes());
                }
                send(output, FromRenderer::Rendered(rendered))?;
            }
            ToRenderer::Click { x, y } => {
                let p = page.as_mut().ok_or("click before load")?;
                let mut result = ClickResult::default();
                let (mut navigate, mut console) = (None, vec![]);
                if let (Some(rt), Some(uid)) = (rt.as_mut(), p.hit(x, y)) {
                    let until = Instant::now() + js::EVENT_BUDGET;
                    let before = rt.version();
                    result = rt.click(uid, until);
                    let mut host = IpcHost {
                        input: &mut *input,
                        output: &mut *output,
                        cookies: cookies.clone(),
                        broken: None,
                    };
                    pump(rt, &mut host, until);
                    if let Some(e) = host.broken {
                        return Err(e);
                    }
                    cookies = host.cookies;
                    result.changed = rt.version() != before;
                    if result.changed {
                        p.refresh(rt);
                    }
                    navigate = rt.take_navigate();
                    console = rt.take_console();
                }
                send(
                    output,
                    FromRenderer::Clicked {
                        prevented: result.prevented,
                        changed: result.changed,
                        navigate,
                        console,
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
            ToRenderer::FetchReply(_) | ToRenderer::Response { .. } | ToRenderer::Cookies(_) => {
                return Err("protocol error: unexpected reply".into())
            }
        }
    }
    Ok(())
}
