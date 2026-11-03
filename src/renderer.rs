//! The renderer process: parses, styles and lays out one page inside the
//! sandbox, and answers the browser over stdin/stdout. It has no network or
//! filesystem access of its own; stylesheets are requested from the browser,
//! which applies the security policy.

use crate::ipc::{read_frame, write_frame, FromRenderer, ToRenderer};
use crate::js::{self, ClickResult, Runtime};
use crate::net::Dest;
use crate::page::{Page, ScriptOutput};
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

pub fn serve(input: &mut impl Read, output: &mut impl Write) -> Result<(), String> {
    let mut page: Option<Page> = None;
    // The page's scripts, if it has any (only for sites you allowed).
    let mut rt: Option<Runtime> = None;
    while let Some(msg) = recv(input)? {
        match msg {
            ToRenderer::Load {
                url,
                html,
                blocker_css,
                scripts,
                viewport,
            } => {
                let mut fetch = |target: &str, dest: Dest| -> Result<String, String> {
                    send(
                        output,
                        FromRenderer::Fetch {
                            url: target.to_string(),
                            dest,
                        },
                    )?;
                    match recv(input)? {
                        Some(ToRenderer::FetchReply(r)) => r,
                        _ => Err("protocol error: expected a fetch reply".into()),
                    }
                };
                let (p, new_rt, out) = if scripts {
                    Page::load_with_scripts(&url, &html, &blocker_css, viewport, &mut fetch)
                } else {
                    (
                        Page::parse(&url, &html, &blocker_css, &mut fetch),
                        None,
                        ScriptOutput::default(),
                    )
                };
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
                    result = rt.click(uid, Instant::now() + js::EVENT_BUDGET);
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
            ToRenderer::FetchReply(_) => return Err("protocol error: unexpected fetch reply".into()),
        }
    }
    Ok(())
}
