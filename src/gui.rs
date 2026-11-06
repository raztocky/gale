//! The desktop browser window: address bar, page, request panel.

use crate::describe;
use brickle::css::Color;
use brickle::font;
use brickle::forms::Submission;
use brickle::layout::{Edges, Rect};
use brickle::lineedit::LineEdit;
use brickle::net;
use brickle::network::{Action, LogEntry, Network, Outcome};
use brickle::page::{self, Key as PageKey, Rendered};
use brickle::paint::{self, Canvas, DisplayList, GlyphBitmap, GlyphCache, Layer, TileCache, TilePainter};
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
/// The menu, tab and address bars.
const TOP_H: f32 = MENU_H + TABS_H + ADDR_H;
/// The bookmarks toolbar, under them (View → Bookmarks toolbar).
const BOOK_H: f32 = 28.0;
static BOOKMARKS_BAR: AtomicBool = AtomicBool::new(true);
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
/// All the bars above the page, in interface units.
fn bars() -> f32 {
    TOP_H
        + if BOOKMARKS_BAR.load(Ordering::Relaxed) {
            BOOK_H
        } else {
            0.0
        }
}

fn bar_h() -> f32 {
    (bars() * ui_scale()).round()
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

const MENUS: [(&str, &[MenuItem]); 5] = [
    (
        "File",
        &[
            item("New tab", "Ctrl+T", (Key::T, true, false, false)),
            item("Close tab", "Ctrl+W", (Key::W, true, false, false)),
            item("Reopen closed tab", "Ctrl+Shift+T", (Key::T, true, true, false)),
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
    (
        "Bookmarks",
        &[
            item("Bookmark this page", "Ctrl+D", (Key::D, true, false, false)),
            item("Bookmarks toolbar", "Ctrl+Shift+B", (Key::B, true, true, false)),
        ],
    ),
];
/// Most tabs open at once (each has its own renderer process).
const MAX_TABS: usize = 30;
const STATUS_H: f32 = 22.0;
/// The requests panel's width, in interface units: drag its left edge.
static PANEL_W: AtomicU32 = AtomicU32::new(0x43c8_0000); // 400.0
const MIN_PANEL_W: f32 = 260.0;

fn panel_w() -> f32 {
    f32::from_bits(PANEL_W.load(Ordering::Relaxed))
}
const PANEL_HEAD: f32 = 116.0;
/// A request row's first line, and each line after it.
const ROW_LINE: f32 = 15.0;
const ROW_PAD: f32 = 8.0;
/// Most lines a request's address, or its notes, wrap onto.
const ROW_MAX_LINES: usize = 8;
/// Longest address, and longest paste, we take from the clipboard.
const MAX_ADDRESS: usize = 8 * 1024;
const MAX_PASTE: usize = 64 * 1024;

const SANS: usize = 0;
const BOLD: usize = 1;
const GRAY: Color = Color::rgb(0x6b, 0x72, 0x80);
const DARK: Color = Color::rgb(0x22, 0x22, 0x22);
const LINE: Color = Color::rgb(0xc8, 0xcc, 0xd2);
const CHROME: Color = Color::rgb(0xec, 0xee, 0xf1);
/// What a request was for, in the request panel.
const EXPLAIN: Color = Color::rgb(0x31, 0x2e, 0x81);
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
/// The "send the form again?" box.
const RESEND_W: f32 = 460.0;
const RESEND_H: f32 = 140.0;
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
/// A page in the back/forward history, and the form it answers if going
/// back to it means sending that form again.
type Visit = (Entry, Option<Submission>);

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
    /// Asking whether to send a form again: for a reload (0) or a step
    /// back (-1) or forward (1) onto a page that answered a form.
    resend: Option<i32>,
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
    resubmit: Option<Submission>,
    /// A note about the whole page for the request panel (fingerprinting).
    note: Option<String>,
}

impl Info {
    fn of(tab: &Tab) -> Info {
        Info {
            url: tab.url.clone(),
            opener: tab.opener.clone(),
            title: tab.title.clone(),
            log: tab.log.clone(),
            interstitial: tab.interstitial.clone(),
            resubmit: tab.resubmit.clone(),
            note: brickle::explain::fingerprinting(tab.read.iter().copied()),
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
                        tab.title_px = tab_title_px();
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
                        tab.title_px = tab_title_px();
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
                        tab.title_px = tab_title_px();
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

    let mut history: Vec<Visit> = Vec::new();
    let mut forward: Vec<Visit> = Vec::new();
    // Set when you said yes to sending a form again: the next reload or
    // history step goes ahead with it.
    let mut resend_ok = false;
    let mut tab: Option<Tab> = None;
    let mut info = Info {
        url: start.to_string(),
        opener: None,
        title: String::new(),
        log: vec![],
        interstitial: None,
        resubmit: None,
        note: None,
    };
    // Painted tiles of the current page; rebuilt when the page changes.
    // Tiles are painted on their own threads, never on this one.
    let mut tiles: Option<TileCache> = None;
    let painter = TilePainter::new(
        std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .clamp(1, 3),
    );
    let mut rendered = Rendered {
        list: vec![],
        atlas: vec![],
        height: 0.0,
        background: Color::WHITE,
        links: vec![],
        texts: vec![],
        layers: vec![],
        scrollers: vec![],
        title: None,
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
        resend: None,
        loading: false,
    };
    shared_scale.store(ui.text_scale.to_bits(), Ordering::Relaxed);
    BOOKMARKS_BAR.store(
        brickle::network::config_dir()
            .and_then(|d| std::fs::read_to_string(d.join("bookmarks-toolbar")).ok())
            .is_none_or(|t| t.trim() != "off"),
        Ordering::Relaxed,
    );
    PANEL_W.store(
        load_scale("panel-width", &[MIN_PANEL_W, 2000.0], 400.0).to_bits(),
        Ordering::Relaxed,
    );
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
    // Dragging a box's scrollbar, and the box you last clicked in (keys
    // scroll it), with the page it was on.
    let mut box_drag: Option<BoxDrag> = None;
    let mut key_box: Option<(u32, String)> = None;
    // Dragging the requests panel's left edge to resize it.
    let mut panel_drag = false;
    // The page as it was painted before its latest change, shown until the
    // new tiles are painted.
    let mut stale_tiles: Option<TileCache> = None;
    let mut bookmarks = brickle::bookmarks::Bookmarks::load(brickle::network::data_dir().as_deref());
    let mut was_right = false;
    let mut resize_cursor = false;
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
    // Tabs you closed, most recent last, for Ctrl+Shift+T. Only in memory:
    // gone when Brickle quits.
    let mut closed: Vec<ClosedTab> = Vec::new();

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
                parked_at: Instant::now(),
                discarded: false,
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
            stale_tiles = None;
            if p.discarded && busy.is_none() {
                // Its renderer was let go: load it again, from where it
                // was opened, so the same checks apply.
                queued = Some((info.url.clone(), info.opener.clone()));
            }
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
                if let Some(t) = &click.copied {
                    ui.flash = Some(if clip.copy(t) {
                        "Copied".into()
                    } else {
                        "Couldn't reach the clipboard".into()
                    });
                }
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
                // New content: repaint tiles on demand. The same page
                // changed (a click, a script) keeps showing as it was
                // until the new tiles arrive, rather than flashing blank.
                let old = tiles.take();
                stale_tiles = if done.new_page { None } else { old };
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
        // Let long-unused background tabs' renderers go (checked about
        // once a second).
        if frame.is_multiple_of(60) {
            let state: Vec<_> = parked
                .iter()
                .map(|p| {
                    let can =
                        p.tab.is_some() && p.busy.is_none() && !p.page_focus && p.pending_input.is_empty()
                        // Reloading it would mean sending its form again.
                        && p.info.resubmit.is_none();
                    (p.parked_at.elapsed(), can)
                })
                .collect();
            for i in tabs_to_discard(&state) {
                let p = &mut parked[i];
                p.tab = None; // ends its renderer process
                p.discarded = true;
            }
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
            if let Some(dir) = ui.resend {
                match key {
                    Key::Enter | Key::NumPadEnter => {
                        ui.resend = None;
                        resend_ok = true;
                        if dir == 0 {
                            reload = true;
                        } else {
                            hist = dir;
                        }
                        dirty = true;
                        continue;
                    }
                    Key::Escape => {
                        ui.resend = None;
                        dirty = true;
                        continue;
                    }
                    _ => {}
                }
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
                    Key::T if shift => Some(TabAction::Reopen),
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
                        let to = net::address_or_search(&a.text);
                        if !to.is_empty() {
                            navigate = Some((to, None));
                        }
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
                // A form control has the keyboard.
                let (select, word) = (shift, ctrl);
                let k = match key {
                    Key::Backspace => Some(PageKey::Backspace { word }),
                    Key::Delete => Some(PageKey::Delete { word }),
                    Key::Enter | Key::NumPadEnter => Some(PageKey::Enter),
                    Key::Tab => Some(PageKey::Tab),
                    Key::Escape => Some(PageKey::Escape),
                    Key::Left if !alt => Some(PageKey::Left { select, word }),
                    Key::Right if !alt => Some(PageKey::Right { select, word }),
                    Key::Home => Some(PageKey::Home { select }),
                    Key::End => Some(PageKey::End { select }),
                    Key::Up => Some(PageKey::Up { select }),
                    Key::Down => Some(PageKey::Down { select }),
                    Key::A if ctrl => Some(PageKey::SelectAll),
                    Key::C | Key::Insert if ctrl => Some(PageKey::Copy),
                    Key::X if ctrl => Some(PageKey::Cut),
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
            // A box under the pointer, or the one you clicked in, scrolls
            // first while it can.
            let line = 40.0 * ui.text_scale;
            let box_key = match key {
                Key::Down => Some((0.0, 1.0, 0)),
                Key::Up => Some((0.0, -1.0, 0)),
                Key::Right if !alt && !ctrl => Some((1.0, 0.0, 0)),
                Key::Left if !alt && !ctrl => Some((-1.0, 0.0, 0)),
                Key::Space if shift => Some((0.0, -1.0, 1)),
                Key::PageDown | Key::Space => Some((0.0, 1.0, 1)),
                Key::PageUp => Some((0.0, -1.0, 1)),
                Key::Home if !ctrl => Some((0.0, -1.0, 2)),
                Key::End if !ctrl => Some((0.0, 1.0, 2)),
                _ => None,
            };
            if let (Some((dx, dy, how)), true) = (box_key, tab.is_some() && info.interstitial.is_none()) {
                let pointer = window
                    .get_mouse_pos(MouseMode::Discard)
                    .filter(|&(mx, my)| my > bar && mx < page_w)
                    .map(|(mx, my)| (mx, my - bar + scroll));
                let clicked = key_box
                    .as_ref()
                    .filter(|(_, u)| *u == info.url)
                    .map(|(id, _)| *id);
                if let Some(uid) = box_for_key(&rendered.scrollers, pointer, clicked, scroll, (dx, dy)) {
                    let h = rendered
                        .scrollers
                        .iter()
                        .find(|s| s.uid == uid)
                        .map_or(0.0, |s| s.rect.h);
                    let amount = match how {
                        0 => line,
                        1 => (h * 0.9).max(line),
                        _ => 1e7,
                    };
                    queue_box_scroll(&mut pending_input, uid, dx * amount, dy * amount);
                    continue;
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
                Key::D if ctrl => {
                    ui.flash = Some(match bookmarks.toggle(&info.url, &info.title) {
                        Ok(m) | Err(m) => m,
                    });
                    dirty = true;
                }
                Key::B if ctrl && shift => {
                    let on = !BOOKMARKS_BAR.load(Ordering::Relaxed);
                    BOOKMARKS_BAR.store(on, Ordering::Relaxed);
                    if let Some(d) = brickle::network::config_dir() {
                        let _ = std::fs::create_dir_all(&d);
                        let _ =
                            std::fs::write(d.join("bookmarks-toolbar"), if on { "on\n" } else { "off\n" });
                    }
                    tiles = None;
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
        if let Some((dx, dy)) = window.get_scroll_wheel() {
            // About three lines a notch (X11 reports 1 per notch), more
            // with bigger text.
            if over_panel {
                let rows = panel_rows(
                    &info.log,
                    &info.url,
                    info.note.as_deref(),
                    (w as f32 - page_w) / k,
                );
                let total = rows.rows.last().map_or(rows.note_h, |r| r.y + r.h);
                let room = view_h / k - PANEL_HEAD;
                ui.panel_scroll = (ui.panel_scroll - dy * WHEEL_STEP).clamp(0.0, (total - room).max(0.0));
            } else {
                // A box under the pointer that can scroll that way takes it
                // first (Shift turns the wheel sideways).
                let step = WHEEL_STEP * ui.text_scale;
                let (mut sx, mut sy) = (-dx * step, -dy * step);
                if shift && sx == 0.0 {
                    (sx, sy) = (sy, 0.0);
                }
                // (Boxes are compared in page space, layers moved for scrolling.)
                let inner = mouse
                    .filter(|&(_, my)| my > bar && tab.is_some() && info.interstitial.is_none())
                    .and_then(|(mx, my)| {
                        page::scroller_at(&rendered.scrollers, mx, my - bar + scroll, scroll, sx, sy)
                    });
                if let Some(sc) = inner {
                    queue_box_scroll(&mut pending_input, sc.uid, sx, sy);
                } else {
                    scroll -= dy * step;
                }
            }
            dirty = true;
        }
        scroll = scroll.clamp(0.0, max_scroll);
        let badge = badge_rect(&info.log, w as f32 / ui_scale());
        let down = window.get_mouse_down(MouseButton::Left);
        // The panel's left edge, which you can drag to resize it.
        let on_panel_edge =
            ui.panel && mouse.is_some_and(|(mx, my)| my > bar && mx >= page_w - 3.0 && mx <= page_w + 5.0);
        if (on_panel_edge || panel_drag) != resize_cursor {
            resize_cursor = on_panel_edge || panel_drag;
            window.set_cursor_style(if resize_cursor {
                minifb::CursorStyle::ResizeLeftRight
            } else {
                minifb::CursorStyle::Arrow
            });
        }
        if let Some((mx, my)) = mouse {
            // The pointer in interface units, for the bars and panels.
            let (lx, ly) = (mx / k, my / k);
            let row = if over_panel {
                panel_row(
                    &info.log,
                    &info.url,
                    info.note.as_deref(),
                    ly,
                    ui.panel_scroll,
                    (w as f32 - page_w) / k,
                )
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
            } else if let Some(b) = bookmark_at(&bookmarks.list, w as f32 / k, lx, ly) {
                Some(format!("{}  ·  right-click to remove", bookmarks.list[b].url))
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
            if mid && !was_mid {
                if let Some(b) = bookmark_at(&bookmarks.list, w as f32 / k, lx, ly) {
                    tab_action = Some(TabAction::Background(bookmarks.list[b].url.clone()));
                }
            }
            was_mid = mid;
            let right = window.get_mouse_down(MouseButton::Right);
            if right && !was_right {
                if let Some(b) = bookmark_at(&bookmarks.list, w as f32 / k, lx, ly) {
                    ui.flash = Some(match bookmarks.remove(b) {
                        Ok(m) | Err(m) => m,
                    });
                    dirty = true;
                }
            }
            was_right = right;
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
            } else if let (true, Some(dir), Some(i)) = (
                down && !was_down,
                ui.resend,
                resend_buttons(page_w / k, bar / k)
                    .iter()
                    .position(|r| r.contains(lx, ly)),
            ) {
                ui.resend = None;
                if i == 0 {
                    resend_ok = true;
                    if dir == 0 {
                        reload = true;
                    } else {
                        hist = dir;
                    }
                }
                dirty = true;
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
                if on_panel_edge {
                    panel_drag = true;
                } else if ly < MENU_H {
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
                } else if ly >= TOP_H && my < bar {
                    // The bookmarks toolbar: Ctrl+click opens in a new tab.
                    if let Some(b) = bookmark_at(&bookmarks.list, w as f32 / k, lx, ly) {
                        let url = bookmarks.list[b].url.clone();
                        if ctrl {
                            tab_action = Some(TabAction::Background(url));
                        } else {
                            navigate = Some((url, None));
                        }
                    }
                    if ui.editing {
                        ui.editing = false;
                        ui.address.set(&info.url);
                    }
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
                } else if let Some(b) = (my > bar && mx < page_w)
                    .then(|| box_bar_at(&rendered.scrollers, mx, my - bar + scroll, scroll))
                    .flatten()
                {
                    // A box's scrollbar: drag its thumb (pressing beside
                    // the thumb jumps there first).
                    let along = if b.vertical { my - bar + scroll } else { mx };
                    let mut d = BoxDrag::grab(&b, along);
                    let to = d.target(along);
                    if to != d.sent {
                        let delta = to - d.sent;
                        let (dx, dy) = if d.vertical { (0.0, delta) } else { (delta, 0.0) };
                        queue_box_scroll(&mut pending_input, d.uid, dx, dy);
                        d.sent = to;
                    }
                    box_drag = Some(d);
                    key_box = Some((b.uid, info.url.clone()));
                } else if my > bar {
                    // Keys scroll the box you clicked in.
                    let (px, py) = (mx, my - bar + scroll);
                    key_box = rendered
                        .scrollers
                        .iter()
                        .rev()
                        .find(|s| s.at(scroll).contains(px, py))
                        .map(|s| (s.uid, info.url.clone()));
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
        if panel_drag {
            if !down {
                panel_drag = false;
                save_scale("panel-width", panel_w());
            } else if let Some((mx, _)) = mouse {
                let max = (w as f32 / k - 200.0).max(MIN_PANEL_W);
                let pw = ((w as f32 - mx) / k).clamp(MIN_PANEL_W, max).round();
                if pw != panel_w() {
                    PANEL_W.store(pw.to_bits(), Ordering::Relaxed);
                    dirty = true;
                }
            }
        }
        if let Some(d) = box_drag.as_mut() {
            match (down, mouse) {
                (true, Some((mx, my))) => {
                    let along = if d.vertical { my - bar + scroll } else { mx };
                    let to = d.target(along);
                    if to != d.sent {
                        let delta = to - d.sent;
                        let (dx, dy) = if d.vertical { (0.0, delta) } else { (delta, 0.0) };
                        queue_box_scroll(&mut pending_input, d.uid, dx, dy);
                        d.sent = to;
                        dirty = true;
                    }
                }
                (true, None) => {}
                _ => box_drag = None,
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
            ui.resend = None;
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
                TabAction::Reopen => match closed.pop() {
                    Some(c) if order.len() < MAX_TABS => {
                        // Loaded afresh (a form's answer isn't sent again),
                        // with its history, where it was in the strip.
                        open_tab!(c.page.0, c.page.1, true);
                        history.extend(c.history);
                        forward.extend(c.forward);
                        if let Some(i) = order.iter().position(|&t| t == cur_id) {
                            let id = order.remove(i);
                            order.insert(c.at.min(order.len()), id);
                        }
                    }
                    Some(c) => {
                        closed.push(c);
                        ui.flash = Some(format!("Brickle keeps at most {MAX_TABS} tabs open"));
                    }
                    None => ui.flash = Some("No closed tabs to reopen".into()),
                },
                TabAction::Close(id) => {
                    let at = order.iter().position(|&t| t == id).unwrap_or(order.len());
                    let gone = if id == cur_id {
                        Some((
                            info.url.clone(),
                            info.opener.clone(),
                            history.clone(),
                            forward.clone(),
                        ))
                    } else {
                        parked.iter().find(|p| p.id == id).map(|p| {
                            (
                                p.info.url.clone(),
                                p.info.opener.clone(),
                                p.history.clone(),
                                p.forward.clone(),
                            )
                        })
                    };
                    if let Some((url, opener, history, forward)) = gone {
                        if !(url.is_empty() || url == "about:home") || !history.is_empty() {
                            if closed.len() >= MAX_CLOSED_TABS {
                                closed.remove(0);
                            }
                            closed.push(ClosedTab {
                                page: (url, opener),
                                history,
                                forward,
                                at,
                            });
                        }
                    }
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
        // Loading a page that answered a form means sending the form again:
        // ask first (as Firefox does) rather than repeat a purchase or post.
        let resends = match (reload, hist) {
            (true, _) => info.resubmit.is_some(),
            (_, -1) => history.last().is_some_and(|v| v.1.is_some()),
            (_, 1) => forward.last().is_some_and(|v| v.1.is_some()),
            _ => false,
        };
        if resends && !resend_ok && navigate.is_none() {
            ui.resend = Some(if reload { 0 } else { hist });
            reload = false;
            hist = 0;
            dirty = true;
        }
        if (navigate.is_some() || reload || hist != 0) && !resend_ok {
            ui.resend = None;
        }
        resend_ok = false;
        // Navigation requests are queued and started when the loader is free.
        let here = || ((info.url.clone(), info.opener.clone()), info.resubmit.clone());
        let moved = match hist {
            -1 => history.pop().inspect(|_| forward.push(here())),
            1 => forward.pop().inspect(|_| history.push(here())),
            _ => None,
        };
        if reload {
            queued = Some((info.url.clone(), info.opener.clone()));
            queued_post = info.resubmit.clone();
        } else if let Some((entry, post)) = moved {
            queued = Some(entry);
            queued_post = post;
        } else if let Some((url, from)) = navigate {
            if !(url.starts_with("mailto:") || url.starts_with("javascript:")) {
                if url != info.url {
                    history.push(here());
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
        if tiles.as_mut().is_some_and(|t| t.collect(&painter)) {
            dirty = true; // tiles came back from the painters
        }
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
            if let Some(old) = stale_tiles.as_ref().filter(|o| o.width() == page_w as usize) {
                old.draw_painted(&mut canvas, scroll, bar as usize);
            }
            if tc.draw(&mut canvas, scroll, bar as usize, Some(&painter)) {
                stale_tiles = None;
            }
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
            // Thumbs on the boxes you can scroll inside.
            for sc in &rendered.scrollers {
                for t in box_bars(sc, sc.at(scroll)).iter().map(|b| b.thumb) {
                    if t.y + t.h < scroll || t.y > scroll + view_h {
                        continue;
                    }
                    let t = Rect {
                        y: t.y - scroll + bar,
                        w: t.w.min(page_w - t.x),
                        ..t
                    };
                    canvas.fill(
                        t,
                        Color {
                            r: 0,
                            g: 0,
                            b: 0,
                            a: 90,
                        },
                        bar,
                    );
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
            if BOOKMARKS_BAR.load(Ordering::Relaxed) {
                let at = mouse.map(|(x, y)| (x / k, y / k));
                draw_bookmarks(&mut canvas, &bookmarks.list, &info.url, at, &mut cache);
            }
            let titles: Vec<(TabLabel, bool)> = order
                .iter()
                .map(|&id| {
                    if id == cur_id {
                        (TabLabel::of(&info, &rendered), true)
                    } else {
                        let p = parked.iter().find(|p| p.id == id);
                        (
                            p.map(|p| TabLabel::of(&p.info, &p.rendered)).unwrap_or_default(),
                            false,
                        )
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
                        y: TOP_H - 3.0,
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
                    &info,
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
            if ui.resend.is_some() {
                draw_resend(&mut canvas, page_w / k, bar / k, &mut cache);
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
    /// Bring back the tab closed last.
    Reopen,
}

/// What Ctrl+Shift+T brings back of a closed tab.
struct ClosedTab {
    page: Entry,
    history: Vec<Visit>,
    forward: Vec<Visit>,
    /// Its place in the tab strip.
    at: usize,
}

/// Closed tabs remembered for reopening.
const MAX_CLOSED_TABS: usize = 25;

/// A tab that isn't showing: everything the loop keeps for the current one.
struct Parked {
    id: TabId,
    tab: Option<Tab>,
    info: Info,
    rendered: Rendered,
    history: Vec<Visit>,
    forward: Vec<Visit>,
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
    /// When it went to the background.
    parked_at: Instant,
    /// Its renderer was let go; switching to it loads the page again.
    discarded: bool,
}

/// A background tab left alone this long gives up its renderer (the process,
/// its memory and the page's scripts); switching back reloads it.
const DISCARD_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// Most background tabs that keep a renderer; the longest-unused go first.
const MAX_LIVE_BACKGROUND: usize = 6;

/// Which background tabs to discard, given how long each has been in the
/// background and whether it can be (it has a renderer, isn't loading, and
/// you weren't typing in it).
fn tabs_to_discard(tabs: &[(std::time::Duration, bool)]) -> Vec<usize> {
    let mut live: Vec<usize> = (0..tabs.len()).filter(|&i| tabs[i].1).collect();
    // Most recently used first.
    live.sort_by_key(|&i| tabs[i].0);
    live.into_iter()
        .enumerate()
        .filter(|&(n, i)| n >= MAX_LIVE_BACKGROUND || tabs[i].0 >= DISCARD_AFTER)
        .map(|(_, i)| i)
        .collect()
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
                resubmit: None,
                note: None,
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
            parked_at: Instant::now(),
            discarded: false,
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
        scrollers: vec![],
        title: None,
    }
}

fn page_w_now(ui: &Ui, w: usize) -> f32 {
    if ui.panel {
        (w as f32 - panel_w() * ui_scale()).max(200.0).round()
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

/// Size of the tab bar's text.
const TAB_TEXT: f32 = 12.0;

/// The tab bar's text size in window pixels, which renderers draw page
/// titles at.
fn tab_title_px() -> f32 {
    TAB_TEXT * ui_scale()
}

/// What a tab shows: its page's title as the renderer drew it (the browser
/// never runs the font code on text a page chose), or, until there is one
/// at the current size, the site's host from the address.
#[derive(Default)]
struct TabLabel<'a> {
    title: Option<&'a GlyphBitmap>,
    fallback: String,
}

impl<'a> TabLabel<'a> {
    fn of(info: &Info, rendered: &'a Rendered) -> TabLabel<'a> {
        let px = tab_title_px();
        let (ascent, descent) = font::vmetrics(SANS, px);
        let fits = |t: &&GlyphBitmap| t.h as usize == ((ascent + descent).ceil() as usize);
        let (host, _) = net::display_parts(&info.url);
        TabLabel {
            title: rendered.title.as_ref().filter(fits),
            fallback: if host.is_empty() { info.url.clone() } else { host },
        }
    }
}

/// `t` cut to `max_w` pixels, fading out over its last few if it was cut.
fn faded(t: &GlyphBitmap, max_w: usize) -> GlyphBitmap {
    let (w, h) = (t.w as usize, t.h as usize);
    let cut = w.min(max_w);
    let fade = if w > max_w { (cut / 4).min(24) } else { 0 };
    let mut data = Vec::with_capacity(cut * h);
    for row in 0..h {
        for x in 0..cut {
            let a = t.data[row * w + x] as usize;
            let left = cut - x;
            data.push(if left <= fade {
                (a * left / (fade + 1)) as u8
            } else {
                a as u8
            });
        }
    }
    GlyphBitmap {
        w: cut as u16,
        h: h as u16,
        data,
    }
}

fn draw_tabs(c: &mut Canvas, titles: &[(TabLabel, bool)], cache: &mut GlyphCache) {
    let w = c.size().0;
    let (rects, plus) = tab_rects(titles.len(), w);
    for ((label, current), (r, close)) in titles.iter().zip(&rects) {
        let bg = if *current {
            Color::WHITE
        } else {
            Color::rgb(0xdd, 0xe0, 0xe5)
        };
        c.fill(*r, bg, 0.0);
        let room = r.w - 34.0;
        match label.title {
            Some(t) if t.data.len() == t.w as usize * t.h as usize => {
                // Placed as `text` would place it: the baseline 17px down.
                let k = c.scale;
                let (ascent, _) = font::vmetrics(SANS, TAB_TEXT * k);
                let color = if c.dark { paint::dark_variant(DARK) } else { DARK };
                c.blit(
                    ((r.x + 8.0) * k).round(),
                    ((r.y + 17.0) * k - ascent).round(),
                    &faded(t, (room * k).max(0.0) as usize),
                    color,
                    0.0,
                );
            }
            _ => {
                let shown = fit(&label.fallback, room, SANS, TAB_TEXT);
                text(c, r.x + 8.0, r.y + 17.0, &shown, SANS, TAB_TEXT, DARK, cache, 0.0);
            }
        }
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

/// A scrollbar of a box you can scroll inside, in page space: the strip
/// along its edge you can grab, and the thumb in it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BoxBar {
    uid: u32,
    vertical: bool,
    track: Rect,
    thumb: Rect,
    /// How far the box scrolls this way, and how far it has.
    max: f32,
    at: f32,
}

/// The scrollbars of a box you can scroll inside, shown at `r` (page
/// space): along its right edge, and its bottom if it scrolls sideways.
fn box_bars(sc: &page::Scroller, r: Rect) -> Vec<BoxBar> {
    let mut out = Vec::new();
    if r.h <= 8.0 || r.w <= 8.0 {
        return out;
    }
    let thumb = |len: f32, max: f32, at: f32| {
        let t = (len * len / (len + max)).max(16.0).min(len);
        ((at / max).clamp(0.0, 1.0) * (len - t), t)
    };
    if sc.max_y > 0.0 {
        let (y, h) = thumb(r.h, sc.max_y, sc.y);
        out.push(BoxBar {
            uid: sc.uid,
            vertical: true,
            track: Rect {
                x: r.x + r.w - 10.0,
                y: r.y,
                w: 10.0,
                h: r.h,
            },
            thumb: Rect {
                x: r.x + r.w - 6.0,
                y: r.y + y,
                w: 4.0,
                h,
            },
            max: sc.max_y,
            at: sc.y,
        });
    }
    if sc.max_x > 0.0 {
        let (x, w) = thumb(r.w, sc.max_x, sc.x);
        out.push(BoxBar {
            uid: sc.uid,
            vertical: false,
            track: Rect {
                x: r.x,
                y: r.y + r.h - 10.0,
                w: r.w,
                h: 10.0,
            },
            thumb: Rect {
                x: r.x + x,
                y: r.y + r.h - 6.0,
                w,
                h: 4.0,
            },
            max: sc.max_x,
            at: sc.x,
        });
    }
    out
}

/// The scrollbar of a box at page point (x, y), innermost first.
fn box_bar_at(scrollers: &[page::Scroller], x: f32, y: f32, scroll: f32) -> Option<BoxBar> {
    scrollers
        .iter()
        .rev()
        .flat_map(|sc| box_bars(sc, sc.at(scroll)))
        .find(|b| b.track.contains(x, y))
}

/// Dragging a box's scrollbar thumb.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BoxDrag {
    uid: u32,
    vertical: bool,
    /// Where along the thumb you hold it.
    grab: f32,
    track_start: f32,
    track_len: f32,
    thumb_len: f32,
    max: f32,
    /// The offset last asked for.
    sent: f32,
}

impl BoxDrag {
    /// Grab `b` at `along` (page space, along its axis): by the thumb where
    /// you hold it, or by its middle if you pressed beside it.
    fn grab(b: &BoxBar, along: f32) -> BoxDrag {
        let (start, len) = if b.vertical {
            (b.thumb.y, b.thumb.h)
        } else {
            (b.thumb.x, b.thumb.w)
        };
        let (track_start, track_len) = if b.vertical {
            (b.track.y, b.track.h)
        } else {
            (b.track.x, b.track.w)
        };
        BoxDrag {
            uid: b.uid,
            vertical: b.vertical,
            grab: if (start..start + len).contains(&along) {
                along - start
            } else {
                len / 2.0
            },
            track_start,
            track_len,
            thumb_len: len,
            max: b.max,
            sent: b.at,
        }
    }

    /// The offset for the pointer at `along`.
    fn target(&self, along: f32) -> f32 {
        let room = self.track_len - self.thumb_len;
        if room <= 0.0 {
            return self.sent;
        }
        (((along - self.grab - self.track_start) / room).clamp(0.0, 1.0) * self.max).round()
    }
}

/// Queue scrolling inside a box, joined with the last such request for it.
fn queue_box_scroll(pending: &mut Vec<Input>, uid: u32, dx: f32, dy: f32) {
    match pending.last_mut() {
        Some(Input::Scroll { uid: u, dx: x, dy: y }) if *u == uid => {
            *x += dx;
            *y += dy;
        }
        _ => pending.push(Input::Scroll { uid, dx, dy }),
    }
}

/// The box a key scrolls by (dx, dy) (a sign; `page` scrolls by most of
/// the box, `end` all the way): the one under the pointer, else the one you
/// last clicked in, if it can still move that way.
fn box_for_key(
    scrollers: &[page::Scroller],
    pointer: Option<(f32, f32)>,
    clicked: Option<u32>,
    scroll: f32,
    dir: (f32, f32),
) -> Option<u32> {
    pointer
        .and_then(|(x, y)| page::scroller_at(scrollers, x, y, scroll, dir.0, dir.1))
        .or_else(|| {
            scrollers
                .iter()
                .rev()
                .find(|s| Some(s.uid) == clicked && s.can_scroll(dir.0, dir.1))
        })
        .map(|s| s.uid)
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

/// Where each bookmark is on the toolbar, left to right, as many as fit
/// in `w`.
fn bookmark_rects(list: &[brickle::bookmarks::Bookmark], w: f32) -> Vec<Rect> {
    let mut x = 8.0;
    let mut out = Vec::new();
    for b in list {
        let bw = (font::measure(SANS, 11.5, &b.shown()) + 20.0).min(200.0);
        if x + bw > w - 40.0 {
            break;
        }
        out.push(Rect {
            x,
            y: TOP_H + 3.0,
            w: bw,
            h: BOOK_H - 6.0,
        });
        x += bw + 4.0;
    }
    out
}

fn bookmark_at(list: &[brickle::bookmarks::Bookmark], w: f32, x: f32, y: f32) -> Option<usize> {
    if !BOOKMARKS_BAR.load(Ordering::Relaxed) || !(TOP_H..TOP_H + BOOK_H).contains(&y) {
        return None;
    }
    bookmark_rects(list, w).iter().position(|r| r.contains(x, y))
}

/// The bookmarks toolbar, under the address bar.
fn draw_bookmarks(
    c: &mut Canvas,
    list: &[brickle::bookmarks::Bookmark],
    page_url: &str,
    mouse: Option<(f32, f32)>,
    cache: &mut GlyphCache,
) {
    let w = c.size().0;
    if list.is_empty() {
        let hint = "Bookmark a page with Ctrl+D and it appears here  ·  Ctrl+Shift+B hides this bar";
        text(
            c,
            12.0,
            TOP_H + 18.0,
            &fit(hint, w - 24.0, SANS, 11.0),
            SANS,
            11.0,
            GRAY,
            cache,
            0.0,
        );
        return;
    }
    let rects = bookmark_rects(list, w);
    for (b, r) in list.iter().zip(&rects) {
        let hovered = mouse.is_some_and(|(x, y)| r.contains(x, y));
        if hovered || b.url == page_url {
            c.fill(
                *r,
                if hovered {
                    LINE
                } else {
                    Color::rgb(0xdd, 0xe1, 0xe6)
                },
                0.0,
            );
        }
        let label = fit(&b.shown(), r.w - 20.0, SANS, 11.5);
        text(c, r.x + 10.0, r.y + 15.5, &label, SANS, 11.5, DARK, cache, 0.0);
    }
    if rects.len() < list.len() {
        let more = format!("+{}", list.len() - rects.len());
        let mw = font::measure(SANS, 11.0, &more);
        text(
            c,
            w - mw - 10.0,
            TOP_H + 18.0,
            &more,
            SANS,
            11.0,
            GRAY,
            cache,
            0.0,
        );
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
        y: bars() + y,
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

fn panel_row(
    log: &[LogEntry],
    page_url: &str,
    note: Option<&str>,
    my: f32,
    panel_scroll: f32,
    pw: f32,
) -> Option<usize> {
    let y = my - bars() - PANEL_HEAD + panel_scroll;
    if y < 0.0 {
        return None;
    }
    let rows = panel_rows(log, page_url, note, pw);
    rows.rows.iter().position(|r| y >= r.y && y < r.y + r.h)
}

/// A request row in the panel, laid out: the address and notes wrapped
/// to the panel's width so all of them can be read.
#[derive(Clone)]
struct PanelRow {
    /// Top, from the first row's, and height.
    y: f32,
    h: f32,
    head: String,
    url: Vec<String>,
    notes: Vec<String>,
    /// What the request was for ([`brickle::explain`]).
    why: Vec<String>,
}

/// The panel's list, laid out: a note about the whole page (if any), then
/// a row per request (their `y` is below the note).
#[derive(Clone, Default)]
struct PanelRows {
    note: Vec<String>,
    note_h: f32,
    rows: Vec<PanelRow>,
}

thread_local! {
    static PANEL_ROWS: std::cell::RefCell<(u64, std::rc::Rc<PanelRows>)> = Default::default();
}

/// The panel's rows for `log` at width `pw` (cached: laid out again only
/// when the log or the width changes).
fn panel_rows(log: &[LogEntry], page_url: &str, note: Option<&str>, pw: f32) -> std::rc::Rc<PanelRows> {
    use std::hash::{Hash, Hasher};
    let described: Vec<(&str, String)> = log.iter().map(describe).collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    pw.to_bits().hash(&mut hasher);
    page_url.hash(&mut hasher);
    note.hash(&mut hasher);
    for (e, (status, notes)) in log.iter().zip(&described) {
        (&e.url, status, notes).hash(&mut hasher);
    }
    let key = hasher.finish();
    if let Some(rows) = PANEL_ROWS.with(|c| {
        let c = c.borrow();
        (c.0 == key && (!c.1.rows.is_empty() || !c.1.note.is_empty())).then(|| c.1.clone())
    }) {
        return rows;
    }
    let note_lines = note.map_or(vec![], |n| wrap(n, pw - 28.0, pw - 28.0, BOLD, 10.5, true));
    let note_h = if note_lines.is_empty() {
        0.0
    } else {
        ROW_PAD * 2.0 + ROW_LINE * note_lines.len() as f32
    };
    let mut rows = Vec::with_capacity(log.len());
    let mut y = note_h;
    for (e, (status, notes)) in log.iter().zip(described) {
        let head = format!("{status} · {}", dest_kind(e.dest));
        let hw = font::measure(BOLD, 11.0, &head) + 8.0;
        let (host, rest) = net::display_parts(&e.url);
        let host = host.split_once("://").map(|(_, h)| h.to_string()).unwrap_or(host);
        let url = wrap(
            &format!("{host}{rest}"),
            pw - 24.0 - hw,
            pw - 24.0,
            SANS,
            11.0,
            false,
        );
        let notes = if notes.is_empty() {
            vec![]
        } else {
            wrap(&notes, pw - 28.0, pw - 28.0, SANS, 10.0, true)
        };
        let why: Vec<String> = brickle::explain::explain(e, page_url)
            .iter()
            .flat_map(|l| wrap(l, pw - 28.0, pw - 28.0, SANS, 10.5, true))
            .collect();
        let h = ROW_PAD * 2.0 + ROW_LINE * (url.len().max(1) + notes.len() + why.len()) as f32;
        rows.push(PanelRow {
            y,
            h,
            head,
            url,
            notes,
            why,
        });
        y += h;
    }
    let rows = std::rc::Rc::new(PanelRows {
        note: note_lines,
        note_h,
        rows,
    });
    PANEL_ROWS.with(|c| *c.borrow_mut() = (key, rows.clone()));
    rows
}

/// `text` broken into lines: the first at most `first_w` wide, the rest
/// `rest_w`, at most [`ROW_MAX_LINES`] (the last ends in "…" if it's cut
/// short). `words`: break at spaces where possible (addresses break
/// anywhere).
fn wrap(text: &str, first_w: f32, rest_w: f32, font_idx: usize, size: f32, words: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut lw = 0.0;
    let mut buf = [0u8; 4];
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let max = if lines.is_empty() { first_w } else { rest_w }.max(20.0);
        let cw = font::measure(font_idx, size, ch.encode_utf8(&mut buf));
        if lw + cw > max && !line.is_empty() {
            if lines.len() + 1 == ROW_MAX_LINES {
                lines.push(fit(
                    &format!("{line}{ch}{}", chars.collect::<String>()),
                    max,
                    font_idx,
                    size,
                ));
                return lines;
            }
            let mut next = String::new();
            if words {
                if let Some(i) = line.rfind(' ').filter(|&i| i > 0) {
                    next = line[i + 1..].to_string();
                    line.truncate(i);
                }
            }
            lines.push(std::mem::take(&mut line));
            lw = font::measure(font_idx, size, &next);
            line = next;
        }
        line.push(ch);
        lw += cw;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn dest_kind(d: net::Dest) -> &'static str {
    match d {
        net::Dest::Document => "page",
        net::Dest::Stylesheet => "css",
        net::Dest::Script => "js",
        net::Dest::Xhr => "xhr",
        net::Dest::Image => "img",
        net::Dest::Download => "list",
    }
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
            h: bars(),
        },
        CHROME,
        0.0,
    );
    c.fill(
        Rect {
            x: 0.0,
            y: bars() - 1.0,
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
            y: bars() + y / c.scale,
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
            bars(),
        );
        c.dark = dark;
    }
}

fn draw_panel(
    c: &mut Canvas,
    info: &Info,
    net: Option<&Network>,
    panel_scroll: f32,
    x0: f32,
    cache: &mut GlyphCache,
) {
    let (page_url, note, log) = (info.url.as_str(), info.note.as_deref(), &info.log[..]);
    let (w, h) = (c.size().0, c.size().1);
    let pw = w - x0;
    c.fill(
        Rect {
            x: x0,
            y: bars(),
            w: pw,
            h: h - bars(),
        },
        Color::rgb(0xf8, 0xf9, 0xfb),
        bars(),
    );
    c.fill(
        Rect {
            x: x0,
            y: bars(),
            w: 1.0,
            h: h - bars(),
        },
        LINE,
        bars(),
    );
    let (blocked, total) = counts(log);
    text(
        c,
        x0 + 14.0,
        bars() + 22.0,
        &format!("Requests  {total}  ·  blocked {blocked}"),
        BOLD,
        13.0,
        DARK,
        cache,
        bars(),
    );
    let sub = match net {
        None => "Loading…".to_string(),
        Some(_) if net::firefox_version().1 => {
            "Brickle is out of date: update it so it doesn't stand out.".to_string()
        }
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
    text(c, x0 + 14.0, bars() + 42.0, &sub, SANS, 11.0, GRAY, cache, bars());
    if let Some(n) = net.filter(|_| net::scheme(page_url).starts_with("http")) {
        let count = n.cookies.for_site(page_url).len();
        let kept = n.cookies.is_kept(page_url);
        let label = format!("Cookies: {count}");
        text(
            c,
            x0 + 14.0,
            bars() + 68.0,
            &label,
            BOLD,
            11.0,
            DARK,
            cache,
            bars(),
        );
        text(
            c,
            x0 + 14.0,
            bars() + 96.0,
            "JavaScript:",
            BOLD,
            11.0,
            DARK,
            cache,
            bars(),
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
            c.fill(r, LINE, bars());
            c.fill(
                Rect {
                    x: r.x + 1.0,
                    y: r.y + 1.0,
                    w: r.w - 2.0,
                    h: r.h - 2.0,
                },
                bg,
                bars(),
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
                bars(),
            );
        }
    }
    c.fill(
        Rect {
            x: x0,
            y: bars() + PANEL_HEAD - 1.0,
            w: pw,
            h: 1.0,
        },
        LINE,
        bars(),
    );

    let clip = bars() + PANEL_HEAD;
    let layout = panel_rows(log, page_url, note, pw);
    if !layout.note.is_empty() {
        let y = clip - panel_scroll;
        c.fill(
            Rect {
                x: x0,
                y,
                w: pw,
                h: layout.note_h - 1.0,
            },
            Color::rgb(0xfe, 0xf3, 0xc7),
            clip,
        );
        for (i, l) in layout.note.iter().enumerate() {
            let base = y + ROW_PAD + 11.0 + i as f32 * ROW_LINE;
            text(
                c,
                x0 + 14.0,
                base,
                l,
                BOLD,
                10.5,
                Color::rgb(0x78, 0x35, 0x0f),
                cache,
                clip,
            );
        }
    }
    for (e, r) in log.iter().zip(layout.rows.iter()) {
        let y = clip + r.y - panel_scroll;
        if y + r.h < clip || y > h {
            continue;
        }
        let color = match e.outcome {
            Outcome::Blocked { .. }
            | Outcome::Denied { .. }
            | Outcome::MixedContent
            | Outcome::NotAllowed
            | Outcome::LocalNetwork
            | Outcome::CrossSite { .. }
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
                h: r.h - 1.0,
            },
            color,
            clip,
        );
        let base = y + ROW_PAD + 11.0;
        text(c, x0 + 14.0, base, &r.head, BOLD, 11.0, color, cache, clip);
        let hx = x0 + 14.0 + font::measure(BOLD, 11.0, &r.head) + 8.0;
        for (i, l) in r.url.iter().enumerate() {
            let x = if i == 0 { hx } else { x0 + 14.0 };
            text(c, x, base + i as f32 * ROW_LINE, l, SANS, 11.0, DARK, cache, clip);
        }
        let nb = base + r.url.len().max(1) as f32 * ROW_LINE;
        for (i, l) in r.notes.iter().enumerate() {
            text(
                c,
                x0 + 14.0,
                nb + i as f32 * ROW_LINE,
                l,
                SANS,
                10.0,
                GRAY,
                cache,
                clip,
            );
        }
        let wb = nb + r.notes.len() as f32 * ROW_LINE;
        for (i, l) in r.why.iter().enumerate() {
            text(
                c,
                x0 + 14.0,
                wb + i as f32 * ROW_LINE,
                l,
                SANS,
                10.5,
                EXPLAIN,
                cache,
                clip,
            );
        }
        c.fill(
            Rect {
                x: x0,
                y: y + r.h - 1.0,
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
        bars(),
    );
    c.fill(bar, Color::WHITE, bars());
    let base = bar.y + 20.0;
    text(c, bar.x + 8.0, base, "Find", BOLD, 13.0, GRAY, cache, bars());
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
        bars(),
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
            bars(),
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
        bars(),
    );
    c.fill(
        Rect {
            x: x_of(q.cursor).min(x0 + avail),
            y: bar.y + 6.0,
            w: 1.0,
            h: bar.h - 12.0,
        },
        DARK,
        bars(),
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
            bars(),
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
            bars(),
        );
    }
}

/// The "send the form again?" box: centred at the top of the page.
fn resend_box(page_w: f32, top: f32) -> Rect {
    let w = RESEND_W.min(page_w - 20.0).max(0.0);
    Rect {
        x: ((page_w - w) / 2.0).max(0.0),
        y: top + 12.0,
        w,
        h: RESEND_H,
    }
}

/// Its Resend and Cancel buttons.
fn resend_buttons(page_w: f32, top: f32) -> [Rect; 2] {
    let b = resend_box(page_w, top);
    let button = |right: f32, w: f32| Rect {
        x: b.x + b.w - right - w,
        y: b.y + b.h - 40.0,
        w,
        h: 28.0,
    };
    [button(108.0, 90.0), button(12.0, 86.0)]
}

fn draw_resend(c: &mut Canvas, page_w: f32, top: f32, cache: &mut GlyphCache) {
    let b = resend_box(page_w, top);
    let edge = Edges {
        t: 1.0,
        r: 1.0,
        b: 1.0,
        l: 1.0,
    };
    c.fill(b.expand(edge), Color::rgb(0x3b, 0x82, 0xf6), bars());
    c.fill(b, Color::WHITE, bars());
    text(
        c,
        b.x + 14.0,
        b.y + 26.0,
        "Send the form again?",
        BOLD,
        14.0,
        DARK,
        cache,
        bars(),
    );
    let line = "This page is the answer to a form you sent. Loading it again sends the form again,                 which can repeat an action such as a purchase or a post.";
    let mut y = b.y + 50.0;
    for l in wrap_words(line, b.w - 28.0, SANS, 12.5).iter().take(3) {
        text(c, b.x + 14.0, y, l, SANS, 12.5, GRAY, cache, bars());
        y += 18.0;
    }
    let [resend, cancel] = resend_buttons(page_w, top);
    c.fill(resend, Color::rgb(0x1d, 0x4e, 0xd8), bars());
    c.fill(cancel, CHROME, bars());
    for (r, label, color) in [(resend, "Resend", Color::WHITE), (cancel, "Cancel", DARK)] {
        let tw = font::measure(SANS, 13.0, label);
        text(
            c,
            r.x + (r.w - tw) / 2.0,
            r.y + 19.0,
            label,
            SANS,
            13.0,
            color,
            cache,
            bars(),
        );
    }
}

/// Split `s` into lines no wider than `max_w`.
fn wrap_words(s: &str, max_w: f32, font: usize, size: f32) -> Vec<String> {
    let mut lines = vec![];
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let next = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if !cur.is_empty() && font::measure(font, size, &next) > max_w {
            lines.push(std::mem::replace(&mut cur, word.to_string()));
        } else {
            cur = next;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
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
    fn idle_and_surplus_background_tabs_are_discarded() {
        use std::time::Duration;
        let min = |m: u64| Duration::from_secs(m * 60);
        // Recent ones stay; one left alone for 11 minutes goes.
        assert_eq!(
            tabs_to_discard(&[(min(1), true), (min(11), true), (min(3), true)]),
            [1]
        );
        // One that's loading or being typed in stays, however old.
        assert!(tabs_to_discard(&[(min(60), false)]).is_empty());
        // Past MAX_LIVE_BACKGROUND, the longest-unused go first.
        let many: Vec<_> = (0..9).map(|i| (Duration::from_secs(i), true)).collect();
        assert_eq!(tabs_to_discard(&many), [6, 7, 8]);
    }

    #[test]
    fn box_scrollbars_drag_and_keys() {
        let sc = page::Scroller {
            uid: 9,
            rect: Rect {
                x: 100.0,
                y: 200.0,
                w: 300.0,
                h: 100.0,
            },
            x: 0.0,
            y: 0.0,
            max_x: 0.0,
            max_y: 300.0,
            layer: None,
        };
        let bars = box_bars(&sc, sc.at(0.0));
        assert_eq!(bars.len(), 1, "only up and down");
        let b = bars[0];
        assert!(b.vertical && b.track.x == 390.0 && b.thumb.h == 25.0, "{b:?}");
        assert_eq!(box_bar_at(&[sc], 395.0, 250.0, 0.0), Some(b));
        assert_eq!(
            box_bar_at(&[sc], 300.0, 250.0, 0.0),
            None,
            "inside, not on the bar"
        );

        // Held by the thumb, it follows the pointer; it stops at the ends.
        let mut d = BoxDrag::grab(&b, 205.0);
        assert_eq!(d.grab, 5.0);
        assert_eq!(d.target(205.0), 0.0);
        assert_eq!(d.target(205.0 + 37.5), 150.0, "half way down the track");
        assert_eq!(d.target(1000.0), 300.0);
        assert_eq!(d.target(-50.0), 0.0);
        // Pressed below the thumb: it jumps to put its middle there.
        d = BoxDrag::grab(&b, 300.0 - 12.5);
        assert_eq!(d.grab, 12.5);
        assert_eq!(d.target(300.0 - 12.5), 300.0);

        // Keys: the box under the pointer, else the one you clicked in.
        let at = Some((150.0, 250.0));
        assert_eq!(box_for_key(&[sc], at, None, 0.0, (0.0, 1.0)), Some(9));
        assert_eq!(
            box_for_key(&[sc], at, None, 0.0, (0.0, -1.0)),
            None,
            "already at the top"
        );
        assert_eq!(
            box_for_key(&[sc], at, None, 0.0, (1.0, 0.0)),
            None,
            "doesn't scroll sideways"
        );
        assert_eq!(
            box_for_key(&[sc], Some((5.0, 5.0)), Some(9), 0.0, (0.0, 1.0)),
            Some(9)
        );
        assert_eq!(box_for_key(&[sc], Some((5.0, 5.0)), None, 0.0, (0.0, 1.0)), None);

        // Requests for one box join up.
        let mut q = vec![];
        queue_box_scroll(&mut q, 9, 0.0, 40.0);
        queue_box_scroll(&mut q, 9, 0.0, 40.0);
        queue_box_scroll(&mut q, 3, 10.0, 0.0);
        assert_eq!(
            q,
            vec![
                Input::Scroll {
                    uid: 9,
                    dx: 0.0,
                    dy: 80.0
                },
                Input::Scroll {
                    uid: 3,
                    dx: 10.0,
                    dy: 0.0
                }
            ]
        );
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
            resend: None,
            loading: false,
        };
        ui.address.set("https://example.com/");
        let w = c.size().0;
        draw_chrome(&mut c, &ui, &[], 0.0, 0.0, 160.0, w, &mut cache);
        // As renderers draw titles: the first fits, the second is cut.
        let px = TAB_TEXT;
        let a = paint::text_mask("Example Domain", SANS, px * k, paint::MAX_TITLE_W);
        let b = paint::text_mask(
            "A very long page title that won't fit in its tab at all",
            SANS,
            px * k,
            paint::MAX_TITLE_W,
        );
        fn label<'a>(title: Option<&'a GlyphBitmap>, fallback: &str) -> TabLabel<'a> {
            TabLabel {
                title,
                fallback: fallback.into(),
            }
        }
        draw_tabs(
            &mut c,
            &[
                (label(a.as_ref(), ""), true),
                (label(b.as_ref(), ""), false),
                (label(None, "loading.example"), false),
            ],
            &mut cache,
        );
        draw_menu_bar(&mut c, &ui, &mut cache);
        if std::env::var("MENU_RESEND").is_ok() {
            draw_resend(&mut c, w as f32 / k, bars(), &mut cache);
        }
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

    /// The bookmarks toolbar and the requests panel with long addresses
    /// (`PANEL_PNG=x.png cargo test panel_picture -- --ignored`).
    #[test]
    #[ignore]
    fn panel_picture() {
        use brickle::bookmarks::Bookmark;
        use brickle::network::{LogEntry, Outcome};
        let mut c = Canvas::new(1100, 820, Color::WHITE);
        c.dark = std::env::var("MENU_LIGHT").is_err();
        let mut cache = GlyphCache::default();
        let mut ui = Ui {
            address: LineEdit::new(64),
            editing: false,
            hover: None,
            panel: true,
            panel_scroll: 0.0,
            flash: None,
            menu: None,
            text_scale: DEFAULT_TEXT_SCALE,
            can_back: true,
            can_forward: false,
            resend: None,
            loading: false,
        };
        ui.address.set("https://www.facebook.com/?_fb_noscript=1");
        let entry = |url: &str, dest, outcome| LogEntry {
            url: url.into(),
            dest,
            outcome,
            stripped: vec![],
            upgraded: false,
            insecure: false,
            allowed_by: None,
            cookies_sent: vec!["datr".into(), "fr".into(), "sb".into()],
            cookies_set: vec!["fr".into()],
            cookies_refused: vec![],
            cors_blocked: false,
            list: None,
        };
        let log = vec![
            entry("https://www.facebook.com/?_fb_noscript=1", net::Dest::Document, Outcome::Loaded { status: 200 }),
            entry(
                "data:text/css; charset=utf-8;base64,LnRyeXtjb2xvcjpyZWR9LmFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6MDEyMzQ1Njc4OQ==",
                net::Dest::Stylesheet,
                Outcome::NotAllowed,
            ),
            entry(
                "https://static.xx.fbcdn.net/rsrc.php/v5/yu/l/0,cross/pmVGPoqGaceAa3Xr8bBdCv.css?_nc_x=Ij3Wp8lg5Kz",
                net::Dest::Stylesheet,
                Outcome::Loaded { status: 200 },
            ),
            entry("https://static.xx.fbcdn.net/rsrc.php/yc/r/78KLk3JFzeg.webp", net::Dest::Image, Outcome::Loaded { status: 200 }),
            LogEntry {
                list: Some("EasyPrivacy (trackers)".into()),
                cookies_sent: vec![],
                cookies_set: vec![],
                ..entry(
                    "https://www.facebook.com/tr?id=1234567&ev=PageView&dl=https%3A%2F%2Fshop.example%2F&fbp=fb.1.1699999999.987654321abc&v=2",
                    net::Dest::Image,
                    Outcome::Blocked {
                        rule: "||facebook.com/tr^".into(),
                    },
                )
            },
        ];
        let bm = |u: &str, t: &str| Bookmark {
            url: u.into(),
            title: t.into(),
        };
        let marks = vec![
            bm("https://www.facebook.com/?_fb_noscript=1", "Facebook"),
            bm("https://lobste.rs/", "Lobsters"),
            bm("https://example.com/a/very/long/path", ""),
        ];
        let w = c.size().0;
        let pw = 340.0;
        draw_chrome(&mut c, &ui, &log, 0.0, 0.0, 300.0, w - pw, &mut cache);
        draw_menu_bar(&mut c, &ui, &mut cache);
        draw_bookmarks(
            &mut c,
            &marks,
            &ui.address.text,
            Some((90.0, TOP_H + 10.0)),
            &mut cache,
        );
        let info = Info {
            url: "https://shop.example/".into(),
            opener: None,
            title: String::new(),
            log: log.clone(),
            interstitial: None,
            resubmit: None,
            note: brickle::explain::fingerprinting([
                "navigator.platform",
                "navigator.languages",
                "navigator.hardwareConcurrency",
                "screen.width",
                "screen.colorDepth",
                "timezone",
                "devicePixelRatio",
            ]),
        };
        draw_panel(&mut c, &info, None, 0.0, w - pw, &mut cache);
        c.write_png(&std::env::var("PANEL_PNG").unwrap_or("panel.png".into()))
            .unwrap();
    }
}
