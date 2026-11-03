//! Browser-process side of a page ("tab").
//!
//! The browser fetches documents and subresources and enforces the security
//! policy. Parsing, styling and layout of the untrusted page happen in a
//! separate, sandboxed renderer process (one per page), which hands back only
//! a validated display list. If the renderer crashes, hangs or sends garbage,
//! it is killed and an error page is shown; the browser keeps running.

use crate::ipc::{read_frame, write_frame, FromRenderer, ToRenderer};
use crate::net;
use crate::network::{Action, LogEntry, Nav, Network};
use crate::page::{error_page, Page, Rendered};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// How long a renderer may take to answer before it is killed.
const RENDERER_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Tab {
    pub url: String,
    /// The page whose link opened this one (`None` if the user typed it).
    /// Kept so reload/back/forward re-apply the same security checks.
    pub opener: Option<String>,
    pub title: String,
    /// Every request made (or refused) for this page.
    pub log: Vec<LogEntry>,
    /// Set when this is a browser-generated warning page; its
    /// `brickle:proceed` link performs this action.
    pub interstitial: Option<Action>,
    backend: Backend,
}

enum Backend {
    Sandboxed(Renderer),
    InProcess(Page),
    Failed(String),
}

impl Tab {
    /// Navigate to `url`. `from` is the page that linked here, if any.
    pub fn open(net: &mut Network, url: &str, from: Option<&str>, sandbox: bool) -> Tab {
        match net.navigate(url, from) {
            Nav::Page { url, html } => Tab::with_html(net, &url, from, &html, sandbox),
            Nav::Interstitial { url, html, action } => {
                // Our own trusted HTML: no renderer or subresources needed.
                let page = Page::trusted(&url, &html);
                Tab {
                    url,
                    opener: from.map(str::to_string),
                    title: page.title.clone(),
                    log: net.take_log(),
                    interstitial: Some(action),
                    backend: Backend::InProcess(page),
                }
            }
        }
    }

    /// Show `html` as the document at `url`.
    pub fn with_html(net: &mut Network, url: &str, from: Option<&str>, html: &str, sandbox: bool) -> Tab {
        let mut tab = Tab {
            url: url.to_string(),
            opener: from.map(str::to_string),
            title: String::new(),
            log: Vec::new(),
            interstitial: None,
            backend: Backend::Failed(String::new()),
        };
        let blocker_css = net.cosmetic_css(url);
        let page_url = url.to_string();
        {
            let mut fetch = |target: &str| net.subresource(&page_url, target);
            if !sandbox {
                let page = Page::parse(url, html, &blocker_css, &mut fetch);
                tab.title = page.title.clone();
                tab.backend = Backend::InProcess(page);
            } else {
                let result = Renderer::spawn().and_then(|mut r| {
                    r.send(ToRenderer::Load {
                        url: url.to_string(),
                        html: html.to_string(),
                        blocker_css,
                    })?;
                    loop {
                        match r.recv()? {
                            FromRenderer::Fetch { url } => r.send(ToRenderer::FetchReply(fetch(&url)))?,
                            FromRenderer::Loaded { title } => {
                                tab.title = title;
                                return Ok(r);
                            }
                            _ => return Err("renderer sent an unexpected message".into()),
                        }
                    }
                });
                tab.backend = match result {
                    Ok(r) => Backend::Sandboxed(r),
                    Err(e) => Backend::Failed(e),
                };
            }
        }
        tab.log = net.take_log();
        tab
    }

    pub fn render(&mut self, width: f32, height: f32) -> Rendered {
        let result = match &mut self.backend {
            Backend::InProcess(p) => return p.render(width, height),
            Backend::Failed(e) => Err(e.clone()),
            Backend::Sandboxed(r) => {
                r.send(ToRenderer::Render { width, height })
                    .and_then(|_| match r.recv()? {
                        FromRenderer::Rendered(out) => Ok(out),
                        _ => Err("renderer sent an unexpected message".into()),
                    })
            }
        };
        result.unwrap_or_else(|e| {
            self.backend = Backend::Failed(e.clone());
            self.title = "Renderer error".into();
            crashed_page(&self.url, &e).render(width, height)
        })
    }

    pub fn dom(&mut self) -> Result<String, String> {
        match &mut self.backend {
            Backend::InProcess(p) => {
                let mut s = String::new();
                p.dom.dump(0, &mut s);
                Ok(s)
            }
            Backend::Failed(e) => Err(e.clone()),
            Backend::Sandboxed(r) => {
                r.send(ToRenderer::Dom)?;
                match r.recv()? {
                    FromRenderer::Dom(s) => Ok(s),
                    _ => Err("renderer sent an unexpected message".into()),
                }
            }
        }
    }

    pub fn link_url(&self, href: &str) -> String {
        net::resolve(&self.url, href)
    }
}

fn crashed_page(url: &str, err: &str) -> Page {
    let msg = format!(
        "The page's renderer process failed: {err}. It ran in a sandbox, so the rest of the browser \
         is unaffected. Reload to try again, or run Brickle with --no-sandbox to debug."
    );
    Page::trusted(url, &error_page(url, &msg))
}

struct Renderer {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Result<Vec<u8>, String>>,
}

impl Renderer {
    fn spawn() -> Result<Renderer, String> {
        let exe = std::env::current_exe().map_err(|e| format!("can't find own executable: {e}"))?;
        let mut child = Command::new(exe)
            .arg("--renderer")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("can't start renderer: {e}"))?;
        let stdin = child.stdin.take().ok_or("no renderer stdin")?;
        let mut stdout = child.stdout.take().ok_or("no renderer stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || loop {
            let msg = match read_frame(&mut stdout) {
                Ok(Some(frame)) => Ok(frame),
                Ok(None) => Err("renderer exited".to_string()),
                Err(e) => Err(format!("renderer pipe: {e}")),
            };
            let stop = msg.is_err();
            if tx.send(msg).is_err() || stop {
                break;
            }
        });
        Ok(Renderer { child, stdin, rx })
    }

    fn send(&mut self, msg: ToRenderer) -> Result<(), String> {
        write_frame(&mut self.stdin, &msg.encode()).map_err(|e| format!("renderer pipe: {e}"))
    }

    fn recv(&mut self) -> Result<FromRenderer, String> {
        let frame = match self.rx.recv_timeout(RENDERER_TIMEOUT) {
            Ok(r) => r?,
            Err(_) => {
                let _ = self.child.kill();
                return Err("renderer stopped responding and was killed".into());
            }
        };
        FromRenderer::decode(&frame).map_err(|e| {
            let _ = self.child.kill();
            format!("renderer sent a malformed message ({e}) and was killed")
        })
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
