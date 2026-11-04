//! Browser-process side of a page ("tab").
//!
//! The browser fetches documents and subresources and enforces the security
//! policy. Parsing, styling and layout of the untrusted page happen in a
//! separate, sandboxed renderer process (one per page), which hands back only
//! a validated display list. If the renderer crashes, hangs or sends garbage,
//! it is killed and an error page is shown; the browser keeps running.

use crate::forms::Submission;
use crate::ipc::{read_frame, write_frame, FromRenderer, ToRenderer, MAX_FRAME};
use crate::net::{self, Cancel};
use crate::net::{Dest, ScriptRequest};
use crate::network::ScriptResponse;
use crate::network::{Action, LogEntry, Nav, Network, MAX_STYLESHEETS};
use crate::page::{error_page, Page, Rendered};
use crate::page::{Host, Key};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

/// How long a renderer may take to lay out and paint a page.
const RENDERER_TIMEOUT: Duration = Duration::from_secs(20);
/// Time for parsing and styling on top of the page's network deadline.
const LOAD_EXTRA: Duration = Duration::from_secs(10);
/// Most subresource requests a renderer may make for one page. The network
/// service refuses anything past its own budget; a renderer that keeps
/// asking anyway is misbehaving.
const MAX_FETCHES: usize = MAX_STYLESHEETS + crate::js::MAX_SCRIPTS + crate::network::MAX_IMAGES + 8;
/// Longest a page with JavaScript may take to load before Brickle gives up
/// on its scripts and shows it without them.
const SCRIPT_LOAD_LIMIT: Duration = Duration::from_secs(15);
/// Bytes a renderer running scripts may send per exchange (script
/// requests carry bodies).
const SCRIPT_BYTES: usize = 16 * 1024 * 1024;
/// Messages a renderer may send per exchange.
const MAX_MESSAGES: usize = 2000;

/// The browser's [`Host`]: every request goes through the network service,
/// for the page the browser itself recorded (`page`).
struct NetHost<'a> {
    net: &'a mut Network,
    page: String,
}

impl Host for NetHost<'_> {
    fn fetch(&mut self, url: &str, dest: Dest) -> Result<String, String> {
        self.net.subresource(&self.page, url, dest)
    }
    fn image(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.net.image(&self.page, url)
    }
    fn request(&mut self, url: &str, req: ScriptRequest) -> Result<ScriptResponse, String> {
        self.net.script_request(&self.page, url, req)
    }
    fn set_cookie(&mut self, value: &str) {
        self.net.set_script_cookie(&self.page, value);
    }
    fn cookies(&mut self) -> String {
        self.net.script_cookies(&self.page)
    }
}

/// Console lines kept per tab.
const MAX_CONSOLE: usize = 1000;
/// Default window size for scripts when there's no window (command line).
pub const DEFAULT_VIEWPORT: (f32, f32) = (1024.0, 768.0);
/// Total bytes a renderer may send while a page loads (subresource
/// requests and the title): a few kilobytes is normal.
const LOAD_BYTES: usize = 1024 * 1024;

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
    /// The page's JavaScript runs (you allowed it for this site).
    pub scripts: bool,
    /// What the page's scripts wrote to the console.
    pub console: Vec<String>,
    /// A navigation the page's scripts asked for while loading.
    pub script_navigate: Option<String>,
    /// Your text size, used when rendering.
    pub text_scale: f32,
    backend: Backend,
}

/// What a click on a page did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Click {
    /// A script called `preventDefault()`: don't follow the link.
    pub prevented: bool,
    /// The page changed and needs rendering again.
    pub changed: bool,
    /// A script asked to go somewhere.
    pub navigate: Option<String>,
    /// A form was submitted: go here (the browser checks it like any
    /// navigation).
    pub submit: Option<Submission>,
    /// A field that takes typing has the focus.
    pub focus: bool,
}

/// Something you did to a page.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A click at a page-space point.
    Click(f32, f32),
    Type(String),
    Key(Key),
}

/// What the renderer reported when a page loaded.
struct Loaded {
    title: String,
    navigate: Option<String>,
    console: Vec<String>,
}

enum Backend {
    Sandboxed(Renderer),
    InProcess(Box<Page>),
    Failed(String),
}

impl Tab {
    /// Navigate to `url`. `from` is the page that linked here, if any.
    pub fn open(net: &mut Network, url: &str, from: Option<&str>, sandbox: bool) -> Tab {
        Tab::open_sized(net, url, from, sandbox, DEFAULT_VIEWPORT)
    }

    /// [`Tab::open`] for a window of this size.
    pub fn open_sized(
        net: &mut Network,
        url: &str,
        from: Option<&str>,
        sandbox: bool,
        viewport: (f32, f32),
    ) -> Tab {
        let nav = net.navigate(url, from);
        Tab::from_nav(net, nav, from, sandbox, viewport)
    }

    /// Submit a form from the page at `from` (the browser's own record of
    /// it) and open what comes back.
    pub fn submit(
        net: &mut Network,
        sub: &Submission,
        from: Option<&str>,
        sandbox: bool,
        viewport: (f32, f32),
    ) -> Tab {
        let nav = net.submit_form(sub, from);
        Tab::from_nav(net, nav, from, sandbox, viewport)
    }

    fn from_nav(net: &mut Network, nav: Nav, from: Option<&str>, sandbox: bool, viewport: (f32, f32)) -> Tab {
        match nav {
            Nav::Page { url, html } => Tab::with_html_sized(net, &url, from, &html, sandbox, viewport),
            Nav::Interstitial { url, html, action } => {
                // Our own trusted HTML: no renderer or subresources needed.
                let page = Page::trusted(&url, &html);
                Tab {
                    url,
                    opener: from.map(str::to_string),
                    title: page.title.clone(),
                    log: net.take_log(),
                    interstitial: Some(action),
                    scripts: false,
                    console: vec![],
                    script_navigate: None,
                    backend: Backend::InProcess(Box::new(page)),
                    text_scale: 1.0,
                }
            }
        }
    }

    /// Show `html` as the document at `url`.
    pub fn with_html(net: &mut Network, url: &str, from: Option<&str>, html: &str, sandbox: bool) -> Tab {
        Tab::with_html_sized(net, url, from, html, sandbox, DEFAULT_VIEWPORT)
    }

    pub fn with_html_sized(
        net: &mut Network,
        url: &str,
        from: Option<&str>,
        html: &str,
        sandbox: bool,
        viewport: (f32, f32),
    ) -> Tab {
        // JavaScript only in the sandboxed renderer, and only where you
        // allowed it.
        let scripts = sandbox && net.scripts_allowed(url);
        let mut tab = Tab {
            url: url.to_string(),
            opener: from.map(str::to_string),
            title: String::new(),
            log: Vec::new(),
            interstitial: None,
            scripts,
            console: vec![],
            script_navigate: None,
            backend: Backend::Failed(String::new()),
            text_scale: 1.0,
        };
        let blocker_css = net.cosmetic_css(url);
        if !sandbox {
            let page_url = url.to_string();
            let mut fetch = |target: &str, dest: Dest| net.subresource(&page_url, target, dest);
            let mut page = Page::parse(url, html, &blocker_css, &mut fetch);
            page.load_images(&mut |u| net.image(&page_url, u));
            tab.title = page.title.clone();
            tab.backend = Backend::InProcess(Box::new(page));
            tab.log = net.take_log();
            return tab;
        }
        let mut result = load_sandboxed(net, url, html, &blocker_css, scripts, viewport);
        if scripts && result.is_err() && !net.cancel.is_cancelled() {
            // Scripts hung or crashed the renderer: show the page without
            // them rather than not at all.
            let why = result.as_ref().err().cloned().unwrap_or_default();
            net.restart_page_budget();
            result = load_sandboxed(net, url, html, &blocker_css, false, viewport);
            tab.scripts = false;
            tab.console.push(format!(
                "[brickle] JavaScript failed ({why}); showing the page without it"
            ));
        }
        match result {
            Ok((r, loaded)) => {
                tab.title = loaded.title;
                tab.script_navigate = loaded.navigate;
                tab.console.extend(loaded.console);
                tab.backend = Backend::Sandboxed(r);
            }
            Err(e) => tab.backend = Backend::Failed(e),
        }
        tab.log = net.take_log();
        tab
    }

    /// Tell the page you clicked at page-space point (x, y), so its scripts
    /// and form controls can react. The caller follows the link under the
    /// click (if any) unless a script prevented it.
    pub fn click(&mut self, net: &mut Network, x: f32, y: f32) -> Click {
        self.interact(net, Input::Click(x, y))
    }

    /// A click, typing or a key press on the page.
    pub fn interact(&mut self, net: &mut Network, input: Input) -> Click {
        let r = match &mut self.backend {
            Backend::Failed(_) => return Click::default(),
            Backend::InProcess(page) => {
                let until = Instant::now() + crate::js::EVENT_BUDGET;
                let done = match input {
                    Input::Click(x, y) => page.click_at(None, x, y, until),
                    Input::Type(t) => page.type_text(None, &t),
                    Input::Key(k) => page.key(None, k),
                };
                return Click {
                    prevented: done.prevented,
                    changed: done.changed,
                    navigate: None,
                    submit: done.submit,
                    focus: done.focus,
                };
            }
            Backend::Sandboxed(r) => r,
        };
        let mut host = NetHost {
            net,
            page: self.url.clone(),
        };
        let msg = match input {
            Input::Click(x, y) => ToRenderer::Click { x, y },
            Input::Type(t) => ToRenderer::Type(t),
            Input::Key(k) => ToRenderer::Key(k),
        };
        let result = r.send(msg).and_then(|_| {
            match r.serve(Instant::now() + RENDERER_TIMEOUT, SCRIPT_BYTES, &mut host)? {
                FromRenderer::Clicked {
                    prevented,
                    changed,
                    navigate,
                    console,
                    submit,
                    focus,
                } => Ok((
                    Click {
                        prevented,
                        changed,
                        navigate,
                        submit,
                        focus,
                    },
                    console,
                )),
                _ => Err(r.kill("renderer sent an unexpected message and was killed")),
            }
        });
        match result {
            Ok((click, console)) => {
                let room = MAX_CONSOLE.saturating_sub(self.console.len());
                self.console.extend(console.into_iter().take(room));
                click
            }
            Err(e) => {
                self.backend = Backend::Failed(e);
                Click {
                    changed: true,
                    ..Click::default()
                }
            }
        }
    }

    pub fn render(&mut self, width: f32, height: f32) -> Rendered {
        let result = match &mut self.backend {
            Backend::InProcess(p) => {
                p.text_scale = self.text_scale;
                return p.render(width, height);
            }
            Backend::Failed(e) => Err(e.clone()),
            Backend::Sandboxed(r) => r
                .send(ToRenderer::Render {
                    width,
                    height,
                    text_scale: self.text_scale,
                })
                .and_then(
                    |_| match r.recv(Instant::now() + RENDERER_TIMEOUT, &mut { MAX_FRAME })? {
                        FromRenderer::Rendered(out) => Ok(out),
                        _ => Err(r.kill("renderer sent an unexpected message and was killed")),
                    },
                ),
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
                match r.recv(Instant::now() + RENDERER_TIMEOUT, &mut { MAX_FRAME })? {
                    FromRenderer::Dom(s) => Ok(s),
                    _ => Err(r.kill("renderer sent an unexpected message and was killed")),
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

/// Load a page in a fresh sandboxed renderer.
fn load_sandboxed(
    net: &mut Network,
    url: &str,
    html: &str,
    blocker_css: &str,
    scripts: bool,
    viewport: (f32, f32),
) -> Result<(Renderer, Loaded), String> {
    let mut deadline = net.deadline().max(Instant::now()) + LOAD_EXTRA;
    if scripts {
        // Don't keep you waiting on scripts: past this, the page is shown
        // without them.
        deadline = deadline.min(Instant::now() + SCRIPT_LOAD_LIMIT);
    }
    let mut r = Renderer::spawn(net.cancel.clone())?;
    let mut host = NetHost {
        net,
        page: url.to_string(),
    };
    let loaded = r.load(
        url,
        html,
        blocker_css.to_string(),
        scripts,
        viewport,
        deadline,
        &mut host,
    )?;
    Ok((r, loaded))
}

/// The browser's end of a renderer process.
///
/// The protocol is strictly request/response, and everything the renderer
/// sends is treated as hostile:
/// - frames are read on a separate thread into a channel that holds none
///   (a rendezvous), so a renderer can't queue up messages in our memory;
/// - a message that arrives when none was asked for gets it killed;
/// - each exchange has a deadline and a byte budget;
/// - writes happen on their own thread, so a renderer that stops reading
///   can't block the browser either;
/// - Stop (the [`Cancel`]) kills it immediately.
struct Renderer {
    child: Child,
    to_child: Option<SyncSender<Vec<u8>>>,
    rx: Receiver<Result<Vec<u8>, String>>,
    cancel: Cancel,
}

impl Renderer {
    fn spawn(cancel: Cancel) -> Result<Renderer, String> {
        let exe = std::env::current_exe().map_err(|e| format!("can't find own executable: {e}"))?;
        let mut cmd = Command::new(exe);
        cmd.arg("--renderer");
        Renderer::spawn_command(cmd, cancel)
    }

    /// Start `cmd` as a renderer (tests use stand-in programs).
    fn spawn_command(mut cmd: Command, cancel: Cancel) -> Result<Renderer, String> {
        let mut child = cmd
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("can't start renderer: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no renderer stdin")?;
        let mut stdout = child.stdout.take().ok_or("no renderer stdout")?;
        let (tx, rx) = mpsc::sync_channel(0);
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
        let (to_child, frames) = mpsc::sync_channel::<Vec<u8>>(1);
        std::thread::spawn(move || {
            for frame in frames {
                if write_frame(&mut stdin, &frame).is_err() {
                    break;
                }
            }
        });
        Ok(Renderer {
            child,
            to_child: Some(to_child),
            rx,
            cancel,
        })
    }

    /// Hand the renderer a document and serve its requests until it
    /// reports the page loaded.
    #[allow(clippy::too_many_arguments)]
    fn load(
        &mut self,
        url: &str,
        html: &str,
        blocker_css: String,
        scripts: bool,
        viewport: (f32, f32),
        deadline: Instant,
        host: &mut dyn Host,
    ) -> Result<Loaded, String> {
        let cookies = if scripts { host.cookies() } else { String::new() };
        self.send(ToRenderer::Load {
            url: url.to_string(),
            html: html.to_string(),
            blocker_css,
            scripts,
            viewport,
            cookies,
        })?;
        let budget = if scripts { SCRIPT_BYTES } else { LOAD_BYTES };
        match self.serve(deadline, budget, host)? {
            FromRenderer::Loaded {
                title,
                navigate,
                console,
            } => Ok(Loaded {
                title,
                navigate,
                console,
            }),
            _ => Err(self.kill("renderer sent an unexpected message and was killed")),
        }
    }

    /// Answer the renderer's requests (files, script requests, cookies)
    /// through `host` until it sends anything else, which is returned.
    fn serve(
        &mut self,
        deadline: Instant,
        budget: usize,
        host: &mut dyn Host,
    ) -> Result<FromRenderer, String> {
        let mut budget = budget;
        let (mut fetches, mut messages) = (0, 0);
        loop {
            messages += 1;
            if messages > MAX_MESSAGES {
                return Err(self.kill("renderer sent too many messages and was killed"));
            }
            match self.recv(deadline, &mut budget)? {
                FromRenderer::Fetch {
                    url,
                    dest: Dest::Image,
                } if fetches < MAX_FETCHES => {
                    fetches += 1;
                    self.send(ToRenderer::ImageReply(host.image(&url)))?
                }
                FromRenderer::Fetch { url, dest } if fetches < MAX_FETCHES => {
                    fetches += 1;
                    self.send(ToRenderer::FetchReply(host.fetch(&url, dest)))?
                }
                FromRenderer::Fetch { .. } => {
                    return Err(self.kill("renderer made too many requests and was killed"))
                }
                FromRenderer::Request { url, req } => {
                    let result = host.request(&url, req);
                    let cookies = host.cookies();
                    self.send(ToRenderer::Response { result, cookies })?
                }
                FromRenderer::SetCookie(v) => {
                    host.set_cookie(&v);
                    let cookies = host.cookies();
                    self.send(ToRenderer::Cookies(cookies))?
                }
                other => return Ok(other),
            }
        }
    }

    /// Kill the renderer and return `why` as the error.
    fn kill(&mut self, why: &str) -> String {
        let _ = self.child.kill();
        self.to_child = None;
        why.to_string()
    }

    fn send(&mut self, msg: ToRenderer) -> Result<(), String> {
        // Already killed: say so, whatever the reader thread saw since.
        if self.to_child.is_none() {
            return Err("renderer was stopped".into());
        }
        match self.rx.try_recv() {
            Ok(Ok(_)) => return Err(self.kill("renderer sent a message nobody asked for and was killed")),
            Ok(Err(e)) => return Err(self.kill(&e)),
            Err(_) => {}
        }
        let frame = msg.encode();
        if frame.len() > MAX_FRAME {
            return Err(self.kill("message to renderer too large"));
        }
        // Never block: if the previous message is still waiting to be
        // written, the renderer has stopped reading.
        let sent = match &self.to_child {
            Some(tx) => tx.try_send(frame),
            None => return Err("renderer was stopped".into()),
        };
        match sent {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                Err(self.kill("renderer stopped reading its input and was killed"))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err(self.kill("renderer pipe closed")),
        }
    }

    /// Wait for the next message, until `deadline`. Each frame is charged to
    /// `budget`.
    fn recv(&mut self, deadline: Instant, budget: &mut usize) -> Result<FromRenderer, String> {
        let frame = loop {
            if self.cancel.is_cancelled() {
                return Err(self.kill("stopped"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(self.kill("renderer stopped responding and was killed"));
            }
            match self.rx.recv_timeout(left.min(Duration::from_millis(25))) {
                Ok(Ok(frame)) => break frame,
                Ok(Err(e)) => return Err(self.kill(&e)),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(self.kill("renderer exited")),
            }
        };
        if frame.len() > *budget {
            return Err(self.kill("renderer sent more data than allowed and was killed"));
        }
        *budget -= frame.len();
        FromRenderer::decode(&frame)
            .map_err(|e| self.kill(&format!("renderer sent a malformed message ({e}) and was killed")))
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The browser/renderer boundary against renderers that misbehave on
/// purpose. Each fake renderer is a small Python script that speaks the
/// frame protocol and does one hostile thing.
#[cfg(test)]
mod hostile_renderer_tests {
    use super::*;
    use crate::ipc::FromRenderer as F;

    const FAKE: &str = r#"
import sys, time, struct
out = sys.stdout.buffer
def frame(b): return struct.pack('<I', len(b)) + b
def read_frame():
    n = sys.stdin.buffer.read(4)
    if len(n) < 4: sys.exit(0)
    sys.stdin.buffer.read(struct.unpack('<I', n)[0])
try:
    for a in sys.argv[1:]:
        k, _, v = a.partition(':')
        if k == 'raw': out.write(bytes.fromhex(v)); out.flush()
        elif k == 'send': out.write(frame(bytes.fromhex(v))); out.flush()
        elif k == 'flood':
            f = frame(bytes.fromhex(v))
            while True: out.write(f); out.flush()
        elif k == 'zeros': out.write(bytes(int(v))); out.flush()
        elif k == 'read': read_frame()
        elif k == 'sleep': time.sleep(float(v))
except BrokenPipeError:
    pass
"#;

    fn hex(m: F) -> String {
        m.encode().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A renderer that runs `script` (see FAKE), or None without python3.
    fn fake(script: &[String], cancel: Cancel) -> Option<Renderer> {
        if Command::new("python3").arg("-c").arg("").status().is_err() {
            eprintln!("python3 not found: skipping hostile renderer test");
            return None;
        }
        let mut cmd = Command::new("python3");
        cmd.arg("-I").arg("-c").arg(FAKE).args(script);
        Some(Renderer::spawn_command(cmd, cancel).expect("spawn fake renderer"))
    }

    fn fetch_msg(url: &str) -> String {
        hex(F::Fetch {
            url: url.into(),
            dest: Dest::Stylesheet,
        })
    }
    fn loaded() -> String {
        hex(F::Loaded {
            title: "t".into(),
            navigate: None,
            console: vec![],
        })
    }

    fn load(
        r: &mut Renderer,
        secs: f32,
        fetch: &mut dyn FnMut(&str, Dest) -> Result<String, String>,
    ) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs_f32(secs);
        r.load(
            "https://page.example/",
            "<p>hi",
            String::new(),
            false,
            DEFAULT_VIEWPORT,
            deadline,
            &mut crate::page::FetchOnly(fetch),
        )
        .map(|l| l.title)
    }

    fn no_fetch(_: &str, _: Dest) -> Result<String, String> {
        Err("no".into())
    }

    #[test]
    fn well_behaved_renderer_works() {
        let s = vec![
            "read".into(),
            format!("send:{}", fetch_msg("https://page.example/a.css")),
            "read".into(),
            format!("send:{}", loaded()),
        ];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let mut asked = vec![];
        let title = load(&mut r, 5.0, &mut |u, _| {
            asked.push(u.to_string());
            Ok(String::new())
        });
        assert_eq!(title.as_deref(), Ok("t"));
        assert_eq!(asked, vec!["https://page.example/a.css"]);
    }

    #[test]
    fn flood_of_requests_is_cut_off() {
        // Asks for stylesheets forever without reading the replies.
        let s = vec![
            "read".into(),
            format!("flood:{}", fetch_msg("https://page.example/x.css")),
        ];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let mut n = 0;
        let start = Instant::now();
        let err = load(&mut r, 10.0, &mut |_, _| {
            n += 1;
            Err("over budget".into())
        })
        .unwrap_err();
        assert!(
            err.contains("too many requests")
                || err.contains("nobody asked")
                || err.contains("stopped reading"),
            "{err}"
        );
        assert!(n <= MAX_FETCHES);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn unsolicited_messages_get_it_killed() {
        // Reports "loaded" three times in a row.
        let l = loaded();
        let s = vec![
            "read".into(),
            format!("send:{l}"),
            format!("send:{l}"),
            format!("send:{l}"),
            "sleep:5".into(),
        ];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        assert!(load(&mut r, 5.0, &mut no_fetch).is_ok());
        std::thread::sleep(Duration::from_millis(200)); // let the extras arrive
        let err = r
            .send(ToRenderer::Render {
                width: 100.0,
                height: 100.0,
                text_scale: 1.0,
            })
            .unwrap_err();
        assert!(err.contains("nobody asked"), "{err}");
    }

    #[test]
    fn out_of_order_replies_get_it_killed() {
        // Answers a load with a DOM dump.
        let s = vec![
            "read".into(),
            format!("send:{}", hex(F::Dom("x".into()))),
            "sleep:5".into(),
        ];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let err = load(&mut r, 5.0, &mut no_fetch).unwrap_err();
        assert!(err.contains("unexpected message"), "{err}");
    }

    #[test]
    fn stalling_and_partial_frames_time_out() {
        for script in [
            vec!["read".to_string(), "sleep:30".into()],
            // Promises 1000 bytes, sends 3, then goes quiet.
            vec!["read".to_string(), "raw:e8030000616263".into(), "sleep:30".into()],
        ] {
            let Some(mut r) = fake(&script, Cancel::new()) else {
                return;
            };
            let start = Instant::now();
            let err = load(&mut r, 0.5, &mut no_fetch).unwrap_err();
            assert!(err.contains("stopped responding"), "{err}");
            assert!(start.elapsed() < Duration::from_secs(3));
        }
        // A partial frame followed by exit is an error, not a hang.
        let s = vec!["read".to_string(), "raw:e8030000616263".into()];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let err = load(&mut r, 5.0, &mut no_fetch).unwrap_err();
        assert!(err.contains("truncated") || err.contains("exited"), "{err}");
    }

    #[test]
    fn oversized_and_garbage_frames_are_refused() {
        // Claims a 4 GB frame.
        let s = vec!["read".to_string(), "raw:ffffffff".into(), "sleep:5".into()];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let err = load(&mut r, 5.0, &mut no_fetch).unwrap_err();
        assert!(err.contains("too large"), "{err}");
        // More than a page load's byte budget, in one frame (refused
        // before it is even decoded).
        let len = (LOAD_BYTES as u32 + 1).to_le_bytes();
        let s = vec![
            "read".to_string(),
            format!(
                "raw:{}",
                len.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ),
            format!("zeros:{}", LOAD_BYTES + 1),
            "sleep:5".into(),
        ];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let err = load(&mut r, 5.0, &mut no_fetch).unwrap_err();
        assert!(err.contains("more data than allowed"), "{err}");
        // Not a message at all.
        let s = vec!["read".to_string(), "send:ff00ff00".into(), "sleep:5".into()];
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let err = load(&mut r, 5.0, &mut no_fetch).unwrap_err();
        assert!(err.contains("malformed"), "{err}");
    }

    #[test]
    fn renderer_that_stops_reading_cant_block_the_browser() {
        let Some(mut r) = fake(&["sleep:30".to_string()], Cancel::new()) else {
            return;
        };
        let html = "x".repeat(4 * 1024 * 1024); // far more than a pipe holds
        let start = Instant::now();
        let deadline = Instant::now() + Duration::from_millis(500);
        let err = r
            .load(
                "https://page.example/",
                &html,
                String::new(),
                false,
                DEFAULT_VIEWPORT,
                deadline,
                &mut crate::page::FetchOnly(no_fetch),
            )
            .map(|l| l.title)
            .unwrap_err();
        assert!(err.contains("stopped responding"), "{err}");
        // Give the reader thread time to notice the renderer is gone: the
        // answer must be the same however late it is.
        std::thread::sleep(Duration::from_millis(300));
        let err = r.send(ToRenderer::Dom).unwrap_err();
        assert!(err.contains("stopped"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn stop_kills_it_at_once() {
        let cancel = Cancel::new();
        let Some(mut r) = fake(&["read".to_string(), "sleep:30".into()], cancel.clone()) else {
            return;
        };
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.cancel();
        });
        let start = Instant::now();
        let err = load(&mut r, 30.0, &mut no_fetch).unwrap_err();
        assert_eq!(err, "stopped");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn forbidden_requests_go_through_browser_policy() {
        // The renderer asks for a local file, a device on your network and a
        // tracker; the browser's policy (not the renderer) refuses all three.
        let targets = [
            "file:///etc/passwd",
            "http://192.168.1.1/admin.css",
            "https://tracker.example/t.css",
        ];
        let mut s = vec!["read".to_string()];
        for t in targets {
            s.push(format!("send:{}", fetch_msg(t)));
            s.push("read".into());
        }
        s.push(format!("send:{}", loaded()));
        let Some(mut r) = fake(&s, Cancel::new()) else {
            return;
        };
        let mut net = Network::with_filters(crate::filter::Filters::parse("||tracker.example^\n"));
        let mut replies = vec![];
        load(&mut r, 5.0, &mut |u, _| {
            let res = net.subresource("https://page.example/", u, Dest::Stylesheet);
            replies.push(res.clone());
            res
        })
        .unwrap();
        assert!(replies.iter().all(|r| r.is_err()), "{replies:?}");
        for t in &targets[1..] {
            assert_eq!(net::requests_sent_to(t), 0, "nothing left the browser for {t}");
        }
        let log = net.take_log();
        use crate::network::Outcome;
        assert!(matches!(log[0].outcome, Outcome::NotAllowed));
        assert!(matches!(
            log[1].outcome,
            Outcome::LocalNetwork | Outcome::MixedContent
        ));
        assert!(matches!(log[2].outcome, Outcome::Blocked { .. }));
    }
}
