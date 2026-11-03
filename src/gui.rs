//! The desktop browser window: address bar, page, request panel.

use crate::describe;
use brickle::css::Color;
use brickle::font;
use brickle::layout::{Edges, Rect};
use brickle::net;
use brickle::network::{LogEntry, Network, Outcome};
use brickle::page::Rendered;
use brickle::paint::{Canvas, GlyphCache};
use brickle::tab::Tab;
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use std::sync::{Arc, Mutex};

const BAR_H: f32 = 40.0;
const STATUS_H: f32 = 22.0;
const PANEL_W: f32 = 400.0;
const PANEL_HEAD: f32 = 58.0;
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

pub fn run(start: &str, sandboxed: bool, mut net: Network, panel: bool) -> Result<(), String> {
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

    let mut history: Vec<Entry> = Vec::new();
    let mut forward: Vec<Entry> = Vec::new();
    let mut page = Tab::open(&mut net, start, None, sandboxed);
    let mut rendered: Option<Rendered> = None;
    let mut scroll = 0.0f32;
    let mut ui = Ui {
        address: page.url.clone(),
        editing: false,
        hover: None,
        panel,
        panel_scroll: 0.0,
        flash: None,
    };
    let mut cache = GlyphCache::default();
    let mut was_down = false;
    let mut dirty = true;
    let mut canvas = Canvas::new(w, h, Color::WHITE);

    while window.is_open() {
        let (nw, nh) = window.get_size();
        if (nw, nh) != (w, h) && nw > 0 && nh > 0 {
            (w, h) = (nw, nh);
            rendered = None;
        }
        let page_w = if ui.panel {
            (w as f32 - PANEL_W).max(200.0)
        } else {
            w as f32
        };
        if rendered.is_none() {
            rendered = Some(page.render(page_w, h as f32 - BAR_H));
            let title = if page.title.is_empty() {
                &page.url
            } else {
                &page.title
            };
            window.set_title(&format!("{title} — Brickle Browser"));
            dirty = true;
        }
        let r = rendered.as_ref().unwrap();
        let view_h = h as f32 - BAR_H;
        let max_scroll = (r.height - view_h).max(0.0);
        let mut navigate: Option<Entry> = None;
        let mut reload = false;
        let mut hist = 0i32;
        // Panel toggled: lay the page out again at the new width.
        let mut relayout = false;

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
                        ui.address = page.url.clone();
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
                Key::F12 => relayout = true,
                Key::E if ctrl && shift => relayout = true,
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
        let badge = badge_rect(&page.log, w as f32);
        let down = window.get_mouse_down(MouseButton::Left);
        if let Some((mx, my)) = mouse {
            let row = if over_panel {
                panel_row(&page.log, my, ui.panel_scroll)
            } else {
                None
            };
            let hover = if let Some(i) = row {
                let e = &page.log[i];
                let (rule, verb) = net.toggle_preview(e);
                Some(if e.host().is_empty() {
                    "local file".to_string()
                } else {
                    format!("Click to {verb} {}  ({rule})", e.host())
                })
            } else if badge.contains(mx, my) {
                Some("Show every request this page made (F12)".into())
            } else if my > BAR_H && mx < page_w {
                r.link_at(mx, my - BAR_H + scroll).map(|s| s.to_string())
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
                    relayout = true;
                } else if my < BAR_H {
                    ui.editing = true;
                    ui.address.clear();
                    dirty = true;
                } else if let Some(i) = row {
                    let e = page.log[i].clone();
                    ui.flash = Some(match net.toggle(&e) {
                        Ok(msg) => {
                            reload = true;
                            msg
                        }
                        Err(err) => err,
                    });
                    dirty = true;
                } else if over_panel {
                } else if let Some(l) = r.link_at(mx, my - BAR_H + scroll) {
                    let url = page.link_url(l);
                    if let Some(rest) = url.strip_prefix("brickle:") {
                        // Internal actions only work on our own warning pages.
                        if let Some(action) = page.interstitial.clone() {
                            match rest {
                                "proceed" => {
                                    navigate = Some((net.allow_action(&action), page.opener.clone()))
                                }
                                "back" => hist = -1,
                                _ => {}
                            }
                        }
                    } else {
                        navigate = Some((url, Some(page.url.clone())));
                    }
                } else if ui.editing {
                    ui.editing = false;
                    ui.address = page.url.clone();
                    dirty = true;
                }
            }
        }
        was_down = down;
        if relayout {
            ui.panel = !ui.panel;
            rendered = None;
            continue;
        }

        let moved = match hist {
            -1 => history
                .pop()
                .inspect(|_| forward.push((page.url.clone(), page.opener.clone()))),
            1 => forward
                .pop()
                .inspect(|_| history.push((page.url.clone(), page.opener.clone()))),
            _ => None,
        };
        let target = if reload {
            Some((page.url.clone(), page.opener.clone()))
        } else {
            moved
        };
        if let Some((url, from)) = target {
            page = Tab::open(&mut net, &url, from.as_deref(), sandboxed);
            ui.address = page.url.clone();
            rendered = None;
            if !reload {
                scroll = 0.0;
            }
            continue;
        }
        if let Some((url, from)) = navigate {
            if url.starts_with("mailto:") || url.starts_with("javascript:") {
                continue;
            }
            if url != page.url {
                history.push((page.url.clone(), page.opener.clone()));
                forward.clear();
            }
            page = Tab::open(&mut net, &url, from.as_deref(), sandboxed);
            ui.address = page.url.clone();
            rendered = None;
            scroll = 0.0;
            ui.hover = None;
            ui.panel_scroll = 0.0;
            continue;
        }

        if dirty {
            canvas = Canvas::new(w, h, r.background);
            canvas.paint(&r.list, scroll, BAR_H, &mut cache);
            draw_chrome(
                &mut canvas,
                &ui,
                &page.log,
                scroll,
                max_scroll,
                view_h,
                page_w,
                &mut cache,
            );
            if ui.panel {
                draw_panel(&mut canvas, &page.log, &net, ui.panel_scroll, page_w, &mut cache);
            }
            draw_status(&mut canvas, &ui, page_w, &mut cache);
            dirty = false;
        }
        window
            .update_with_buffer(&canvas.pixels, w, h)
            .map_err(|e| e.to_string())?;
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
        if net::scheme(&ui.address) == "http" {
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
    log: &[LogEntry],
    net: &Network,
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
    let stats = net.filter_stats();
    let sub = if net.lists_loaded == 0 {
        "No filter lists. Run `brickle filters update`.".to_string()
    } else {
        format!(
            "{} lists, {}k rules · click a row to block/allow",
            net.lists_loaded,
            (stats.network + stats.cosmetic) / 1000
        )
    };
    let sub = fit(&sub, pw - 28.0, SANS, 11.0);
    text(c, x0 + 14.0, BAR_H + 42.0, &sub, SANS, 11.0, GRAY, cache, BAR_H);
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
            | Outcome::NotAllowed => RED,
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
