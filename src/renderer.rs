//! The renderer process: parses, styles and lays out one page inside the
//! sandbox, and answers the browser over stdin/stdout. It has no network or
//! filesystem access of its own; stylesheets are requested from the browser,
//! which applies the security policy.

use crate::ipc::{read_frame, write_frame, FromRenderer, ToRenderer};
use crate::page::Page;
use std::io::{self, Read, Write};

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
    while let Some(msg) = recv(input)? {
        match msg {
            ToRenderer::Load { url, html } => {
                let mut fetch = |target: &str| -> Result<String, String> {
                    send(
                        output,
                        FromRenderer::Fetch {
                            url: target.to_string(),
                        },
                    )?;
                    match recv(input)? {
                        Some(ToRenderer::FetchReply(r)) => r,
                        _ => Err("protocol error: expected a fetch reply".into()),
                    }
                };
                let p = Page::parse(&url, &html, &mut fetch);
                send(
                    output,
                    FromRenderer::Loaded {
                        title: p.title.clone(),
                    },
                )?;
                page = Some(p);
            }
            ToRenderer::Render { width, height } => {
                let p = page.as_ref().ok_or("render before load")?;
                let width = width.clamp(1.0, 16_000.0);
                send(
                    output,
                    FromRenderer::Rendered(p.render(width, height.clamp(1.0, 16_000.0))),
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
