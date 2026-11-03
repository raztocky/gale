//! The desktop browser window: address bar, page, request panel.

use crate::describe;
use brickle::css::Color;
use brickle::font;
use brickle::layout::{Edges, Rect};
use brickle::net;
use brickle::network::{Action, LogEntry, Network, Outcome};
use brickle::page::Rendered;
use brickle::paint::{Canvas, GlyphCache, TileCache};
use brickle::tab::Tab;
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

const BAR_H: f32 = 40.0;
const STATUS_H: f32 = 22.0;
const PANEL_W: f32 = 400.0;
const PANEL_HEAD: f32 = 116.0;
const ROW_H: f32 = 38.0;

const SANS: usize = 0;
const BOLD: usize = 1;
const GRAY: Color = Color::rgb(0x6b, 0x72, 0x80);
const DARK: Color = Color::rgb(0x22, 0x22, 0x22);
const LINE: Color = Color::rgb(0xc8, 0xcc, 0xd2);
const CHROME: Color = Color::rgb(0xec, 0xee, 0xf1);
const RED: Color = Color::rgb(0xc0, 0x1c, 0x1c);
const GREEN: Color = Color::rgb(0x15, 0x80, 0x3d);
const AMBER: Color = Color::rgb(0xb4, 0x53, 0x09);

struct Chars(Arc<Mutex<Vec<char>>>);
impl minifb::InputCallback for Chars {
    fn add_char(&mut self, c: u32) {
        if let Some(c) = char::from_u32(c) {
            self.0.lock().unwrap().push(c);
        }
    }
}

/// History entries remember who opened them, so back/forward re-apply the
/// same security checks.
type Entry = (String, Option<String>);

struct Ui {
    address: String,
    editing: bool,
    hover: Option<String>,
    panel: bool,
    panel_scroll: f32,
    /// A short message shown in the status bar (e.g. "block: ||x^").
    flash: Option<String>,
}

/// What the window knows about the current page. The `Tab` itself may be
/// away on the loader thread, so this copy is what the UI reads.
#[derive(Clone)]
struct Info {
    url: String,
    opener: Option<String>,
    title: String,
    log: Vec<LogEntry>,
    interstitial: Option<Action>,
    /// The page runs JavaScript, so clicks go to its scripts first.
    scripts: bool,
}

impl Info {
    fn of(tab: &Tab) -> Info {
        Info {
            url: tab.url.clone(),
            opener: tab.opener.clone(),
            title: tab.title.clone(),
            log: tab.log.clone(),
            interstitial: tab.interstitial.clone(),
            scripts: tab.scripts,
        }
    }
}

/// Work done off the UI thread, so a slow site or a stuck renderer never
/// freezes the window.
enum Job {
    Open {
        url: String,
        from: Option<String>,
        w: f32,
        h: f32,
        cancel: net::Cancel,
    },
    Render {
        tab: Box<Tab>,
        w: f32,
        h: f32,
    },
    /// A click on a page with scripts; `link` is the link under it, if any.
    Click {
        tab: Box<Tab>,
        x: f32,
        y: f32,
        link: Option<String>,
        w: f32,
        h: f32,
    },
}

struct Done {
    tab: Tab,
    rendered: Rendered,
    new_page: bool,
    /// For a click: what scripts did, and the link that was clicked.
    click: Option<(brickle::tab::Click, Option<String>)>,
}

fn spawn_loader(net: Arc<Mutex<Network>>, sandboxed: bool) -> (Sender<Job>, Receiver<Done>) {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    std::thread::Builder::new()
        .name("brickle-loader".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            for job in job_rx {
                let done = match job {
                    Job::Open {
                        url,
                        from,
                        w,
                        h,
                        cancel,
                    } => {
                        let mut tab = {
                            let mut n = net.lock().unwrap_or_else(|e| e.into_inner());
                            n.cancel = cancel;
                            Tab::open_sized(&mut n, &url, from.as_deref(), sandboxed, (w, h))
                        };
                        let rendered = tab.render(w, h);
                        Done {
                            tab,
                            rendered,
                            new_page: true,
                            click: None,
                        }
                    }
                    Job::Render { mut tab, w, h } => {
                        let rendered = tab.render(w, h);
                        Done {
                            tab: *tab,
                            rendered,
                            new_page: false,
                            click: None,
                        }
                    }
                    Job::Click {
                        mut tab,
                        x,
                        y,
                        link,
                        w,
                        h,
                    } => {
                        let click = tab.click(x, y);
                        let rendered = tab.render(w, h);
                        Done {
                            tab: *tab,
                            rendered,
                            new_page: false,
                            click: Some((click, link)),
                        }
                    }
                };
                if done_tx.send(done).is_err() {
                    break;
                }
            }
        })
        .expect("loader thread");
    (job_tx, done_rx)
}

pub fn run(start: &str, sandboxed: bool, net: Network, panel: bool) -> Result<(), String> {
    let (mut w, mut h) = (1100usize, 768usize);
    let mut window = Window::new(
        "Brickle Browser",
        w,
        h,
        WindowOptions {
            resize: true,
            ..WindowOptions::default()
        },
    )
    .map_err(|e| e.to_string())?;
    window.set_target_fps(60);
    let typed = Arc::new(Mutex::new(Vec::new()));
    window.set_input_callback(Box::new(Chars(typed.clone())));

    let net = Arc::new(Mutex::new(net));
    let (jobs, done_rx) = spawn_loader(net.clone(), sandboxed);

    let mut history: Vec<Entry> = Vec::new();
    let mut forward: Vec<Entry> = Vec::new();
    let mut tab: Option<Tab> = None;
    let mut info = Info {
        url: start.to_string(),
        opener: None,
        title: String::new(),
        log: vec![],
        interstitial: None,
        scripts: false,
    };
    // Painted tiles of the current page; rebuilt when the page changes.
    let mut tiles: Option<TileCache> = None;
    let mut rendered = Rendered {
        list: vec![],
        atlas: vec![],
        height: 0.0,
        background: Color::WHITE,
        links: vec![],
    };
    // In flight: the URL being loaded (None for a re-layout).
    let mut busy: Option<Option<String>>;
    // Esc pressed: drop the result of the current load when it arrives.
    let mut discard = false;
    // Wanted while busy: a navigation, or a re-layout at a new size.
    let mut queued: Option<Entry> = None;
    let mut need_layout = false;
    // Navigations scripts asked for, and how many in a row (a page that
    // keeps redirecting itself is stopped).
    let mut script_nav: Option<String> = None;
    let mut script_hops = 0u32;
    let mut clicked_link: Option<String> = None;
    let mut laid_out_at = (0.0f32, 0.0f32);
    let mut scroll = 0.0f32;
    let mut ui = Ui {
        address: start.to_string(),
        editing: false,
        hover: None,
        panel,
        panel_scroll: 0.0,
        flash: None,
    };
    let mut cache = GlyphCache::default();
    let mut was_down = false;
    let mut dirty = true;
    let mut frame = 0u32;
    let mut canvas = Canvas::new(w, h, Color::WHITE);

    // Stop button for the load in flight.
    let mut stop = net::Cancel::new();
    let _ = jobs.send(Job::Open {
        url: start.to_string(),
        from: None,
        w: w as f32,
        h: h as f32 - BAR_H,
        cancel: stop.clone(),
    });
    busy = Some(Some(start.to_string()));

    while window.is_open() {
        frame = frame.wrapping_add(1);
        let (nw, nh) = window.get_size();
        if (nw, nh) != (w, h) && nw > 0 && nh > 0 {
            (w, h) = (nw, nh);
            dirty = true;
        }
        let page_w = if ui.panel {
            (w as f32 - PANEL_W).max(200.0)
        } else {
            w as f32
        };
        let view_h = h as f32 - BAR_H;

        // Results from the loader.
        while let Ok(done) = done_rx.try_recv() {
            let was_open = matches!(busy, Some(Some(_)));
            busy = None;
            if discard && was_open {
                discard = false;
                continue; // stopped: keep showing the old page
            }
            discard = false;
            if let Some((click, link)) = &done.click {
                // Scripts had their say; now the link (or their navigation).
                if let Some(to) = &click.navigate {
                    script_nav = Some(to.clone());
                } else if let (false, Some(l)) = (click.prevented, link) {
                    clicked_link = Some(l.clone());
                }
            }
            if done.new_page {
                if let Some(to) = &done.tab.script_navigate {
                    script_nav = Some(to.clone());
                }
            }
            info = Info::of(&done.tab);
            rendered = done.rendered;
            tiles = None; // new content: repaint tiles on demand
            laid_out_at = (page_w, view_h);
            tab = Some(done.tab);
            if done.new_page {
                ui.address = info.url.clone();
                ui.panel_scroll = 0.0;
                ui.hover = None;
                let title = if info.title.is_empty() {
                    &info.url
                } else {
                    &info.title
                };
                window.set_title(&format!("{title} — Brickle Browser"));
            }
            dirty = true;
        }
        // Start queued work once idle.
        if busy.is_none() {
            if let Some((url, from)) = queued.take() {
                stop = net::Cancel::new();
                let _ = jobs.send(Job::Open {
                    url: url.clone(),
                    from,
                    w: page_w,
                    h: view_h,
                    cancel: stop.clone(),
                });
                busy = Some(Some(url));
                scroll = 0.0;
            } else if need_layout || laid_out_at != (page_w, view_h) {
                need_layout = false;
                if let Some(t) = tab.take() {
                    let _ = jobs.send(Job::Render {
                        tab: Box::new(t),
                        w: page_w,
                        h: view_h,
                    });
                    busy = Some(None);
                }
            }
        }

        let max_scroll = (rendered.height - view_h).max(0.0);
        let mut navigate: Option<Entry> = None;
        let mut reload = false;
        let mut hist = 0i32;

        // Keyboard
        let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
        let shift = window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);
        let alt = window.is_key_down(Key::LeftAlt) || window.is_key_down(Key::RightAlt);
        let chars: Vec<char> = std::mem::take(&mut *typed.lock().unwrap());
        for key in window.get_keys_pressed(KeyRepeat::Yes) {
            if ui.editing {
                match key {
                    Key::Enter => {
                        navigate = Some((net::normalize(&ui.address), None));
                        ui.editing = false;
                    }
                    Key::Escape => {
                        ui.editing = false;
                        ui.address = info.url.clone();
                    }
                    Key::Backspace => {
                        ui.address.pop();
                    }
                    _ => {}
                }
                dirty = true;
                continue;
            }
            let before = scroll;
            match key {
                Key::Down => scroll += 40.0,
                Key::Up => scroll -= 40.0,
                Key::PageDown | Key::Space => scroll += view_h * 0.9,
                Key::PageUp => scroll -= view_h * 0.9,
                Key::Home => scroll = 0.0,
                Key::End => scroll = max_scroll,
                Key::L if ctrl => {
                    ui.editing = true;
                    ui.address.clear();
                    dirty = true;
                }
                Key::R if ctrl => reload = true,
                Key::F5 => reload = true,
                Key::F12 => ui.panel = !ui.panel,
                Key::E if ctrl && shift => ui.panel = !ui.panel,
                Key::Escape if matches!(busy, Some(Some(_))) => {
                    // Kills the renderer and abandons network requests, so
                    // the loader is free again almost at once.
                    stop.cancel();
                    discard = true;
                    queued = None;
                    ui.address = info.url.clone();
                    ui.flash = Some("Stopped".into());
                    dirty = true;
                }
                Key::Left if alt => hist = -1,
                Key::Backspace => hist = -1,
                Key::Right if alt => hist = 1,
                _ => {}
            }
            if scroll != before {
                dirty = true;
            }
        }
        if ui.editing {
            for c in chars.into_iter().filter(|c| !c.is_control()) {
                ui.address.push(c);
                dirty = true;
            }
        }

        // Mouse
        let mouse = window.get_mouse_pos(MouseMode::Discard);
        let over_panel = ui.panel && mouse.is_some_and(|(mx, my)| mx >= page_w && my > BAR_H);
        if let Some((_, dy)) = window.get_scroll_wheel() {
            if over_panel {
                ui.panel_scroll = (ui.panel_scroll - dy * 3.0).max(0.0);
            } else {
                scroll -= dy * 3.0;
            }
            dirty = true;
        }
        scroll = scroll.clamp(0.0, max_scroll);
        let badge = badge_rect(&info.log, w as f32);
        let down = window.get_mouse_down(MouseButton::Left);
        if let Some((mx, my)) = mouse {
            let row = if over_panel {
                panel_row(&info.log, my, ui.panel_scroll)
            } else {
                None
            };
            let button = if over_panel && net::scheme(&info.url).starts_with("http") {
                site_button_at(page_w, mx, my)
            } else {
                None
            };
            let hover = if let Some(b) = button {
                Some(match b {
                    SiteButton::KeepCookies => {
                        "Keep this site's cookies when Brickle closes (to stay logged in)".to_string()
                    }
                    SiteButton::ForgetCookies => "Delete this site's cookies now".to_string(),
                    SiteButton::ToggleJs => {
                        "Run this site's JavaScript (sandboxed; scripts get no network access)".to_string()
                    }
                })
            } else if let Some(i) = row {
                let e = &info.log[i];
                Some(match net.try_lock() {
                    _ if e.host().is_empty() => "local file".to_string(),
                    Ok(n) => {
                        let (rule, verb) = n.toggle_preview(e);
                        format!("Click to {verb} {}  ({rule})", e.host())
                    }
                    Err(_) => "Loading…".to_string(),
                })
            } else if badge.contains(mx, my) {
                Some("Show every request this page made (F12)".into())
            } else if my > BAR_H && mx < page_w {
                rendered.link_at(mx, my - BAR_H + scroll).map(|s| s.to_string())
            } else {
                None
            };
            if hover != ui.hover {
                ui.hover = hover;
                dirty = true;
            }
            if down && !was_down {
                ui.flash = None;
                if badge.contains(mx, my) {
                    ui.panel = !ui.panel;
                } else if my < BAR_H {
                    ui.editing = true;
                    ui.address.clear();
                } else if let Some(b) = button {
                    let site = brickle::cookies::site(&info.url);
                    ui.flash = Some(match (net.try_lock(), b) {
                        (Err(_), _) => "Wait for the page to finish loading".into(),
                        (Ok(mut n), SiteButton::KeepCookies) => {
                            let keep = !n.cookies.is_kept(&site);
                            match n.cookies.set_kept(&site, keep) {
                                Ok(true) => format!("Keeping cookies for {site} after Brickle closes"),
                                Ok(false) => {
                                    format!("Cookies for {site} will be deleted when Brickle closes")
                                }
                                Err(e) => e,
                            }
                        }
                        (Ok(mut n), SiteButton::ForgetCookies) => {
                            let count = n.cookies.forget(&site);
                            let _ = n.cookies.save();
                            format!("Deleted {count} cookie(s) for {site}")
                        }
                        (Ok(mut n), SiteButton::ToggleJs) => {
                            let on = !n.scripts_allowed(&info.url);
                            match n.set_scripts_allowed(&info.url, on) {
                                Ok(site) => {
                                    reload = true;
                                    format!("JavaScript {} for {site}", if on { "on" } else { "off" })
                                }
                                Err(e) => e,
                            }
                        }
                    });
                } else if let Some(i) = row {
                    let e = info.log[i].clone();
                    ui.flash = Some(match net.try_lock() {
                        Ok(mut n) => match n.toggle(&e) {
                            Ok(msg) => {
                                reload = true;
                                msg
                            }
                            Err(err) => err,
                        },
                        Err(_) => "Wait for the page to finish loading".into(),
                    });
                } else if over_panel {
                } else if info.scripts && my > BAR_H && busy.is_none() && tab.is_some() {
                    // Let the page's scripts see the click first.
                    let link = rendered.link_at(mx, my - BAR_H + scroll).map(|l| l.to_string());
                    if let Some(t) = tab.take() {
                        let _ = jobs.send(Job::Click {
                            tab: Box::new(t),
                            x: mx,
                            y: my - BAR_H + scroll,
                            link,
                            w: page_w,
                            h: view_h,
                        });
                        busy = Some(None);
                    }
                } else if let Some(l) = rendered.link_at(mx, my - BAR_H + scroll) {
                    let url = net::resolve(&info.url, l);
                    if let Some(rest) = url.strip_prefix("brickle:") {
                        // Internal actions only work on our own warning pages.
                        if let Some(action) = info.interstitial.clone() {
                            match rest {
                                "proceed" => {
                                    if let Ok(mut n) = net.try_lock() {
                                        navigate = Some((n.allow_action(&action), info.opener.clone()));
                                    }
                                }
                                "back" => hist = -1,
                                _ => {}
                            }
                        }
                    } else {
                        navigate = Some((url, Some(info.url.clone())));
                    }
                } else if ui.editing {
                    ui.editing = false;
                    ui.address = info.url.clone();
                }
                dirty = true;
            }
        }
        was_down = down;

        if let Some(l) = clicked_link.take() {
            let url = net::resolve(&info.url, &l);
            if !url.starts_with("brickle:") {
                navigate = Some((url, Some(info.url.clone())));
            }
        }
        if let Some(to) = script_nav.take() {
            script_hops += 1;
            if script_hops <= 5 {
                navigate = Some((to, Some(info.url.clone())));
            } else {
                ui.flash = Some("Stopped a page that kept redirecting itself".into());
            }
        } else if navigate.is_some() || reload || hist != 0 {
            script_hops = 0;
        }
        // Navigation requests are queued and started when the loader is free.
        let moved = match hist {
            -1 => history
                .pop()
                .inspect(|_| forward.push((info.url.clone(), info.opener.clone()))),
            1 => forward
                .pop()
                .inspect(|_| history.push((info.url.clone(), info.opener.clone()))),
            _ => None,
        };
        if reload {
            queued = Some((info.url.clone(), info.opener.clone()));
        } else if let Some(entry) = moved {
            queued = Some(entry);
        } else if let Some((url, from)) = navigate {
            if !(url.starts_with("mailto:") || url.starts_with("javascript:")) {
                if url != info.url {
                    history.push((info.url.clone(), info.opener.clone()));
                    forward.clear();
                }
                ui.address = url.clone();
                queued = Some((url, from));
            }
        }
        if queued.is_some() && matches!(busy, Some(Some(_))) {
            stop.cancel();
            discard = true; // superseded by the newer navigation
        }

        let loading = matches!(busy, Some(Some(_))) || queued.is_some();
        if dirty || loading {
            canvas = Canvas::new(w, h, rendered.background);
            if tiles.as_ref().is_some_and(|t| t.width() != page_w as usize) {
                tiles = None; // window resized: tiles are the wrong width
            }
            let tc = tiles.get_or_insert_with(|| {
                TileCache::new(
                    &rendered.list,
                    &rendered.atlas,
                    page_w as usize,
                    rendered.height,
                    rendered.background,
                )
            });
            tc.draw(
                &mut canvas,
                &rendered.list,
                &rendered.atlas,
                scroll,
                BAR_H as usize,
            );
            draw_chrome(
                &mut canvas,
                &ui,
                &info.log,
                scroll,
                max_scroll,
                view_h,
                page_w,
                &mut cache,
            );
            if loading {
                // Indeterminate progress bar under the address bar.
                let x = (frame % 120) as f32 / 120.0 * (w as f32 + 200.0) - 200.0;
                canvas.fill(
                    Rect {
                        x,
                        y: BAR_H - 3.0,
                        w: 200.0,
                        h: 3.0,
                    },
                    Color::rgb(0x3b, 0x82, 0xf6),
                    0.0,
                );
            }
            if ui.panel {
                let n = net.try_lock().ok();
                draw_panel(
                    &mut canvas,
                    &info.url,
                    &info.log,
                    n.as_deref(),
                    ui.panel_scroll,
                    page_w,
                    &mut cache,
                );
            }
            draw_status(&mut canvas, &ui, page_w, &mut cache);
            dirty = false;
        }
        window
            .update_with_buffer(&canvas.pixels, w, h)
            .map_err(|e| e.to_string())?;
    }
    // Session cookies die with the process; kept sites' cookies are saved.
    if let Err(e) = net.lock().unwrap_or_else(|e| e.into_inner()).cookies.save() {
        eprintln!("brickle: saving cookies: {e}");
    }
    Ok(())
}

fn counts(log: &[LogEntry]) -> (usize, usize) {
    let blocked = log
        .iter()
        .filter(|e| !e.was_sent() && e.outcome != Outcome::Local)
        .count();
    (blocked, log.len())
}

fn badge_text(log: &[LogEntry]) -> String {
    let (blocked, total) = counts(log);
    format!("{blocked} blocked · {total} requests")
}

fn badge_rect(log: &[LogEntry], w: f32) -> Rect {
    let tw = font::measure(SANS, 12.0, &badge_text(log)) + 20.0;
    Rect {
        x: w - tw - 10.0,
        y: 8.0,
        w: tw,
        h: BAR_H - 16.0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SiteButton {
    KeepCookies,
    ForgetCookies,
    ToggleJs,
}

/// Per-site controls at the top of the request panel.
fn site_buttons(x0: f32) -> [(Rect, SiteButton); 3] {
    let r = |x: f32, y: f32, w: f32| Rect {
        x: x0 + x,
        y: BAR_H + y,
        w,
        h: 22.0,
    };
    [
        (r(100.0, 54.0, 140.0), SiteButton::KeepCookies),
        (r(246.0, 54.0, 60.0), SiteButton::ForgetCookies),
        (r(100.0, 82.0, 206.0), SiteButton::ToggleJs),
    ]
}

fn site_button_at(x0: f32, mx: f32, my: f32) -> Option<SiteButton> {
    site_buttons(x0)
        .into_iter()
        .find(|(r, _)| r.contains(mx, my))
        .map(|(_, b)| b)
}

fn panel_row(log: &[LogEntry], my: f32, panel_scroll: f32) -> Option<usize> {
    let y = my - BAR_H - PANEL_HEAD + panel_scroll;
    if y < 0.0 {
        return None;
    }
    let i = (y / ROW_H) as usize;
    (i < log.len()).then_some(i)
}

#[allow(clippy::too_many_arguments)]
fn text(
    c: &mut Canvas,
    x: f32,
    y: f32,
    s: &str,
    font: usize,
    size: f32,
    color: Color,
    cache: &mut GlyphCache,
    clip: f32,
) {
    c.text(x, y, s, font, size, color, false, cache, clip);
}

#[allow(clippy::too_many_arguments)]
fn draw_chrome(
    c: &mut Canvas,
    ui: &Ui,
    log: &[LogEntry],
    scroll: f32,
    max_scroll: f32,
    view_h: f32,
    page_w: f32,
    cache: &mut GlyphCache,
) {
    let w = c.width as f32;
    c.fill(
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h: BAR_H,
        },
        CHROME,
        0.0,
    );
    c.fill(
        Rect {
            x: 0.0,
            y: BAR_H - 1.0,
            w,
            h: 1.0,
        },
        LINE,
        0.0,
    );

    // Request counter button.
    let badge = badge_rect(log, w);
    let (blocked, _) = counts(log);
    c.fill(
        badge,
        if ui.panel {
            Color::rgb(0xdb, 0xea, 0xfe)
        } else {
            Color::WHITE
        },
        0.0,
    );
    let label = badge_text(log);
    let col = if blocked > 0 { RED } else { GRAY };
    text(
        c,
        badge.x + 10.0,
        badge.y + 16.0,
        &label,
        SANS,
        12.0,
        col,
        cache,
        0.0,
    );

    let field = Rect {
        x: 10.0,
        y: 6.0,
        w: badge.x - 20.0,
        h: BAR_H - 12.0,
    };
    let border = if ui.editing {
        Color::rgb(0x3b, 0x82, 0xf6)
    } else {
        LINE
    };
    c.fill(
        field.expand(Edges {
            t: 1.0,
            r: 1.0,
            b: 1.0,
            l: 1.0,
        }),
        border,
        0.0,
    );
    c.fill(field, Color::WHITE, 0.0);
    let base = field.y + 19.0;
    let max_x = field.x + field.w - 10.0;
    if ui.editing {
        let shown = fit(&format!("{}|", ui.address), max_x - field.x - 10.0, SANS, 14.0);
        text(c, field.x + 10.0, base, &shown, SANS, 14.0, DARK, cache, 0.0);
    } else {
        // Anti-spoofing: the real (punycode) host in bold, the rest dimmed,
        // and a warning on unencrypted pages.
        let mut x = field.x + 10.0;
        if net::scheme(&ui.address) == "http" && !net::is_local_host(&brickle::filter::host_of(&ui.address)) {
            text(c, x, base, "Not secure", BOLD, 13.0, RED, cache, 0.0);
            x += font::measure(BOLD, 13.0, "Not secure") + 6.0;
            c.fill(
                Rect {
                    x,
                    y: field.y + 6.0,
                    w: 1.0,
                    h: field.h - 12.0,
                },
                LINE,
                0.0,
            );
            x += 8.0;
        }
        let (head, rest) = net::display_parts(&ui.address);
        let head = fit(&head, max_x - x, BOLD, 14.0);
        text(c, x, base, &head, BOLD, 14.0, DARK, cache, 0.0);
        x += font::measure(BOLD, 14.0, &head);
        let rest = fit(&rest, max_x - x, SANS, 14.0);
        text(c, x, base, &rest, SANS, 14.0, GRAY, cache, 0.0);
    }

    if max_scroll > 0.0 {
        let thumb = (view_h / (view_h + max_scroll) * view_h).max(24.0);
        let y = BAR_H + scroll / max_scroll * (view_h - thumb);
        let bar = Rect {
            x: page_w - 8.0,
            y,
            w: 6.0,
            h: thumb,
        };
        c.fill(
            bar,
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 90,
            },
            BAR_H,
        );
    }
}

fn draw_panel(
    c: &mut Canvas,
    page_url: &str,
    log: &[LogEntry],
    net: Option<&Network>,
    panel_scroll: f32,
    x0: f32,
    cache: &mut GlyphCache,
) {
    let (w, h) = (c.width as f32, c.height as f32);
    let pw = w - x0;
    c.fill(
        Rect {
            x: x0,
            y: BAR_H,
            w: pw,
            h: h - BAR_H,
        },
        Color::rgb(0xf8, 0xf9, 0xfb),
        BAR_H,
    );
    c.fill(
        Rect {
            x: x0,
            y: BAR_H,
            w: 1.0,
            h: h - BAR_H,
        },
        LINE,
        BAR_H,
    );
    let (blocked, total) = counts(log);
    text(
        c,
        x0 + 14.0,
        BAR_H + 22.0,
        &format!("Requests  {total}  ·  blocked {blocked}"),
        BOLD,
        13.0,
        DARK,
        cache,
        BAR_H,
    );
    let sub = match net {
        None => "Loading…".to_string(),
        Some(n) if n.lists_loaded == 0 => "No filter lists. Run `brickle filters update`.".to_string(),
        Some(n) => {
            let stats = n.filter_stats();
            format!(
                "{} lists, {}k rules · click a row to block/allow",
                n.lists_loaded,
                (stats.network + stats.cosmetic) / 1000
            )
        }
    };
    let sub = fit(&sub, pw - 28.0, SANS, 11.0);
    text(c, x0 + 14.0, BAR_H + 42.0, &sub, SANS, 11.0, GRAY, cache, BAR_H);
    if let Some(n) = net.filter(|_| net::scheme(page_url).starts_with("http")) {
        let count = n.cookies.for_site(page_url).len();
        let kept = n.cookies.is_kept(page_url);
        let label = format!("Cookies: {count}");
        text(c, x0 + 14.0, BAR_H + 68.0, &label, BOLD, 11.0, DARK, cache, BAR_H);
        text(
            c,
            x0 + 14.0,
            BAR_H + 96.0,
            "JavaScript:",
            BOLD,
            11.0,
            DARK,
            cache,
            BAR_H,
        );
        let js = n.scripts_allowed(page_url);
        for (r, b) in site_buttons(x0) {
            let (caption, on) = match b {
                SiteButton::KeepCookies if kept => ("Kept after closing ✓", true),
                SiteButton::KeepCookies => ("Keep after closing", false),
                SiteButton::ForgetCookies => ("Forget", false),
                SiteButton::ToggleJs if js => ("On for this site ✓", true),
                SiteButton::ToggleJs => ("Off · allow on this site", false),
            };
            let (bg, fg) = if on {
                (GREEN, Color::WHITE)
            } else {
                (Color::WHITE, DARK)
            };
            c.fill(r, LINE, BAR_H);
            c.fill(
                Rect {
                    x: r.x + 1.0,
                    y: r.y + 1.0,
                    w: r.w - 2.0,
                    h: r.h - 2.0,
                },
                bg,
                BAR_H,
            );
            let tw = font::measure(SANS, 11.0, caption);
            text(
                c,
                r.x + (r.w - tw) / 2.0,
                r.y + 15.0,
                caption,
                SANS,
                11.0,
                fg,
                cache,
                BAR_H,
            );
        }
    }
    c.fill(
        Rect {
            x: x0,
            y: BAR_H + PANEL_HEAD - 1.0,
            w: pw,
            h: 1.0,
        },
        LINE,
        BAR_H,
    );

    let clip = BAR_H + PANEL_HEAD;
    for (i, e) in log.iter().enumerate() {
        let y = clip + i as f32 * ROW_H - panel_scroll;
        if y + ROW_H < clip || y > h {
            continue;
        }
        let (status, notes) = describe(e);
        let color = match e.outcome {
            Outcome::Blocked { .. }
            | Outcome::Denied { .. }
            | Outcome::MixedContent
            | Outcome::NotAllowed
            | Outcome::LocalNetwork
            | Outcome::BadCertificate(_) => RED,
            Outcome::NeedsHttpConsent | Outcome::Failed(_) | Outcome::OverLimit => AMBER,
            _ if e.insecure => AMBER,
            _ => GREEN,
        };
        c.fill(
            Rect {
                x: x0 + 1.0,
                y,
                w: 4.0,
                h: ROW_H - 1.0,
            },
            color,
            clip,
        );
        let kind = match e.dest {
            net::Dest::Document => "page",
            net::Dest::Stylesheet => "css",
            net::Dest::Script => "js",
            net::Dest::Download => "list",
        };
        let head = format!("{status} · {kind}");
        text(c, x0 + 14.0, y + 15.0, &head, BOLD, 11.0, color, cache, clip);
        let hx = x0 + 14.0 + font::measure(BOLD, 11.0, &head) + 8.0;
        let (host, rest) = net::display_parts(&e.url);
        let host = host.split_once("://").map(|(_, h)| h.to_string()).unwrap_or(host);
        let shown = fit(&format!("{host}{rest}"), w - hx - 10.0, SANS, 11.0);
        text(c, hx, y + 15.0, &shown, SANS, 11.0, DARK, cache, clip);
        if !notes.is_empty() {
            let notes = fit(&notes, pw - 28.0, SANS, 10.0);
            text(c, x0 + 14.0, y + 30.0, &notes, SANS, 10.0, GRAY, cache, clip);
        }
        c.fill(
            Rect {
                x: x0,
                y: y + ROW_H - 1.0,
                w: pw,
                h: 1.0,
            },
            Color::rgb(0xe5, 0xe7, 0xeb),
            clip,
        );
    }
}

fn draw_status(c: &mut Canvas, ui: &Ui, page_w: f32, cache: &mut GlyphCache) {
    let h = c.height as f32;
    let msg = ui.flash.as_deref().or(ui.hover.as_deref());
    if let Some(msg) = msg {
        let shown = if msg.contains("://") && !msg.starts_with("Click") {
            let (head, rest) = net::display_parts(msg);
            format!("{head}{rest}")
        } else {
            msg.to_string()
        };
        let shown = fit(&shown, page_w * 0.9, SANS, 12.0);
        let tw = font::measure(SANS, 12.0, &shown) + 16.0;
        c.fill(
            Rect {
                x: 0.0,
                y: h - STATUS_H,
                w: tw,
                h: STATUS_H,
            },
            CHROME,
            BAR_H,
        );
        text(
            c,
            8.0,
            h - 7.0,
            &shown,
            SANS,
            12.0,
            Color::rgb(0x33, 0x33, 0x33),
            cache,
            BAR_H,
        );
    }
}

/// Truncate `text` with an ellipsis so it fits in `max_w` pixels.
fn fit(text: &str, max_w: f32, font_idx: usize, size: f32) -> String {
    if font::measure(font_idx, size, text) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if font::measure(font_idx, size, &out) + font::measure(font_idx, size, "…") > max_w {
            out.pop();
            break;
        }
    }
    out.push('…');
    out
}
