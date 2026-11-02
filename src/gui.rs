//! A minimal desktop browser window (address bar, scrolling, links, history).

use crate::css::Color;
use crate::font;
use crate::layout::Rect;
use crate::net;
use crate::page::{Page, Rendered};
use crate::paint::{Canvas, GlyphCache};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use std::sync::{Arc, Mutex};

const BAR_H: f32 = 40.0;
const STATUS_H: f32 = 22.0;

struct Chars(Arc<Mutex<Vec<char>>>);
impl minifb::InputCallback for Chars {
    fn add_char(&mut self, c: u32) {
        if let Some(c) = char::from_u32(c) {
            self.0.lock().unwrap().push(c);
        }
    }
}

pub fn run(start: &str) -> Result<(), String> {
    let (mut w, mut h) = (1024usize, 768usize);
    let mut window = Window::new(
        "Brickle Browser",
        w,
        h,
        WindowOptions { resize: true, ..WindowOptions::default() },
    )
    .map_err(|e| e.to_string())?;
    window.set_target_fps(60);
    let typed = Arc::new(Mutex::new(Vec::new()));
    window.set_input_callback(Box::new(Chars(typed.clone())));

    // History entries remember who opened them so back/forward can't be used
    // to sneak past the file:// rule.
    type Entry = (String, Option<String>);
    let mut history: Vec<Entry> = Vec::new();
    let mut forward: Vec<Entry> = Vec::new();
    let mut page = Page::load(start, None);
    let mut rendered: Option<Rendered> = None;
    let mut scroll = 0.0f32;
    let mut address = page.url.clone();
    let mut editing = false;
    let mut cache = GlyphCache::default();
    let mut was_down = false;
    let mut dirty = true;
    let mut hover: Option<String> = None;
    let mut canvas = Canvas::new(w, h, Color::WHITE);

    while window.is_open() {
        let (nw, nh) = window.get_size();
        if (nw, nh) != (w, h) && nw > 0 && nh > 0 {
            (w, h) = (nw, nh);
            rendered = None;
        }
        if rendered.is_none() {
            let r = page.render(w as f32, h as f32 - BAR_H);
            window.set_title(&format!("{} — Brickle Browser", if page.title.is_empty() { &page.url } else { &page.title }));
            rendered = Some(r);
            dirty = true;
        }
        let r = rendered.as_ref().unwrap();
        let view_h = h as f32 - BAR_H;
        let max_scroll = (r.height - view_h).max(0.0);
        let mut navigate: Option<Entry> = None;
        let mut hist = 0i32;

        // Keyboard
        let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
        let alt = window.is_key_down(Key::LeftAlt) || window.is_key_down(Key::RightAlt);
        let chars: Vec<char> = std::mem::take(&mut *typed.lock().unwrap());
        for key in window.get_keys_pressed(KeyRepeat::Yes) {
            if editing {
                match key {
                    Key::Enter => {
                        navigate = Some((net::normalize(&address), None));
                        editing = false;
                    }
                    Key::Escape => {
                        editing = false;
                        address = page.url.clone();
                    }
                    Key::Backspace => {
                        address.pop();
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
                    editing = true;
                    address.clear();
                }
                Key::R if ctrl => navigate = Some((page.url.clone(), page.opener.clone())),
                Key::F5 => navigate = Some((page.url.clone(), page.opener.clone())),
                Key::Left if alt => {
                    hist = -1;
                }
                Key::Backspace => {
                    hist = -1;
                }
                Key::Right if alt => {
                    hist = 1;
                }
                _ => {}
            }
            if scroll != before {
                dirty = true;
            }
        }
        if editing {
            for c in chars {
                if !c.is_control() {
                    address.push(c);
                    dirty = true;
                }
            }
        }

        // Mouse
        if let Some((_, dy)) = window.get_scroll_wheel() {
            scroll -= dy * 3.0;
            dirty = true;
        }
        scroll = scroll.clamp(0.0, max_scroll);
        let down = window.get_mouse_down(MouseButton::Left);
        if let Some((mx, my)) = window.get_mouse_pos(MouseMode::Discard) {
            let link = if my > BAR_H { r.link_at(mx, my - BAR_H + scroll).map(|s| s.to_string()) } else { None };
            if link != hover {
                hover = link.clone();
                dirty = true;
            }
            if down && !was_down {
                if my < BAR_H {
                    editing = true;
                    address.clear();
                    dirty = true;
                } else if let Some(l) = link {
                    navigate = Some((page.link_url(&l), Some(page.url.clone())));
                } else if editing {
                    editing = false;
                    address = page.url.clone();
                    dirty = true;
                }
            }
        }
        was_down = down;

        let moved = match hist {
            -1 => history.pop().inspect(|_| forward.push((page.url.clone(), page.opener.clone()))),
            1 => forward.pop().inspect(|_| history.push((page.url.clone(), page.opener.clone()))),
            _ => None,
        };
        if let Some((url, from)) = moved {
            page = Page::load(&url, from.as_deref());
            address = page.url.clone();
            rendered = None;
            scroll = 0.0;
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
            page = Page::load(&url, from.as_deref());
            address = page.url.clone();
            rendered = None;
            scroll = 0.0;
            hover = None;
            continue;
        }

        if dirty {
            canvas = Canvas::new(w, h, r.background);
            canvas.paint(&r.list, scroll, BAR_H, &mut cache);
            draw_chrome(&mut canvas, &address, editing, hover.as_deref(), scroll, max_scroll, view_h, &mut cache);
            dirty = false;
        }
        window.update_with_buffer(&canvas.pixels, w, h).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_chrome(
    c: &mut Canvas,
    address: &str,
    editing: bool,
    hover: Option<&str>,
    scroll: f32,
    max_scroll: f32,
    view_h: f32,
    cache: &mut GlyphCache,
) {
    let w = c.width as f32;
    let h = c.height as f32;
    c.fill(Rect { x: 0.0, y: 0.0, w, h: BAR_H }, Color::rgb(0xec, 0xee, 0xf1), 0.0);
    c.fill(Rect { x: 0.0, y: BAR_H - 1.0, w, h: 1.0 }, Color::rgb(0xc8, 0xcc, 0xd2), 0.0);
    let field = Rect { x: 10.0, y: 6.0, w: w - 20.0, h: BAR_H - 12.0 };
    let border = if editing { Color::rgb(0x3b, 0x82, 0xf6) } else { Color::rgb(0xc8, 0xcc, 0xd2) };
    c.fill(field.expand(crate::layout::Edges { t: 1.0, r: 1.0, b: 1.0, l: 1.0 }), border, 0.0);
    c.fill(field, Color::WHITE, 0.0);
    let f = 0; // sans
    let base = field.y + 19.0;
    let max_x = field.x + field.w - 10.0;
    let dark = Color::rgb(0x22, 0x22, 0x22);
    if editing {
        let shown = format!("{address}|");
        c.text(field.x + 10.0, base, &fit(&shown, max_x - field.x - 10.0, 0, 14.0), f, 14.0, dark, false, cache, 0.0);
    } else {
        // Anti-spoofing: always show the real (punycode) host in bold, the
        // rest of the URL dimmed, and warn about unencrypted pages.
        let mut x = field.x + 10.0;
        if net::scheme(address) == "http" {
            let red = Color::rgb(0xc0, 0x1c, 0x1c);
            c.text(x, base, "Not secure", 1, 13.0, red, false, cache, 0.0);
            x += font::measure(1, 13.0, "Not secure") + 6.0;
            c.fill(Rect { x, y: field.y + 6.0, w: 1.0, h: field.h - 12.0 }, Color::rgb(0xc8, 0xcc, 0xd2), 0.0);
            x += 8.0;
        }
        let (head, rest) = net::display_parts(address);
        let head = fit(&head, max_x - x, 1, 14.0);
        c.text(x, base, &head, 1, 14.0, dark, false, cache, 0.0);
        x += font::measure(1, 14.0, &head);
        let rest = fit(&rest, max_x - x, f, 14.0);
        c.text(x, base, &rest, f, 14.0, Color::rgb(0x6b, 0x72, 0x80), false, cache, 0.0);
    }

    // Scrollbar
    if max_scroll > 0.0 {
        let track = view_h;
        let thumb = (view_h / (view_h + max_scroll) * track).max(24.0);
        let y = BAR_H + scroll / max_scroll * (track - thumb);
        c.fill(Rect { x: w - 8.0, y, w: 6.0, h: thumb }, Color { r: 0, g: 0, b: 0, a: 90 }, BAR_H);
    }

    // Status bubble with the hovered link.
    if let Some(link) = hover {
        let (head, rest) = net::display_parts(link);
        let link = &fit(&format!("{head}{rest}"), w * 0.6, f, 12.0);
        let tw = font::measure(f, 12.0, link) + 16.0;
        let r = Rect { x: 0.0, y: h - STATUS_H, w: tw.min(w), h: STATUS_H };
        c.fill(r, Color::rgb(0xec, 0xee, 0xf1), BAR_H);
        c.text(8.0, h - 7.0, link, f, 12.0, Color::rgb(0x33, 0x33, 0x33), false, cache, BAR_H);
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
