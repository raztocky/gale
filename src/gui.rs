//! The desktop browser window: address bar, page, request panel.

use crate::describe;
use brickle::css::Color;
use brickle::font;
use brickle::forms::Submission;
use brickle::layout::{Edges, Rect};
use brickle::lineedit::LineEdit;
use brickle::net;
use brickle::network::{Action, LogEntry, Network, Outcome};
use brickle::page::{Key as PageKey, Rendered};
use brickle::paint::{Canvas, DisplayList, GlyphCache, Layer, TileCache};
use brickle::select::{self, Bands, FindText, Pos};
use brickle::tab::{Input, Tab};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// The tab strip, then the address bar.
const MENU_H: f32 = 26.0;
const TABS_H: f32 = 30.0;
const ADDR_H: f32 = 40.0;
const BAR_H: f32 = MENU_H + TABS_H + ADDR_H;
/// Where the tab strip starts.
const TABS_Y: f32 = MENU_H;
const MENU_ITEM_H: f32 = 26.0;
/// Back, forward and reload buttons, then the address field.
const NAV_W: f32 = 32.0;
const FIELD_X: f32 = 8.0 + 3.0 * (NAV_W + 2.0) + 6.0;
const MENU_W: f32 = 250.0;

/// Your interface size: the menu bar, tabs, address bar, panels and status
/// bar are laid out in interface units, drawn and hit-tested this many
/// pixels to the unit. (An f32's bits.)
static UI_SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);

fn ui_scale() -> f32 {
    f32::from_bits(UI_SCALE.load(Ordering::Relaxed))
}

/// The top bars' height in window pixels: where the page starts.
fn bar_h() -> f32 {
    (BAR_H * ui_scale()).round()
}

/// The interface is dark (the default) or light.
static DARK_UI: AtomicBool = AtomicBool::new(true);

fn theme_file() -> Option<std::path::PathBuf> {
    brickle::network::config_dir().map(|d| d.join("interface-theme"))
}

/// Interface sizes the View menu and Ctrl+Alt+=/- step through.
const UI_SCALES: [f32; 7] = [0.8, 0.9, 1.0, 1.25, 1.5, 1.75, 2.0];

/// Text sizes the menu and Ctrl+=/Ctrl+- step through.
const TEXT_SCALES: [f32; 12] = [0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
const DEFAULT_TEXT_SCALE: f32 = 1.25;

/// A menu item: its label, the shortcut shown beside it, and the key (with
/// Ctrl, Shift, Alt) it presses, so menus and shortcuts always agree.
struct MenuItem {
    label: &'static str,
    keys: &'static str,
    press: (Key, bool, bool, bool),
}

const fn item(label: &'static str, keys: &'static str, press: (Key, bool, bool, bool)) -> MenuItem {
    MenuItem { label, keys, press }
}

const MENUS: [(&str, &[MenuItem]); 4] = [
    (
        "File",
        &[
            item("New tab", "Ctrl+T", (Key::T, true, false, false)),
            item("Close tab", "Ctrl+W", (Key::W, true, false, false)),
            item("Quit", "Ctrl+Q", (Key::Q, true, false, false)),
        ],
    ),
    (
        "Edit",
        &[
            item("Copy", "Ctrl+C", (Key::C, true, false, false)),
            item("Paste", "Ctrl+V", (Key::V, true, false, false)),
            item("Find in page", "Ctrl+F", (Key::F, true, false, false)),
            item("Edit address", "Ctrl+L", (Key::L, true, false, false)),
        ],
    ),
    (
        "View",
        &[
            item("Bigger text", "Ctrl+=", (Key::Equal, true, false, false)),
            item("Smaller text", "Ctrl+-", (Key::Minus, true, false, false)),
            item("Normal text size", "Ctrl+0", (Key::Key0, true, false, false)),
            item("Bigger interface", "Ctrl+Alt+=", (Key::Equal, true, false, true)),
            item("Smaller interface", "Ctrl+Alt+-", (Key::Minus, true, false, true)),
            item(
                "Normal interface size",
                "Ctrl+Alt+0",
                (Key::Key0, true, false, true),
            ),
            item(
                "Light / dark interface",
                "Ctrl+Shift+D",
                (Key::D, true, true, false),
            ),
            item("Requests panel", "F12", (Key::F12, false, false, false)),
        ],
    ),
    (
        "History",
        &[
            item("Back", "Alt+Left", (Key::Left, false, false, true)),
            item("Forward", "Alt+Right", (Key::Right, false, false, true)),
            item("Reload", "Ctrl+R", (Key::R, true, false, false)),
        ],
    ),
];
/// Most tabs open at once (each has its own renderer process).
const MAX_TABS: usize = 30;
const STATUS_H: f32 = 22.0;
const PANEL_W: f32 = 400.0;
const PANEL_HEAD: f32 = 116.0;
const ROW_H: f32 = 38.0;
/// Longest address, and longest paste, we take from the clipboard.
const MAX_ADDRESS: usize = 8 * 1024;
const MAX_PASTE: usize = 64 * 1024;

const SANS: usize = 0;
const BOLD: usize = 1;
const GRAY: Color = Color::rgb(0x6b, 0x72, 0x80);
const DARK: Color = Color::rgb(0x22, 0x22, 0x22);
const LINE: Color = Color::rgb(0xc8, 0xcc, 0xd2);
const CHROME: Color = Color::rgb(0xec, 0xee, 0xf1);
const RED: Color = Color::rgb(0xc0, 0x1c, 0x1c);
const GREEN: Color = Color::rgb(0x15, 0x80, 0x3d);
const AMBER: Color = Color::rgb(0xb4, 0x53, 0x09);
/// Drawn over find-in-page matches, and over the current one.
const MATCH: Color = Color {
    r: 0xff,
    g: 0xd5,
    b: 0x4f,
    a: 0x90,
};
const CURRENT_MATCH: Color = Color {
    r: 0xff,
    g: 0x8c,
    b: 0x1a,
    a: 0xb0,
};
const FIND_W: f32 = 340.0;
/// Pixels a mouse wheel notch scrolls, at 100% text.
const WHEEL_STEP: f32 = 48.0;
/// The scrollbar's width, at the page's right edge, in interface units.
const SCROLLBAR_W: f32 = 12.0;
const FIND_H: f32 = 30.0;
/// Drawn over selected text.
const SELECTION: Color = Color {
    r: 0x33,
    g: 0x90,
    b: 0xff,
    a: 0x60,
};

struct Chars(Arc<Mutex<Vec<char>>>);
impl minifb::InputCallback for Chars {
    fn add_char(&mut self, c: u32) {
        if let Some(c) = char::from_u32(c) {
            self.0.lock().unwrap().push(c);
        }
    }
}

/// The find-in-page bar (Ctrl+F): what you're looking for and where it is.
struct Find {
    query: LineEdit,
    matches: Vec<(Pos, Pos)>,
    current: usize,
    /// The page's text, flattened for searching; built when first needed.
    text: Option<FindText>,
}

impl Find {
    fn new() -> Find {
        Find {
            query: LineEdit::new(1024),
            matches: Vec::new(),
            current: 0,
            text: None,
        }
    }

    /// Search again (the query or the page changed).
    fn search(&mut self, r: &Rendered) {
        let text = self.text.get_or_insert_with(|| FindText::new(&r.texts));
        self.matches = text.find(&r.texts, &self.query.text);
        self.current = self.current.min(self.matches.len().saturating_sub(1));
    }

    /// The page changed under the bar.
    fn page_changed(&mut self, r: &Rendered) {
        self.text = None;
        self.search(r);
    }

    fn step(&mut self, back: bool) {
        let n = self.matches.len();
        if n > 0 {
            self.current = if back {
                (self.current + n - 1) % n
            } else {
                (self.current + 1) % n
            };
        }
    }

    /// A scroll position that shows the current match, if it's off screen.
    fn scroll_to(&self, r: &Rendered, scroll: f32, view_h: f32, max_scroll: f32) -> Option<f32> {
        let (start, _) = *self.matches.get(self.current)?;
        let rect = r.texts.get(start.0)?.rect;
        let visible =
            rect.y >= scroll && rect.y + rect.h <= scroll + view_h - (FIND_H + STATUS_H) * ui_scale();
        (!visible).then(|| (rect.y - view_h / 3.0).clamp(0.0, max_scroll))
    }
}

/// Lookups into the current page's text and links, so hovering, clicking
/// and selecting look only near the mouse however much a page sends.
struct PageIndex {
    text: Bands,
    links: Bands,
    /// The page's display list without its fixed and sticky layers, and
    /// each layer's own list.
    page: DisplayList,
    layers: Vec<(Layer, DisplayList)>,
}

impl PageIndex {
    fn new(r: &Rendered) -> PageIndex {
        let mut page = Vec::with_capacity(r.list.len());
        let mut layers = Vec::new();
        let mut at = 0;
        for l in &r.layers {
            page.extend_from_slice(&r.list[at.min(l.start)..l.start]);
            layers.push((*l, r.list[l.start..l.end].to_vec()));
            at = l.end;
        }
        page.extend_from_slice(&r.list[at.min(r.list.len())..]);
        PageIndex {
            text: Bands::new(r.texts.iter().map(|t| t.rect)),
            links: Bands::new(r.links.iter().map(|(rect, _)| *rect)),
            page,
            layers,
        }
    }

    /// The page point under window point (`mx`, `my`): on a fixed or sticky
    /// layer if one is there (topmost first), else on the scrolled page.
    fn page_point(&self, mx: f32, my: f32, scroll: f32) -> (f32, f32) {
        let vy = my - bar_h();
        for (l, _) in self.layers.iter().rev() {
            let y = vy + scroll - l.shift(scroll);
            if l.bounds.contains(mx, y) {
                return (mx, y);
            }
        }
        (mx, vy + scroll)
    }

    /// The topmost link at a window point.
    fn link_at_window<'r>(&self, r: &'r Rendered, mx: f32, my: f32, scroll: f32) -> Option<&'r str> {
        let (x, y) = self.page_point(mx, my, scroll);
        self.link_at(r, x, y)
    }

    /// Topmost link under a page-space point.
    fn link_at<'r>(&self, r: &'r Rendered, x: f32, y: f32) -> Option<&'r str> {
        self.links
            .at(y)
            .into_iter()
            .rev()
            .filter_map(|i| r.links.get(i))
            .find(|(rect, _)| rect.contains(x, y))
            .map(|(_, l)| l.as_str())
    }
}

/// The system clipboard, opened when first used (it may be unavailable,
/// e.g. without a display server). Only the browser process touches it, and
/// only when you press a copy or paste key: pages can never read it.
/// On X11 what we copy is served by this object, so it lives as long as the
/// window.
struct Clip(Option<arboard::Clipboard>);

impl Clip {
    fn get(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.0.is_none() {
            self.0 = arboard::Clipboard::new().ok();
        }
        self.0.as_mut()
    }

    fn paste(&mut self) -> Option<String> {
        Some(clean_paste(&self.get()?.get_text().ok()?))
    }

    fn copy(&mut self, text: &str) -> bool {
        self.get().is_some_and(|c| c.set_text(text).is_ok())
    }
}

/// Clipboard text as typed input: line endings as `\n`, control characters
/// other than newlines and tabs dropped, and at most `MAX_PASTE` bytes.
fn clean_paste(t: &str) -> String {
    let t = t.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::new();
    for c in t.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t') {
        if out.len() + c.len_utf8() > MAX_PASTE {
            break;
        }
        out.push(c);
    }
    out
}

/// History entries remember who opened them, so back/forward re-apply the
/// same security checks.
type Entry = (String, Option<String>);

struct Ui {
    /// The address bar: what it shows, or what you're typing.
    address: LineEdit,
    editing: bool,
    hover: Option<String>,
    panel: bool,
    panel_scroll: f32,
    /// A short message shown in the status bar (e.g. "block: ||x^").
    flash: Option<String>,
    /// The menu that's open, by index into `MENUS`.
    menu: Option<usize>,
    /// Your text size (see `StyleContext::text_scale`).
    text_scale: f32,
    /// For the back, forward and reload/stop buttons.
    can_back: bool,
    can_forward: bool,
    loading: bool,
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
}

impl Info {
    fn of(tab: &Tab) -> Info {
        Info {
            url: tab.url.clone(),
            opener: tab.opener.clone(),
            title: tab.title.clone(),
            log: tab.log.clone(),
            interstitial: tab.interstitial.clone(),
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
        /// A form submission to send instead of a plain request.
        post: Option<Submission>,
    },
    Render {
        tab: Box<Tab>,
        w: f32,
        h: f32,
    },
    /// A click, typing or a key on the page; `link` is the link under a
    /// click, if any.
    Input {
        tab: Box<Tab>,
        input: Input,
        link: Option<String>,
        w: f32,
        h: f32,
    },
}

struct Done {
    tab: Tab,
    /// None if the page didn't change.
    rendered: Option<Rendered>,
    new_page: bool,
    /// For a click: what scripts did, and the link that was clicked.
    click: Option<(brickle::tab::Click, Option<String>)>,
}

/// Which tab a job or its result is for.
type TabId = u64;

/// The window's ends of the loader's channels.
type Loader = (Sender<(TabId, Job)>, Receiver<(TabId, Done)>);

fn spawn_loader(net: Arc<Mutex<Network>>, sandboxed: bool, scale: Arc<AtomicU32>) -> Loader {
    let (job_tx, job_rx) = mpsc::channel::<(TabId, Job)>();
    let (done_tx, done_rx) = mpsc::channel::<(TabId, Done)>();
    std::thread::Builder::new()
        .name("brickle-loader".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            for (id, job) in job_rx {
                let text_scale = f32::from_bits(scale.load(Ordering::Relaxed));
                let done = match job {
                    Job::Open {
                        url,
                        from,
                        w,
                        h,
                        cancel,
                        post,
                    } => {
                        let mut tab = {
                            let mut n = net.lock().unwrap_or_else(|e| e.into_inner());
                            n.cancel = cancel;
                            match &post {
                                Some(sub) => Tab::submit(&mut n, sub, from.as_deref(), sandboxed, (w, h)),
                                // The built-in start page.
                                None if url == "about:home" => Tab::with_html_sized(
                                    &mut n,
                                    "about:home",
                                    None,
                                    crate::HOME,
                                    sandboxed,
                                    (w, h),
                                ),
                                None => Tab::open_sized(&mut n, &url, from.as_deref(), sandboxed, (w, h)),
                            }
                        };
                        tab.text_scale = text_scale;
                        let rendered = Some(tab.render(w, h));
                        Done {
                            tab,
                            rendered,
                            new_page: true,
                            click: None,
                        }
                    }
                    Job::Render { mut tab, w, h } => {
                        tab.text_scale = text_scale;
                        let rendered = Some(tab.render(w, h));
                        Done {
                            tab: *tab,
                            rendered,
                            new_page: false,
                            click: None,
                        }
                    }
                    Job::Input {
                        mut tab,
                        input,
                        link,
                        w,
                        h,
                    } => {
                        let click = {
                            let mut n = net.lock().unwrap_or_else(|e| e.into_inner());
                            tab.interact(&mut n, input)
                        };
                        tab.text_scale = text_scale;
                        let rendered = click.changed.then(|| tab.render(w, h));
                        Done {
                            tab: *tab,
                            rendered,
                            new_page: false,
                            click: Some((click, link)),
                        }
                    }
                };
                if done_tx.send((id, done)).is_err() {
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
    let mut clip = Clip(None);

    let net = Arc::new(Mutex::new(net));
    let shared_scale = Arc::new(AtomicU32::new(DEFAULT_TEXT_SCALE.to_bits()));
    let (jobs, done_rx) = spawn_loader(net.clone(), sandboxed, shared_scale.clone());

    let mut history: Vec<Entry> = Vec::new();
    let mut forward: Vec<Entry> = Vec::new();
    let mut tab: Option<Tab> = None;
    let mut info = Info {
        url: start.to_string(),
        opener: None,
        title: String::new(),
        log: vec![],
        interstitial: None,
    };
    // Painted tiles of the current page; rebuilt when the page changes.
    let mut tiles: Option<TileCache> = None;
    let mut rendered = Rendered {
        list: vec![],
        atlas: vec![],
        height: 0.0,
        background: Color::WHITE,
        links: vec![],
        texts: vec![],
        layers: vec![],
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
    // A form field on the page has the keyboard; what you typed, waiting to
    // be sent; a form submission waiting to be opened.
    let mut page_focus = false;
    let mut pending_input: Vec<Input> = Vec::new();
    let mut submitted: Option<Submission> = None;
    let mut queued_post: Option<Submission> = None;
    let mut laid_out_at = (0.0f32, 0.0f32);
    let mut scroll = 0.0f32;
    let mut ui = Ui {
        address: {
            let mut a = LineEdit::new(MAX_ADDRESS);
            a.set(start);
            a
        },
        editing: false,
        hover: None,
        panel,
        panel_scroll: 0.0,
        flash: None,
        menu: None,
        text_scale: load_scale("text-size", &TEXT_SCALES, DEFAULT_TEXT_SCALE),
        can_back: false,
        can_forward: false,
        loading: false,
    };
    shared_scale.store(ui.text_scale.to_bits(), Ordering::Relaxed);
    UI_SCALE.store(
        load_scale("interface-size", &UI_SCALES, 1.0).to_bits(),
        Ordering::Relaxed,
    );
    let theme = theme_file().and_then(|f| std::fs::read_to_string(f).ok());
    DARK_UI.store(
        theme.as_deref().map(str::trim) != Some("light"),
        Ordering::Relaxed,
    );
    // A menu item picked: the shortcut it presses, next frame.
    let mut menu_press: Option<(Key, bool, bool, bool)> = None;
    let mut quit = false;
    let mut cache = GlyphCache::default();
    let mut was_down = false;
    // Selected text on the page (where it started, where it ends now), and a
    // press on the page that becomes a click when you let go, unless you
    // dragged to select. A second press soon after selects a word.
    let mut sel: Option<(Pos, Pos)> = None;
    let mut ix = PageIndex::new(&rendered);
    let mut find: Option<Find> = None;
    let mut press: Option<(f32, f32)> = None;
    // Dragging the scrollbar: where on the thumb you grabbed it.
    let mut scroll_drag: Option<f32> = None;
    let mut dragged = false;
    let mut word_press = false;
    let mut last_press: Option<(Instant, f32, f32)> = None;
    let mut dirty = true;
    let mut frame = 0u32;
    let mut canvas = Canvas::new(w, h, Color::WHITE);

    // Tabs: the loop's variables above are the current tab's; the others
    // wait in `parked`. `order` is the tab strip, left to right.
    let mut next_id: TabId = 1;
    let mut cur_id: TabId = 0;
    let mut order: Vec<TabId> = vec![cur_id];
    let mut parked: Vec<Parked> = Vec::new();
    let mut was_mid = false;
    // The last background tab opened from the current one: the next goes
    // after it, so tabs open in the order you opened them (as in Firefox).
    let mut child_anchor: Option<TabId> = None;

    // Stop button for the load in flight.
    let mut stop = net::Cancel::new();
    let _ = jobs.send((
        cur_id,
        Job::Open {
            url: start.to_string(),
            from: None,
            w: w as f32,
            h: h as f32 - bar_h(),
            cancel: stop.clone(),
            post: None,
        },
    ));
    busy = Some(Some(start.to_string()));

    // Put the current tab's state away / make a parked tab current.
    macro_rules! park {
        () => {
            Parked {
                id: cur_id,
                tab: tab.take(),
                info: info.clone(),
                rendered: std::mem::replace(&mut rendered, blank_rendered()),
                history: std::mem::take(&mut history),
                forward: std::mem::take(&mut forward),
                scroll,
                busy: busy.take(),
                discard,
                queued: queued.take(),
                queued_post: queued_post.take(),
                laid_out_at,
                page_focus,
                pending_input: std::mem::take(&mut pending_input),
                stop: stop.clone(),
                address: if ui.editing {
                    info.url.clone()
                } else {
                    ui.address.text.clone()
                },
                script_hops,
            }
        };
    }
    macro_rules! unpark {
        ($p:expr) => {{
            let p: Parked = $p;
            cur_id = p.id;
            tab = p.tab;
            info = p.info;
            rendered = p.rendered;
            sel = None;
            ix = PageIndex::new(&rendered);
            find = None;
            history = p.history;
            forward = p.forward;
            scroll = p.scroll;
            busy = p.busy;
            discard = p.discard;
            queued = p.queued;
            queued_post = p.queued_post;
            laid_out_at = p.laid_out_at;
            page_focus = p.page_focus;
            pending_input = p.pending_input;
            stop = p.stop;
            ui.address.set(&p.address);
            ui.editing = false;
            ui.hover = None;
            ui.panel_scroll = 0.0;
            script_hops = p.script_hops;
            tiles = None;
            let title = if info.title.is_empty() {
                &info.url
            } else {
                &info.title
            };
            window.set_title(&format!("{title} — Brickle Browser"));
        }};
    }
    // Start loading `url` in a new tab (in the background unless `front`).
    macro_rules! open_tab {
        ($url:expr, $from:expr, $front:expr) => {{
            if order.len() >= MAX_TABS {
                ui.flash = Some(format!("Brickle keeps at most {MAX_TABS} tabs open"));
            } else {
                let id = next_id;
                next_id += 1;
                let url: String = $url;
                let cancel = net::Cancel::new();
                let _ = jobs.send((
                    id,
                    Job::Open {
                        url: url.clone(),
                        from: $from,
                        w: page_w_now(&ui, w),
                        h: h as f32 - bar_h(),
                        cancel: cancel.clone(),
                        post: None,
                    },
                ));
                let after = child_anchor.filter(|a| order.contains(a)).unwrap_or(cur_id);
                let pos = order
                    .iter()
                    .position(|&t| t == after)
                    .map(|i| i + 1)
                    .unwrap_or(order.len());
                order.insert(pos, id);
                let p = Parked::loading(id, &url, cancel);
                if $front {
                    child_anchor = None;
                    let old = park!();
                    parked.push(old);
                    unpark!(p);
                    if url == "about:home" {
                        ui.editing = true;
                        ui.address.set("");
                    }
                } else {
                    child_anchor = Some(id);
                    parked.push(p);
                }
            }
        }};
    }

    while window.is_open() {
        frame = frame.wrapping_add(1);
        let (nw, nh) = window.get_size();
        if (nw, nh) != (w, h) && nw > 0 && nh > 0 {
            (w, h) = (nw, nh);
            dirty = true;
        }
        let page_w = page_w_now(&ui, w);
        // Window pixels per interface unit, and where the page starts.
        let k = ui_scale();
        let bar = bar_h();
        let view_h = h as f32 - bar;

        // Results from the loader.
        while let Ok((id, done)) = done_rx.try_recv() {
            if id != cur_id {
                // A background tab finished something.
                if let Some(p) = parked.iter_mut().find(|p| p.id == id) {
                    p.finish(done, &jobs);
                }
                continue;
            }
            let was_open = matches!(busy, Some(Some(_)));
            busy = None;
            if discard && was_open {
                discard = false;
                continue; // stopped: keep showing the old page
            }
            discard = false;
            if let Some((click, link)) = &done.click {
                // Scripts and form controls had their say; now a form
                // submission, a script's navigation, or the link.
                page_focus = click.focus;
                if let Some(sub) = &click.submit {
                    submitted = Some(sub.clone());
                } else if let Some(to) = &click.navigate {
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
            if let Some(r) = done.rendered {
                rendered = r;
                sel = None;
                ix = PageIndex::new(&rendered);
                if let Some(f) = &mut find {
                    f.page_changed(&rendered);
                }
                tiles = None; // new content: repaint tiles on demand
                laid_out_at = (page_w, view_h);
            }
            if done.new_page {
                page_focus = false;
                pending_input.clear();
            }
            tab = Some(done.tab);
            if done.new_page {
                if !ui.editing {
                    ui.address.set(&info.url);
                }
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
                let _ = jobs.send((
                    cur_id,
                    Job::Open {
                        url: url.clone(),
                        from,
                        w: page_w,
                        h: view_h,
                        cancel: stop.clone(),
                        post: queued_post.take(),
                    },
                ));
                busy = Some(Some(url));
                scroll = 0.0;
            } else if let (Some(input), true) = (pending_input.first().cloned(), tab.is_some()) {
                // Typing goes to the page in order, a batch at a time.
                pending_input.remove(0);
                if let Some(t) = tab.take() {
                    let _ = jobs.send((
                        cur_id,
                        Job::Input {
                            tab: Box::new(t),
                            input,
                            link: None,
                            w: page_w,
                            h: view_h,
                        },
                    ));
                    busy = Some(None);
                }
            } else if need_layout || laid_out_at != (page_w, view_h) {
                need_layout = false;
                if let Some(t) = tab.take() {
                    let _ = jobs.send((
                        cur_id,
                        Job::Render {
                            tab: Box::new(t),
                            w: page_w,
                            h: view_h,
                        },
                    ));
                    busy = Some(None);
                }
            }
        }

        let max_scroll = (rendered.height - view_h).max(0.0);
        let mut navigate: Option<Entry> = None;
        let mut reload = false;
        let mut hist = 0i32;
        let mut tab_action: Option<TabAction> = None;

        // Keyboard
        let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
        let shift = window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);
        let alt = window.is_key_down(Key::LeftAlt) || window.is_key_down(Key::RightAlt);
        let chars: Vec<char> = std::mem::take(&mut *typed.lock().unwrap());
        // A menu item stands in for this frame's keys.
        let picked = menu_press.take();
        let (ctrl, shift, alt) = picked.map_or((ctrl, shift, alt), |(_, c, s, a)| (c, s, a));
        let keys = match picked {
            Some((k, ..)) => vec![k],
            None => window.get_keys_pressed(KeyRepeat::Yes),
        };
        for key in keys {
            if key == Key::Escape && ui.menu.is_some() {
                ui.menu = None;
                dirty = true;
                continue;
            }
            let sizing = match key {
                Key::Equal | Key::NumPadPlus => Some(1),
                Key::Minus | Key::NumPadMinus => Some(-1),
                Key::Key0 | Key::NumPad0 => Some(0),
                _ => None,
            };
            if let (true, Some(dir)) = (ctrl, sizing) {
                if alt {
                    // Interface size: the page is laid out again for the
                    // new space it gets.
                    let cur = ui_scale();
                    let next = if dir == 0 {
                        1.0
                    } else {
                        step_scale(&UI_SCALES, cur, dir)
                    };
                    if next != cur {
                        UI_SCALE.store(next.to_bits(), Ordering::Relaxed);
                        save_scale("interface-size", next);
                    }
                    ui.flash = Some(format!("Interface size {}%", (next * 100.0).round()));
                } else {
                    let next = if dir == 0 {
                        DEFAULT_TEXT_SCALE
                    } else {
                        step_scale(&TEXT_SCALES, ui.text_scale, dir)
                    };
                    if next != ui.text_scale {
                        ui.text_scale = next;
                        shared_scale.store(next.to_bits(), Ordering::Relaxed);
                        save_scale("text-size", next);
                        need_layout = true;
                        for p in &mut parked {
                            p.laid_out_at = (0.0, 0.0);
                        }
                    }
                    ui.flash = Some(format!("Text size {}%", (ui.text_scale * 100.0).round()));
                }
                dirty = true;
                continue;
            }
            if ctrl {
                let digit = [
                    Key::Key1,
                    Key::Key2,
                    Key::Key3,
                    Key::Key4,
                    Key::Key5,
                    Key::Key6,
                    Key::Key7,
                    Key::Key8,
                    Key::Key9,
                ]
                .iter()
                .position(|k| *k == key);
                let action = match key {
                    Key::T => Some(TabAction::New),
                    Key::W => Some(TabAction::Close(cur_id)),
                    Key::Tab => Some(TabAction::Next(if shift { -1 } else { 1 })),
                    Key::PageDown => Some(TabAction::Next(1)),
                    Key::PageUp => Some(TabAction::Next(-1)),
                    _ if digit == Some(8) => Some(TabAction::Switch(order.len().saturating_sub(1))),
                    _ => digit.map(TabAction::Switch),
                };
                if action.is_some() {
                    tab_action = action;
                    continue;
                }
            }
            if ctrl && key == Key::F {
                let f = find.get_or_insert_with(Find::new);
                f.query.select_all();
                ui.editing = false;
                page_focus = false;
                dirty = true;
                continue;
            }
            if let (Some(f), true) = (&mut find, key == Key::F3 || (ctrl && key == Key::G)) {
                f.step(shift);
                if let Some(y) = f.scroll_to(&rendered, scroll, view_h, max_scroll) {
                    scroll = y;
                }
                dirty = true;
                continue;
            }
            // Paste into the address bar, the find bar or the focused form field.
            if (ctrl && key == Key::V) || (shift && key == Key::Insert) {
                if !ui.editing && !page_focus && find.is_none() {
                    continue;
                }
                match clip.paste() {
                    Some(t) if !ui.editing && find.is_some() => {
                        let line: String = t.trim().chars().filter(|c| !c.is_control()).collect();
                        if let Some(f) = &mut find {
                            f.query.insert(&line);
                            f.search(&rendered);
                            if let Some(y) = f.scroll_to(&rendered, scroll, view_h, max_scroll) {
                                scroll = y;
                            }
                        }
                    }
                    Some(t) if ui.editing => {
                        // One line: newlines and tabs are dropped.
                        let line: String = t.trim().chars().filter(|c| !c.is_control()).collect();
                        ui.address.insert(&line);
                    }
                    // Single-line fields drop newlines themselves.
                    Some(t) if !t.is_empty() => pending_input.push(Input::Type(t)),
                    Some(_) => {}
                    None => ui.flash = Some("Couldn't read the clipboard".into()),
                }
                dirty = true;
                continue;
            }
            // Copy the selected text, or else the address: what you're
            // typing, or the page's.
            if ctrl && key == Key::C && !page_focus {
                let selected = sel
                    .filter(|_| !ui.editing)
                    .map(|(a, b)| select::text(&rendered.texts, a, b))
                    .filter(|t| !t.is_empty());
                let (what, done) = match &selected {
                    Some(t) => (t.as_str(), "Copied"),
                    None if ui.editing => match ui.address.selected() {
                        Some(part) => (part, "Copied"),
                        None => (ui.address.text.as_str(), "Copied the address"),
                    },
                    None => (info.url.as_str(), "Copied the address"),
                };
                ui.flash = Some(if clip.copy(what) {
                    done.into()
                } else {
                    "Couldn't reach the clipboard".into()
                });
                dirty = true;
                continue;
            }
            if ctrl && key == Key::A && !ui.editing && !page_focus {
                sel = select::all(&rendered.texts);
                dirty = true;
                continue;
            }
            if ui.editing {
                let a = &mut ui.address;
                let end = a.text.chars().count();
                match key {
                    Key::Enter | Key::NumPadEnter => {
                        navigate = Some((net::address_or_search(&a.text), None));
                        ui.editing = false;
                    }
                    Key::Escape => {
                        ui.editing = false;
                        a.set(&info.url);
                    }
                    Key::Backspace => a.backspace(ctrl),
                    Key::Delete => a.delete(ctrl),
                    Key::Left => a.left(ctrl, shift),
                    Key::Right => a.right(ctrl, shift),
                    Key::Home => a.move_to(0, shift),
                    Key::End => a.move_to(end, shift),
                    Key::A if ctrl => a.select_all(),
                    Key::X if ctrl => {
                        if let Some(cut) = a.cut() {
                            clip.copy(&cut);
                        }
                    }
                    _ => {}
                }
                dirty = true;
                continue;
            }
            if let Some(f) = &mut find {
                let q = &mut f.query;
                let end = q.text.chars().count();
                let before = q.text.clone();
                let mut handled = true;
                match key {
                    Key::Enter | Key::NumPadEnter => f.step(shift),
                    Key::Escape => {
                        find = None;
                        dirty = true;
                        continue;
                    }
                    Key::Backspace => q.backspace(ctrl),
                    Key::Delete => q.delete(ctrl),
                    Key::Left => q.left(ctrl, shift),
                    Key::Right => q.right(ctrl, shift),
                    Key::Home => q.move_to(0, shift),
                    Key::End => q.move_to(end, shift),
                    Key::A if ctrl => q.select_all(),
                    // Anything else (scrolling, shortcuts) works as usual.
                    _ => handled = false,
                }
                if handled {
                    if f.query.text != before {
                        f.search(&rendered);
                    }
                    if let Some(y) = f.scroll_to(&rendered, scroll, view_h, max_scroll) {
                        scroll = y;
                    }
                    dirty = true;
                    continue;
                }
                if key == Key::Space || key == Key::Backspace {
                    continue; // typed into the bar, not a scroll or "back"
                }
            }
            if page_focus {
                // A form field has the keyboard.
                let k = match key {
                    Key::Backspace => Some(PageKey::Backspace),
                    Key::Enter | Key::NumPadEnter => Some(PageKey::Enter),
                    Key::Tab => Some(PageKey::Tab),
                    Key::Escape => Some(PageKey::Escape),
                    _ => None,
                };
                if let Some(k) = k {
                    pending_input.push(Input::Key(k));
                    if k == PageKey::Escape {
                        page_focus = false;
                    }
                    continue;
                }
                if key == Key::Space {
                    continue; // typed as a character, not a scroll
                }
            }
            let before = scroll;
            match key {
                Key::Down => scroll += 40.0 * ui.text_scale,
                Key::Up => scroll -= 40.0 * ui.text_scale,
                Key::PageDown | Key::Space => scroll += view_h * 0.9,
                Key::PageUp => scroll -= view_h * 0.9,
                Key::Home => scroll = 0.0,
                Key::End => scroll = max_scroll,
                Key::L if ctrl => {
                    // The address, selected: typing replaces it.
                    ui.editing = true;
                    ui.address.set(&info.url);
                    ui.address.select_all();
                    dirty = true;
                }
                Key::R if ctrl => reload = true,
                Key::Q if ctrl => quit = true,
                Key::D if ctrl && shift => {
                    let dark = !DARK_UI.load(Ordering::Relaxed);
                    DARK_UI.store(dark, Ordering::Relaxed);
                    if let Some(f) = theme_file() {
                        let _ = f.parent().map(std::fs::create_dir_all);
                        let _ = std::fs::write(f, if dark { "dark\n" } else { "light\n" });
                    }
                    ui.flash = Some(format!("{} interface", if dark { "Dark" } else { "Light" }));
                    dirty = true;
                }
                Key::F5 => reload = true,
                Key::F12 => ui.panel = !ui.panel,
                Key::E if ctrl && shift => ui.panel = !ui.panel,
                Key::Escape if matches!(busy, Some(Some(_))) => {
                    // Kills the renderer and abandons network requests, so
                    // the loader is free again almost at once.
                    stop.cancel();
                    discard = true;
                    queued = None;
                    ui.address.set(&info.url);
                    ui.flash = Some("Stopped".into());
                    dirty = true;
                }
                Key::Escape if sel.is_some() => {
                    sel = None;
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
        if quit {
            break;
        }
        if ui.editing {
            let typed: String = chars.into_iter().filter(|c| !c.is_control()).collect();
            if !typed.is_empty() {
                ui.address.insert(&typed);
                dirty = true;
            }
        } else if let Some(f) = &mut find {
            let typed: String = chars.into_iter().filter(|c| !c.is_control()).collect();
            if !typed.is_empty() {
                f.query.insert(&typed);
                f.search(&rendered);
                if let Some(y) = f.scroll_to(&rendered, scroll, view_h, max_scroll) {
                    scroll = y.clamp(0.0, max_scroll);
                }
                dirty = true;
            }
        } else if page_focus {
            let text: String = chars.into_iter().filter(|c| !c.is_control()).collect();
            if !text.is_empty() {
                // Consecutive characters travel together.
                match pending_input.last_mut() {
                    Some(Input::Type(t)) if t.len() < 4096 => t.push_str(&text),
                    _ => pending_input.push(Input::Type(text)),
                }
            }
        }

        // Mouse
        let mouse = window.get_mouse_pos(MouseMode::Discard);
        let over_panel = ui.panel && mouse.is_some_and(|(mx, my)| mx >= page_w && my > bar);
        if let Some((_, dy)) = window.get_scroll_wheel() {
            // About three lines a notch (X11 reports 1 per notch), more
            // with bigger text.
            if over_panel {
                ui.panel_scroll = (ui.panel_scroll - dy * WHEEL_STEP).max(0.0);
            } else {
                scroll -= dy * WHEEL_STEP * ui.text_scale;
            }
            dirty = true;
        }
        scroll = scroll.clamp(0.0, max_scroll);
        let badge = badge_rect(&info.log, w as f32 / ui_scale());
        let down = window.get_mouse_down(MouseButton::Left);
        if let Some((mx, my)) = mouse {
            // The pointer in interface units, for the bars and panels.
            let (lx, ly) = (mx / k, my / k);
            let row = if over_panel {
                panel_row(&info.log, ly, ui.panel_scroll)
            } else {
                None
            };
            let button = if over_panel && net::scheme(&info.url).starts_with("http") {
                site_button_at(page_w / k, lx, ly)
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
            } else if let Some(t) = text_size_control_at(w as f32 / k, lx, ly) {
                Some(
                    match t {
                        -1 => "Smaller text (Ctrl+-)",
                        1 => "Bigger text (Ctrl+=)",
                        _ => "Back to the normal text size (Ctrl+0)",
                    }
                    .into(),
                )
            } else if let Some(i) = nav_button_at(lx, ly) {
                Some(
                    match i {
                        0 => "Back (Alt+Left)",
                        1 => "Forward (Alt+Right)",
                        _ if matches!(busy, Some(Some(_))) => "Stop loading (Esc)",
                        _ => "Reload (Ctrl+R)",
                    }
                    .into(),
                )
            } else if badge.contains(lx, ly) {
                Some("Show every request this page made (F12)".into())
            } else if my > bar && mx < page_w {
                ix.link_at_window(&rendered, mx, my, scroll)
                    .map(|s| s.to_string())
            } else {
                None
            };
            if hover != ui.hover {
                ui.hover = hover;
                dirty = true;
            }
            // With a menu open, pointing at another menu's title opens it.
            if let (Some(m), Some(t)) = (ui.menu, menu_title_at(lx, ly)) {
                if m != t {
                    ui.menu = Some(t);
                    dirty = true;
                }
            }
            if ui.menu.is_some() {
                dirty = true; // the item under the pointer is highlighted
            }
            let mid = window.get_mouse_down(MouseButton::Middle);
            if mid && !was_mid && my > bar && mx < page_w {
                if let Some(l) = ix.link_at_window(&rendered, mx, my, scroll) {
                    let url = net::resolve(&info.url, l);
                    if matches!(net::scheme(&url).as_str(), "http" | "https" | "file") {
                        tab_action = Some(TabAction::Background(url));
                    }
                }
            }
            was_mid = mid;
            let link_under = (my > bar && mx < page_w)
                .then(|| {
                    ix.link_at_window(&rendered, mx, my, scroll)
                        .map(|l| net::resolve(&info.url, l))
                })
                .flatten();
            if down
                && !was_down
                && ctrl
                && link_under
                    .as_deref()
                    .is_some_and(|u| net::scheme(u).starts_with("http"))
            {
                tab_action = Some(TabAction::Background(link_under.clone().unwrap_or_default()));
            } else if down && !was_down && ui.menu.is_some() {
                // A click with a menu open picks an item, or just closes it.
                let m = ui.menu.unwrap_or(0);
                if let Some(i) = menu_item_at(m, lx, ly) {
                    menu_press = Some(MENUS[m].1[i].press);
                }
                ui.menu = menu_title_at(lx, ly).filter(|&t| t != m);
                dirty = true;
            } else if down && !was_down {
                ui.flash = None;
                if ly < MENU_H {
                    if let Some(t) = menu_title_at(lx, ly) {
                        ui.menu = Some(t);
                        if ui.editing {
                            ui.editing = false;
                            ui.address.set(&info.url);
                        }
                    } else if let Some(t) = text_size_control_at(w as f32 / k, lx, ly) {
                        menu_press = Some(match t {
                            -1 => (Key::Minus, true, false, false),
                            1 => (Key::Equal, true, false, false),
                            _ => (Key::Key0, true, false, false),
                        });
                    }
                } else if ly < TABS_Y + TABS_H {
                    let (rects, plus) = tab_rects(order.len(), w as f32 / k);
                    if plus.contains(lx, ly) {
                        tab_action = Some(TabAction::New);
                    } else if let Some(i) = rects.iter().position(|(r, _)| r.contains(lx, ly)) {
                        tab_action = Some(if rects[i].1.contains(lx, ly) {
                            TabAction::Close(order[i])
                        } else {
                            TabAction::Switch(i)
                        });
                    }
                } else if let Some(i) = nav_button_at(lx, ly) {
                    match i {
                        0 => hist = -1,
                        1 => hist = 1,
                        _ if matches!(busy, Some(Some(_))) => {
                            menu_press = Some((Key::Escape, false, false, false));
                        }
                        _ => reload = true,
                    }
                } else if badge.contains(lx, ly) {
                    ui.panel = !ui.panel;
                } else if my < bar {
                    if ui.editing {
                        // Already typing: put the cursor where you clicked.
                        let at = address_index(&ui.address, badge.x - FIELD_X - 30.0, lx - FIELD_X - 10.0);
                        ui.address.move_to(at, shift);
                    } else {
                        ui.editing = true;
                        ui.address.set(&info.url);
                        ui.address.select_all();
                    }
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
                } else if let Some((y, h)) = scrollbar_thumb(scroll, max_scroll, view_h, k)
                    .filter(|_| my > bar && mx >= page_w - SCROLLBAR_W * k && mx < page_w)
                {
                    // On the scrollbar: drag the thumb from where you
                    // grabbed it; off it, the thumb jumps there first.
                    let at = my - bar;
                    let grab = if (y..y + h).contains(&at) { at - y } else { h / 2.0 };
                    scroll_drag = Some(grab);
                    scroll = scroll_for_thumb(at - grab, max_scroll, view_h, h);
                } else if my > bar {
                    // The page gets the click when you let go (below), so
                    // that dragging can select text instead.
                    press = Some((mx, my));
                    dragged = false;
                    let double = last_press.is_some_and(|(t, x, y)| {
                        t.elapsed().as_millis() < 400 && (mx - x).abs() + (my - y).abs() < 4.0
                    });
                    last_press = if double {
                        None
                    } else {
                        Some((Instant::now(), mx, my))
                    };
                    word_press = double;
                    sel = match select::hit(&rendered.texts, &ix.text, mx, my - bar + scroll) {
                        Some(p) if double => select::word(&rendered.texts, p),
                        Some(p) => Some((p, p)),
                        None => None,
                    };
                    if ui.editing {
                        ui.editing = false;
                        ui.address.set(&info.url);
                    }
                } else if ui.editing {
                    ui.editing = false;
                    ui.address.set(&info.url);
                }
                dirty = true;
            }
        }
        if let Some(grab) = scroll_drag {
            match (down, mouse, scrollbar_thumb(scroll, max_scroll, view_h, k)) {
                (true, Some((_, my)), Some((_, h))) => {
                    let to = scroll_for_thumb(my - bar - grab, max_scroll, view_h, h);
                    if to != scroll {
                        scroll = to;
                        dirty = true;
                    }
                }
                (true, None, _) => {}
                _ => scroll_drag = None,
            }
        }
        // A press on the page: dragging selects text, letting go without
        // dragging clicks where it was pressed.
        if let Some((px, py)) = press {
            if down {
                if let Some((mx, my)) = mouse {
                    if (mx - px).abs() + (my - py).abs() > 4.0 {
                        dragged = true;
                    }
                    if dragged && !word_press {
                        let to = select::hit(&rendered.texts, &ix.text, mx, my - bar + scroll);
                        if let (Some((from, _)), Some(to)) = (sel, to) {
                            if sel != Some((from, to)) {
                                sel = Some((from, to));
                                dirty = true;
                            }
                        }
                    }
                } else {
                    // Left the window: letting go out there isn't a click.
                    dragged = true;
                }
            } else {
                press = None;
                if !dragged && !word_press {
                    sel = None;
                    let (mx, my) = (px, py);
                    if busy.is_none() && tab.is_some() && info.interstitial.is_none() {
                        // The page sees the click first (form controls, scripts);
                        // the link is followed after, unless a script prevents it.
                        let link = ix
                            .link_at(&rendered, mx, my - bar + scroll)
                            .map(|l| l.to_string());
                        if let Some(t) = tab.take() {
                            let _ = jobs.send((
                                cur_id,
                                Job::Input {
                                    tab: Box::new(t),
                                    input: {
                                        let (cx, cy) = ix.page_point(mx, my, scroll);
                                        Input::Click(cx, cy)
                                    },
                                    link,
                                    w: page_w,
                                    h: view_h,
                                },
                            ));
                            busy = Some(None);
                        }
                    } else if let Some(l) = ix.link_at_window(&rendered, mx, my, scroll) {
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
                    }
                }
                dirty = true;
            }
        }
        was_down = down;

        if let Some(action) = tab_action {
            // What was decided for the old tab doesn't apply to the new one.
            navigate = None;
            reload = false;
            hist = 0;
            macro_rules! switch_to {
                ($id:expr) => {{
                    let id: TabId = $id;
                    if id != cur_id {
                        if let Some(i) = parked.iter().position(|p| p.id == id) {
                            let p = parked.remove(i);
                            let old = park!();
                            parked.push(old);
                            unpark!(p);
                            child_anchor = None;
                        }
                    }
                }};
            }
            match action {
                TabAction::New => open_tab!("about:home".to_string(), None, true),
                TabAction::Background(url) => {
                    open_tab!(url, Some(info.url.clone()), false);
                    ui.flash = Some(format!("Opened in a new tab ({} open)", order.len()));
                }
                TabAction::Switch(i) => {
                    if let Some(&id) = order.get(i) {
                        switch_to!(id);
                    }
                }
                TabAction::Next(d) => {
                    let i = order.iter().position(|&t| t == cur_id).unwrap_or(0) as i32;
                    let j = (i + d).rem_euclid(order.len() as i32) as usize;
                    switch_to!(order[j]);
                }
                TabAction::Close(id) => {
                    if order.len() == 1 {
                        // Closing the last tab leaves a fresh one.
                        open_tab!("about:home".to_string(), None, true);
                    } else if id == cur_id {
                        let i = order.iter().position(|&t| t == id).unwrap_or(0);
                        let j = if i + 1 < order.len() { i + 1 } else { i - 1 };
                        switch_to!(order[j]);
                    }
                    // Dropping a tab ends its renderer process.
                    if id != cur_id {
                        order.retain(|&t| t != id);
                        parked.retain(|p| p.id != id);
                    }
                }
            }
            dirty = true;
        }

        if let Some(sub) = submitted.take() {
            // Opened like a link from this page; POSTs carry their body.
            let from = Some(info.url.clone());
            if sub.post {
                queued_post = Some(sub.clone());
            }
            navigate = Some((sub.url, from));
        }
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
                ui.address.set(&url);
                queued = Some((url, from));
            }
        }
        if queued.is_some() && matches!(busy, Some(Some(_))) {
            stop.cancel();
            discard = true; // superseded by the newer navigation
        }

        let loading = matches!(busy, Some(Some(_))) || queued.is_some();
        ui.can_back = !history.is_empty();
        ui.can_forward = !forward.is_empty();
        ui.loading = loading;
        if dirty || loading {
            canvas = Canvas::new(w, h, rendered.background);
            if tiles.as_ref().is_some_and(|t| t.width() != page_w as usize) {
                tiles = None; // window resized: tiles are the wrong width
            }
            let tc = tiles.get_or_insert_with(|| {
                TileCache::new(
                    &ix.page,
                    &rendered.atlas,
                    page_w as usize,
                    rendered.height,
                    rendered.background,
                )
            });
            tc.draw(&mut canvas, &ix.page, &rendered.atlas, scroll, bar as usize);
            // Fixed and sticky boxes, where scrolling puts them.
            for (l, list) in &ix.layers {
                canvas.paint(list, &rendered.atlas, scroll - l.shift(scroll), bar);
            }
            if let Some(f) = &find {
                let shown = |(a, _): &(Pos, Pos)| {
                    rendered.texts.get(a.0).is_some_and(|r| {
                        r.rect.y + r.rect.h + 200.0 > scroll && r.rect.y < scroll + view_h + 200.0
                    })
                };
                for (k, m) in f.matches.iter().enumerate().filter(|(_, m)| shown(m)) {
                    let color = if k == f.current { CURRENT_MATCH } else { MATCH };
                    for r in select::highlights(&rendered.texts, &ix.text, m.0, m.1, scroll, scroll + view_h)
                    {
                        let r = Rect {
                            y: r.y - scroll + bar,
                            w: r.w.min(page_w - r.x),
                            ..r
                        };
                        canvas.fill(r, color, bar);
                    }
                }
            }
            if let Some((a, b)) = sel {
                for r in select::highlights(&rendered.texts, &ix.text, a, b, scroll, scroll + view_h) {
                    let r = Rect {
                        y: r.y - scroll + bar,
                        w: r.w.min(page_w - r.x),
                        ..r
                    };
                    canvas.fill(r, SELECTION, bar);
                }
            }
            // The bars and panels, in interface units and colours.
            canvas.scale = k;
            canvas.dark = DARK_UI.load(Ordering::Relaxed);
            draw_chrome(
                &mut canvas,
                &ui,
                &info.log,
                scroll,
                max_scroll,
                view_h / k,
                page_w / k,
                &mut cache,
            );
            let titles: Vec<(String, bool)> = order
                .iter()
                .map(|&id| {
                    if id == cur_id {
                        let t = if info.title.is_empty() {
                            &info.url
                        } else {
                            &info.title
                        };
                        (t.clone(), true)
                    } else {
                        let t = parked.iter().find(|p| p.id == id).map(|p| p.title().to_string());
                        (t.unwrap_or_default(), false)
                    }
                })
                .collect();
            draw_tabs(&mut canvas, &titles, &mut cache);
            draw_menu_bar(&mut canvas, &ui, &mut cache);
            if loading {
                // Indeterminate progress bar under the address bar.
                let x = (frame % 120) as f32 / 120.0 * (w as f32 / k + 200.0) - 200.0;
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
                    page_w / k,
                    &mut cache,
                );
            }
            draw_status(&mut canvas, &ui, page_w / k, &mut cache);
            if let Some(f) = &find {
                draw_find(&mut canvas, f, page_w / k, &mut cache);
            }
            if let Some(m) = ui.menu {
                draw_menu(&mut canvas, m, mouse.map(|(x, y)| (x / k, y / k)), &mut cache);
            }
            canvas.scale = 1.0;
            canvas.dark = false;
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

/// Something to do with tabs, decided during input and done after it.
#[derive(Debug, Clone, PartialEq)]
enum TabAction {
    New,
    Close(TabId),
    Switch(usize),
    Next(i32),
    /// Open a link in a new tab, in the background.
    Background(String),
}

/// A tab that isn't showing: everything the loop keeps for the current one.
struct Parked {
    id: TabId,
    tab: Option<Tab>,
    info: Info,
    rendered: Rendered,
    history: Vec<Entry>,
    forward: Vec<Entry>,
    scroll: f32,
    busy: Option<Option<String>>,
    discard: bool,
    queued: Option<Entry>,
    queued_post: Option<Submission>,
    laid_out_at: (f32, f32),
    page_focus: bool,
    pending_input: Vec<Input>,
    stop: net::Cancel,
    address: String,
    script_hops: u32,
}

impl Parked {
    /// A new tab whose first page is being loaded.
    fn loading(id: TabId, url: &str, stop: net::Cancel) -> Parked {
        Parked {
            id,
            tab: None,
            info: Info {
                url: url.to_string(),
                opener: None,
                title: String::new(),
                log: vec![],
                interstitial: None,
            },
            rendered: blank_rendered(),
            history: vec![],
            forward: vec![],
            scroll: 0.0,
            busy: Some(Some(url.to_string())),
            discard: false,
            queued: None,
            queued_post: None,
            laid_out_at: (0.0, 0.0),
            page_focus: false,
            pending_input: vec![],
            stop,
            address: url.to_string(),
            script_hops: 0,
        }
    }

    /// A loader result for this tab while it's in the background.
    fn finish(&mut self, done: Done, jobs: &Sender<(TabId, Job)>) {
        let was_open = matches!(self.busy, Some(Some(_)));
        self.busy = None;
        if self.discard && was_open {
            self.discard = false;
            return;
        }
        self.discard = false;
        self.info = Info::of(&done.tab);
        if let Some(r) = done.rendered {
            self.rendered = r;
        }
        if done.new_page {
            self.address = self.info.url.clone();
            self.scroll = 0.0;
            self.page_focus = false;
            // A script redirect (rare in the background): follow it once.
            if let (Some(to), 0) = (done.tab.script_navigate.clone(), self.script_hops) {
                self.script_hops += 1;
                self.stop = net::Cancel::new();
                let from = Some(self.info.url.clone());
                let _ = jobs.send((
                    self.id,
                    Job::Open {
                        url: to.clone(),
                        from,
                        w: self.laid_out_at.0.max(800.0),
                        h: self.laid_out_at.1.max(600.0),
                        cancel: self.stop.clone(),
                        post: None,
                    },
                ));
                self.busy = Some(Some(to));
            }
        }
        self.tab = Some(done.tab);
    }

    fn title(&self) -> &str {
        if self.info.title.is_empty() {
            &self.info.url
        } else {
            &self.info.title
        }
    }
}

fn blank_rendered() -> Rendered {
    Rendered {
        list: vec![],
        atlas: vec![],
        height: 0.0,
        background: Color::WHITE,
        links: vec![],
        texts: vec![],
        layers: vec![],
    }
}

fn page_w_now(ui: &Ui, w: usize) -> f32 {
    if ui.panel {
        (w as f32 - PANEL_W * ui_scale()).max(200.0).round()
    } else {
        w as f32
    }
}

/// Tab strip layout: each tab's rectangle and its close button, then the
/// new-tab button.
fn tab_rects(n: usize, w: f32) -> (Vec<(Rect, Rect)>, Rect) {
    let tw = ((w - 44.0) / n.max(1) as f32).clamp(60.0, 220.0);
    let mut v = Vec::new();
    for i in 0..n {
        let r = Rect {
            x: 6.0 + i as f32 * tw,
            y: TABS_Y + 4.0,
            w: tw - 4.0,
            h: TABS_H - 4.0,
        };
        let close = Rect {
            x: r.x + r.w - 20.0,
            y: r.y + 5.0,
            w: 16.0,
            h: 16.0,
        };
        v.push((r, close));
    }
    let plus = Rect {
        x: 6.0 + n as f32 * tw,
        y: TABS_Y + 6.0,
        w: 24.0,
        h: TABS_H - 8.0,
    };
    (v, plus)
}

fn draw_tabs(c: &mut Canvas, titles: &[(String, bool)], cache: &mut GlyphCache) {
    let w = c.size().0;
    let (rects, plus) = tab_rects(titles.len(), w);
    for ((title, current), (r, close)) in titles.iter().zip(&rects) {
        let bg = if *current {
            Color::WHITE
        } else {
            Color::rgb(0xdd, 0xe0, 0xe5)
        };
        c.fill(*r, bg, 0.0);
        let shown = fit(title, r.w - 34.0, SANS, 12.0);
        text(
            c,
            r.x + 8.0,
            r.y + 17.0,
            &shown,
            if *current { BOLD } else { SANS },
            12.0,
            DARK,
            cache,
            0.0,
        );
        text(
            c,
            close.x + 4.0,
            close.y + 12.0,
            "×",
            SANS,
            13.0,
            GRAY,
            cache,
            0.0,
        );
    }
    text(c, plus.x + 7.0, plus.y + 16.0, "+", BOLD, 16.0, GRAY, cache, 0.0);
}

/// The scrollbar thumb's top (from the top of the page area) and height,
/// in window pixels, if the page scrolls. `k` is the interface size.
fn scrollbar_thumb(scroll: f32, max_scroll: f32, view_h: f32, k: f32) -> Option<(f32, f32)> {
    if !(max_scroll > 0.0 && view_h > 0.0) {
        return None;
    }
    let h = (view_h / (view_h + max_scroll) * view_h)
        .max(24.0 * k)
        .min(view_h);
    Some(((scroll / max_scroll).clamp(0.0, 1.0) * (view_h - h), h))
}

/// The scroll position that puts the thumb's top at `top`.
fn scroll_for_thumb(top: f32, max_scroll: f32, view_h: f32, thumb_h: f32) -> f32 {
    let track = view_h - thumb_h;
    if track <= 0.0 {
        return 0.0;
    }
    (top / track * max_scroll).clamp(0.0, max_scroll)
}

/// The next size in `steps` up (`dir` 1) or down (-1) from `cur`.
fn step_scale(steps: &[f32], cur: f32, dir: i32) -> f32 {
    if dir > 0 {
        steps.iter().copied().find(|&s| s > cur + 0.001).unwrap_or(cur)
    } else {
        steps
            .iter()
            .rev()
            .copied()
            .find(|&s| s < cur - 0.001)
            .unwrap_or(cur)
    }
}

/// A size remembered in `~/.config/brickle/<name>` (`text-size`,
/// `interface-size`), kept within `steps`; `default` if there's none.
fn load_scale(name: &str, steps: &[f32], default: f32) -> f32 {
    brickle::network::config_dir()
        .and_then(|d| std::fs::read_to_string(d.join(name)).ok())
        .and_then(|t| t.trim().parse::<f32>().ok())
        .filter(|s| s.is_finite())
        .map_or(default, |s| s.clamp(steps[0], steps[steps.len() - 1]))
}

fn save_scale(name: &str, scale: f32) {
    if let Some(d) = brickle::network::config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(name), format!("{scale}\n"));
    }
}

/// The back, forward and reload/stop buttons, left of the address field.
fn nav_buttons() -> [Rect; 3] {
    std::array::from_fn(|i| Rect {
        x: 8.0 + i as f32 * (NAV_W + 2.0),
        y: TABS_Y + TABS_H + 6.0,
        w: NAV_W,
        h: ADDR_H - 12.0,
    })
}

fn nav_button_at(x: f32, y: f32) -> Option<usize> {
    nav_buttons().iter().position(|r| r.contains(x, y))
}

/// Where each menu's title is in the menu bar.
fn menu_titles() -> Vec<Rect> {
    let mut x = 6.0;
    MENUS
        .iter()
        .map(|(title, _)| {
            let w = font::measure(SANS, 13.0, title) + 20.0;
            let r = Rect {
                x,
                y: 0.0,
                w,
                h: MENU_H,
            };
            x += w;
            r
        })
        .collect()
}

fn menu_title_at(mx: f32, my: f32) -> Option<usize> {
    menu_titles().iter().position(|r| r.contains(mx, my))
}

/// The dropdown of menu `m`, and each of its items.
fn menu_rects(m: usize) -> (Rect, Vec<Rect>) {
    let x = menu_titles()[m].x;
    let items: Vec<Rect> = (0..MENUS[m].1.len())
        .map(|i| Rect {
            x: x + 4.0,
            y: MENU_H + 4.0 + i as f32 * MENU_ITEM_H,
            w: MENU_W - 8.0,
            h: MENU_ITEM_H,
        })
        .collect();
    let drop = Rect {
        x,
        y: MENU_H,
        w: MENU_W,
        h: items.len() as f32 * MENU_ITEM_H + 8.0,
    };
    (drop, items)
}

fn menu_item_at(m: usize, mx: f32, my: f32) -> Option<usize> {
    menu_rects(m).1.iter().position(|r| r.contains(mx, my))
}

/// The text size control at the right of the menu bar: smaller, the
/// current size (which resets it), bigger.
fn text_size_rects(w: f32) -> [Rect; 3] {
    let r = |x: f32, bw: f32| Rect {
        x,
        y: 3.0,
        w: bw,
        h: MENU_H - 6.0,
    };
    let plus = r(w - 38.0, 30.0);
    let label = r(plus.x - 56.0, 54.0);
    [r(label.x - 32.0, 30.0), label, plus]
}

/// -1 for smaller, 0 for the size itself, 1 for bigger.
fn text_size_control_at(w: f32, mx: f32, my: f32) -> Option<i32> {
    text_size_rects(w)
        .iter()
        .position(|r| r.contains(mx, my))
        .map(|i| i as i32 - 1)
}

fn draw_menu_bar(c: &mut Canvas, ui: &Ui, cache: &mut GlyphCache) {
    let w = c.size().0;
    c.fill(
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h: MENU_H,
        },
        Color::rgb(0xf7, 0xf8, 0xfa),
        0.0,
    );
    for (i, r) in menu_titles().iter().enumerate() {
        if ui.menu == Some(i) {
            c.fill(*r, Color::rgb(0xdb, 0xea, 0xfe), 0.0);
        }
        text(
            c,
            r.x + 10.0,
            r.y + 17.0,
            MENUS[i].0,
            SANS,
            13.0,
            DARK,
            cache,
            0.0,
        );
    }
    let [minus, label, plus] = text_size_rects(w);
    for r in [minus, plus] {
        c.fill(r, Color::WHITE, 0.0);
    }
    text(
        c,
        minus.x + 8.0,
        minus.y + 15.0,
        "A−",
        SANS,
        12.0,
        DARK,
        cache,
        0.0,
    );
    text(c, plus.x + 6.0, plus.y + 15.0, "A+", BOLD, 14.0, DARK, cache, 0.0);
    let pct = format!("{}%", (ui.text_scale * 100.0).round());
    let pw = font::measure(SANS, 12.0, &pct);
    text(
        c,
        label.x + (label.w - pw) / 2.0,
        label.y + 14.0,
        &pct,
        SANS,
        12.0,
        GRAY,
        cache,
        0.0,
    );
}

/// Menu `m`, open, over everything else.
fn draw_menu(c: &mut Canvas, m: usize, mouse: Option<(f32, f32)>, cache: &mut GlyphCache) {
    let (drop, items) = menu_rects(m);
    c.fill(
        drop.expand(Edges {
            t: 1.0,
            r: 1.0,
            b: 1.0,
            l: 1.0,
        }),
        LINE,
        0.0,
    );
    c.fill(drop, Color::WHITE, 0.0);
    for (it, r) in MENUS[m].1.iter().zip(&items) {
        if mouse.is_some_and(|(mx, my)| r.contains(mx, my)) {
            c.fill(*r, Color::rgb(0xdb, 0xea, 0xfe), 0.0);
        }
        text(c, r.x + 10.0, r.y + 17.0, it.label, SANS, 13.0, DARK, cache, 0.0);
        let kw = font::measure(SANS, 12.0, it.keys);
        text(
            c,
            r.x + r.w - 10.0 - kw,
            r.y + 17.0,
            it.keys,
            SANS,
            12.0,
            GRAY,
            cache,
            0.0,
        );
    }
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
        y: TABS_Y + TABS_H + 8.0,
        w: tw,
        h: ADDR_H - 16.0,
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
    let w = c.size().0;
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

    let enabled = [ui.can_back, ui.can_forward, true];
    let labels = ["←", "→", if ui.loading { "✕" } else { "↻" }];
    for ((r, label), on) in nav_buttons().iter().zip(labels).zip(enabled) {
        if on {
            c.fill(*r, Color::rgb(0xf7, 0xf8, 0xfa), 0.0);
        }
        let lw = font::measure(SANS, 18.0, label);
        let color = if on { DARK } else { LINE };
        text(
            c,
            r.x + (r.w - lw) / 2.0,
            r.y + 19.0,
            label,
            SANS,
            18.0,
            color,
            cache,
            0.0,
        );
    }
    let field = Rect {
        x: FIELD_X,
        y: TABS_Y + TABS_H + 6.0,
        w: badge.x - FIELD_X - 10.0,
        h: ADDR_H - 12.0,
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
        // The part of the text around the cursor that fits, the selection
        // behind it, and the cursor.
        let a = &ui.address;
        let avail = max_x - field.x - 10.0;
        let start = address_window(a, avail);
        let chars: Vec<char> = a.text.chars().collect();
        let x_of = |i: usize| {
            let s: String = chars[start..i.clamp(start, chars.len())].iter().collect();
            field.x + 10.0 + font::measure(SANS, 14.0, &s)
        };
        if let Some((lo, hi)) = a.selection() {
            let (x0, x1) = (x_of(lo), x_of(hi).min(max_x));
            c.fill(
                Rect {
                    x: x0,
                    y: field.y + 5.0,
                    w: (x1 - x0).max(0.0),
                    h: field.h - 10.0,
                },
                Color::rgb(0xbf, 0xdb, 0xfe),
                0.0,
            );
        }
        let shown: String = chars[start..].iter().collect();
        text(
            c,
            field.x + 10.0,
            base,
            &fit(&shown, avail, SANS, 14.0),
            SANS,
            14.0,
            DARK,
            cache,
            0.0,
        );
        c.fill(
            Rect {
                x: x_of(a.cursor).min(max_x),
                y: field.y + 5.0,
                w: 1.0,
                h: field.h - 10.0,
            },
            DARK,
            0.0,
        );
    } else {
        // Anti-spoofing: the real (punycode) host in bold, the rest dimmed,
        // and a warning on unencrypted pages.
        let mut x = field.x + 10.0;
        if net::scheme(&ui.address.text) == "http"
            && !net::is_local_host(&brickle::filter::host_of(&ui.address.text))
        {
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
        let (head, rest) = net::display_parts(&ui.address.text);
        let head = fit(&head, max_x - x, BOLD, 14.0);
        text(c, x, base, &head, BOLD, 14.0, DARK, cache, 0.0);
        x += font::measure(BOLD, 14.0, &head);
        let rest = fit(&rest, max_x - x, SANS, 14.0);
        text(c, x, base, &rest, SANS, 14.0, GRAY, cache, 0.0);
    }

    // `view_h` is in interface units here, `scroll` in pixels.
    if let Some((y, thumb)) = scrollbar_thumb(scroll, max_scroll, view_h * c.scale, c.scale) {
        let bar = Rect {
            x: page_w - SCROLLBAR_W + 2.0,
            y: BAR_H + y / c.scale,
            w: SCROLLBAR_W - 4.0,
            h: thumb / c.scale,
        };
        // Over the page, so in the page's colours, not the interface's.
        let dark = std::mem::replace(&mut c.dark, false);
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
        c.dark = dark;
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
    let (w, h) = (c.size().0, c.size().1);
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
            net::Dest::Xhr => "xhr",
            net::Dest::Image => "img",
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

/// The find bar, at the bottom right of the page: the query with its
/// cursor, and which match you're on.
fn draw_find(c: &mut Canvas, f: &Find, page_w: f32, cache: &mut GlyphCache) {
    let h = c.size().1;
    let bar = Rect {
        x: (page_w - FIND_W - 10.0).max(0.0),
        y: h - STATUS_H - FIND_H - 6.0,
        w: FIND_W.min(page_w),
        h: FIND_H,
    };
    c.fill(
        bar.expand(Edges {
            t: 1.0,
            r: 1.0,
            b: 1.0,
            l: 1.0,
        }),
        Color::rgb(0x3b, 0x82, 0xf6),
        BAR_H,
    );
    c.fill(bar, Color::WHITE, BAR_H);
    let base = bar.y + 20.0;
    text(c, bar.x + 8.0, base, "Find", BOLD, 13.0, GRAY, cache, BAR_H);
    let count = match (f.query.text.is_empty(), f.matches.len()) {
        (true, _) => String::new(),
        (false, 0) => "No matches".into(),
        (false, n) if n >= select::MAX_MATCHES => format!("{} of {n}+", f.current + 1),
        (false, n) => format!("{} of {n}", f.current + 1),
    };
    let count_w = font::measure(SANS, 12.0, &count);
    let color = if count == "No matches" { RED } else { GRAY };
    text(
        c,
        bar.x + bar.w - 8.0 - count_w,
        base,
        &count,
        SANS,
        12.0,
        color,
        cache,
        BAR_H,
    );
    let x0 = bar.x + 46.0;
    let avail = bar.w - 46.0 - count_w - 20.0;
    let q = &f.query;
    let start = address_window(q, avail);
    let chars: Vec<char> = q.text.chars().collect();
    let x_of = |i: usize| {
        let s: String = chars[start..i.clamp(start, chars.len())].iter().collect();
        x0 + font::measure(SANS, 14.0, &s)
    };
    if let Some((lo, hi)) = q.selection() {
        let (a, b) = (x_of(lo), x_of(hi).min(x0 + avail));
        c.fill(
            Rect {
                x: a,
                y: bar.y + 6.0,
                w: (b - a).max(0.0),
                h: bar.h - 12.0,
            },
            Color::rgb(0xbf, 0xdb, 0xfe),
            BAR_H,
        );
    }
    let shown: String = chars[start..].iter().collect();
    text(
        c,
        x0,
        base,
        &fit(&shown, avail, SANS, 14.0),
        SANS,
        14.0,
        DARK,
        cache,
        BAR_H,
    );
    c.fill(
        Rect {
            x: x_of(q.cursor).min(x0 + avail),
            y: bar.y + 6.0,
            w: 1.0,
            h: bar.h - 12.0,
        },
        DARK,
        BAR_H,
    );
}

fn draw_status(c: &mut Canvas, ui: &Ui, page_w: f32, cache: &mut GlyphCache) {
    let h = c.size().1;
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
/// The first character shown in an address bar `avail` px wide, so the
/// cursor stays in view.
fn address_window(a: &LineEdit, avail: f32) -> usize {
    let chars: Vec<char> = a.text.chars().collect();
    let mut start = 0;
    let cursor = a.cursor.min(chars.len());
    while start < cursor {
        let s: String = chars[start..cursor].iter().collect();
        if font::measure(SANS, 14.0, &s) <= avail - 4.0 {
            break;
        }
        start += 1;
    }
    start
}

/// The character position nearest `x` px into an address bar `avail` px
/// wide.
fn address_index(a: &LineEdit, avail: f32, x: f32) -> usize {
    let start = address_window(a, avail);
    let mut w = 0.0;
    for (i, c) in a.text.chars().enumerate().skip(start) {
        let cw = font::measure(SANS, 14.0, &c.to_string());
        if x < w + cw / 2.0 {
            return i;
        }
        w += cw;
    }
    a.text.chars().count()
}

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

#[cfg(test)]
mod clipboard_tests {
    use super::{clean_paste, Clip, MAX_PASTE};

    #[test]
    fn pasted_text_is_cleaned() {
        assert_eq!(clean_paste("a\r\nb\rc\n\td"), "a\nb\nc\n\td");
        assert_eq!(clean_paste("x\u{1b}[31my\u{7}"), "x[31my", "no escape sequences");
        let big = "é".repeat(MAX_PASTE);
        let p = clean_paste(&big);
        assert!(p.len() <= MAX_PASTE && p.chars().all(|c| c == 'é'));
    }

    /// Needs a display: `cargo test -- --ignored clipboard_round_trip`.
    #[test]
    #[ignore]
    fn clipboard_round_trip() {
        let mut clip = Clip(None);
        assert!(clip.copy("https://example.com/brickle"));
        assert_eq!(clip.paste().as_deref(), Some("https://example.com/brickle"));
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn text_size_steps_and_limits() {
        let step = |cur, dir| step_scale(&TEXT_SCALES, cur, dir);
        assert_eq!(step(1.25, 1), 1.5);
        assert_eq!(step(1.25, -1), 1.1);
        assert_eq!(step(3.0, 1), 3.0);
        assert_eq!(step(0.5, -1), 0.5);
        // A size from an edited settings file still steps sensibly.
        assert_eq!(step(1.3, 1), 1.5);
        assert_eq!(step(1.3, -1), 1.25);
        assert_eq!(step_scale(&UI_SCALES, 1.0, 1), 1.25);
    }

    #[test]
    fn scrollbar_maps_both_ways() {
        assert_eq!(scrollbar_thumb(0.0, 0.0, 500.0, 1.0), None, "nothing to scroll");
        // A page twice the window: the thumb is half the track.
        let (y, h) = scrollbar_thumb(0.0, 500.0, 500.0, 1.0).unwrap();
        assert_eq!((y, h), (0.0, 250.0));
        let (y, _) = scrollbar_thumb(500.0, 500.0, 500.0, 1.0).unwrap();
        assert_eq!(y, 250.0, "at the bottom");
        assert_eq!(scroll_for_thumb(125.0, 500.0, 500.0, 250.0), 250.0);
        assert_eq!(scroll_for_thumb(-50.0, 500.0, 500.0, 250.0), 0.0);
        assert_eq!(scroll_for_thumb(9e9, 500.0, 500.0, 250.0), 500.0);
        // A huge page still gets a thumb you can grab.
        let (_, h) = scrollbar_thumb(0.0, 1e7, 500.0, 2.0).unwrap();
        assert_eq!(h, 48.0);
    }

    #[test]
    fn menus_and_controls_dont_overlap() {
        let titles = menu_titles();
        assert_eq!(titles.len(), MENUS.len());
        for (i, t) in titles.iter().enumerate() {
            assert_eq!(menu_title_at(t.x + 2.0, 10.0), Some(i));
            let (drop, items) = menu_rects(i);
            assert!(
                drop.y >= MENU_H
                    && items
                        .iter()
                        .all(|r| r.y >= drop.y && r.y + r.h <= drop.y + drop.h)
            );
            assert_eq!(menu_item_at(i, items[0].x + 5.0, items[0].y + 5.0), Some(0));
        }
        let right = titles.last().map_or(0.0, |r| r.x + r.w);
        let [minus, label, plus] = text_size_rects(1100.0);
        assert!(right < minus.x && minus.x + minus.w <= label.x && label.x + label.w <= plus.x);
        assert_eq!(text_size_control_at(1100.0, plus.x + 3.0, 10.0), Some(1));
        assert_eq!(text_size_control_at(1100.0, label.x + 3.0, 10.0), Some(0));
        // The buttons come before the address field, in the address row.
        let nav = nav_buttons();
        assert!(nav[2].x + nav[2].w < FIELD_X && nav.iter().all(|r| r.y > TABS_Y + TABS_H));
        assert_eq!(nav_button_at(nav[0].x + 3.0, nav[0].y + 3.0), Some(0));
        // The tab strip is below the menu bar.
        let (tabs, plus_tab) = tab_rects(3, 1100.0);
        assert!(tabs.iter().all(|(r, _)| r.y >= MENU_H) && plus_tab.y >= MENU_H);
    }

    /// Paints the top of the window with the View menu open, to look at:
    /// `cargo test --bin brickle -- --ignored menu_bar_picture`.
    #[test]
    #[ignore]
    fn menu_bar_picture() {
        let k: f32 = std::env::var("MENU_SCALE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        let mut c = Canvas::new(1100, (260.0 * k) as usize, Color::WHITE);
        c.scale = k;
        c.dark = std::env::var("MENU_LIGHT").is_err();
        let mut cache = GlyphCache::default();
        let mut ui = Ui {
            address: LineEdit::new(64),
            editing: false,
            hover: None,
            panel: false,
            panel_scroll: 0.0,
            flash: None,
            menu: std::env::var("MENU_CLOSED").is_err().then_some(2),
            text_scale: DEFAULT_TEXT_SCALE,
            can_back: true,
            can_forward: false,
            loading: false,
        };
        ui.address.set("https://example.com/");
        let w = c.size().0;
        draw_chrome(&mut c, &ui, &[], 0.0, 0.0, 160.0, w, &mut cache);
        draw_tabs(
            &mut c,
            &[("Example Domain".into(), true), ("Docs".into(), false)],
            &mut cache,
        );
        draw_menu_bar(&mut c, &ui, &mut cache);
        if let Some(m) = ui.menu {
            let (_, items) = menu_rects(m);
            draw_menu(
                &mut c,
                m,
                Some((items[0].x + 20.0, items[0].y + 10.0)),
                &mut cache,
            );
        }
        c.write_png(&std::env::var("MENU_PNG").unwrap_or("menu.png".into()))
            .unwrap();
    }
}
