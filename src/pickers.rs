//! Pickers for date, time, month, week and colour fields: a month
//! calendar, a clock, a year of months or a palette under the field, drawn into the page like
//! a select's options. Their cells have no node in the document, so they
//! get uids from a range no document reaches ([`BASE`]); a click on one is
//! handled here and never reaches the page's scripts.

use crate::dom::{self, AttrMap, Node};
use crate::js::Dom;

/// Picker cells' uids start here (far above any document's node count).
pub const BASE: u32 = 0xFFF0_0000;
const PREV: u32 = BASE + 100;
const NEXT: u32 = BASE + 101;
const TODAY: u32 = BASE + 102;
const CLEAR: u32 = BASE + 103;
/// Month `m` (1-12) of a month picker is `MONTH_CELL + m`.
const MONTH_CELL: u32 = BASE + 200;
/// Hour `h` of a clock is `HOUR + h`; minute `5i` is `MINUTE + i`.
const HOUR: u32 = BASE + 300;
const MINUTE: u32 = BASE + 400;
/// Day `d` of the shown month is `BASE + d`; swatch `i` is `SWATCH + i`.
const SWATCH: u32 = BASE + 1000;

pub fn is_cell(uid: u32) -> bool {
    uid >= BASE
}

/// The palette: rows of light to dark, then greys.
pub const PALETTE: [&str; 48] = [
    "#ffcdd2", "#ffe0b2", "#fff9c4", "#c8e6c9", "#b2ebf2", "#bbdefb", "#d1c4e9", "#f8bbd0", //
    "#ef9a9a", "#ffcc80", "#fff59d", "#a5d6a7", "#80deea", "#90caf9", "#b39ddb", "#f48fb1", //
    "#e53935", "#fb8c00", "#fdd835", "#43a047", "#00acc1", "#1e88e5", "#5e35b1", "#d81b60", //
    "#b71c1c", "#e65100", "#f9a825", "#1b5e20", "#006064", "#0d47a1", "#311b92", "#880e4f", //
    "#ffffff", "#e0e0e0", "#bdbdbd", "#9e9e9e", "#757575", "#616161", "#424242", "#000000", //
    "#ff0000", "#ff8000", "#ffff00", "#00ff00", "#00ffff", "#0000ff", "#8000ff", "#ff00ff",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A year, month (1-12) and day.
pub type Date = (i32, u32, u32);

/// The fields with a calendar or clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `yyyy-mm-dd`
    Date,
    /// `hh:mm` (with seconds if given)
    Time,
    /// `yyyy-mm-ddThh:mm` (`datetime-local`)
    DateTime,
    /// `yyyy-mm`
    Month,
    /// `yyyy-Www` (ISO weeks, Monday first)
    Week,
}

fn input_type(e: &dom::ElementData) -> Option<String> {
    (e.tag == "input").then(|| {
        e.attrs
            .get("type")
            .map(|t| t.trim().to_ascii_lowercase())
            .unwrap_or_default()
    })
}

pub fn kind(e: &dom::ElementData) -> Option<Kind> {
    match input_type(e)?.as_str() {
        "date" => Some(Kind::Date),
        "time" => Some(Kind::Time),
        "datetime-local" => Some(Kind::DateTime),
        "month" => Some(Kind::Month),
        "week" => Some(Kind::Week),
        _ => None,
    }
}

/// A date, time, month or week field.
pub fn is_date(e: &dom::ElementData) -> bool {
    kind(e).is_some()
}

pub fn is_color(e: &dom::ElementData) -> bool {
    input_type(e).as_deref() == Some("color")
}

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `yyyy-mm-dd` (a year of four or more digits), as HTML dates are written.
pub fn parse_date(s: &str) -> Option<Date> {
    let mut parts = s.trim().split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || y.len() < 4
        || y.len() > 6
        || m.len() != 2
        || d.len() != 2
        || !(y.bytes().chain(m.bytes()).chain(d.bytes())).all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (y, m, d): (i32, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    (y >= 1 && (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)).then_some((y, m, d))
}

pub fn format_date((y, m, d): Date) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

const MS_DAY: i64 = 86_400_000;
const MS_HOUR: i64 = 3_600_000;
const MS_MINUTE: i64 = 60_000;

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// `hh:mm`, `hh:mm:ss` or `hh:mm:ss.sss`, as milliseconds into the day.
fn parse_time(s: &str) -> Option<i64> {
    let mut parts = s.split(':');
    let h = digits(parts.next()?, 2).filter(|&h| h < 24)?;
    let m = digits(parts.next()?, 2).filter(|&m| m < 60)?;
    let mut ms = 0;
    if let Some(sec) = parts.next() {
        let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
        let whole = digits(whole, 2).filter(|&s| s < 60)?;
        if sec.contains('.') && (frac.is_empty() || frac.len() > 3) {
            return None;
        }
        let frac = if frac.is_empty() {
            0
        } else {
            digits(frac, frac.len())? * 10u32.pow(3 - frac.len() as u32)
        };
        ms = whole as i64 * 1000 + frac as i64;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(h as i64 * MS_HOUR + m as i64 * MS_MINUTE + ms)
}

fn format_time(ms: i64) -> String {
    let (h, m) = (ms / MS_HOUR, ms % MS_HOUR / MS_MINUTE);
    let (sec, frac) = (ms % MS_MINUTE / 1000, ms % 1000);
    match (sec, frac) {
        (0, 0) => format!("{h:02}:{m:02}"),
        (_, 0) => format!("{h:02}:{m:02}:{sec:02}"),
        _ => format!("{h:02}:{m:02}:{sec:02}.{frac:03}"),
    }
}

/// ISO weeks in a year: 53 when it starts on a Thursday (or a Wednesday
/// in a leap year).
fn weeks_in_year(y: i32) -> u32 {
    let jan1 = weekday((y, 1, 1));
    if jan1 == 3 || (leap(y) && jan1 == 2) {
        53
    } else {
        52
    }
}

/// The day the Monday of ISO week `w` of `y` falls on.
fn week_monday(y: i32, w: u32) -> i64 {
    let jan4 = (y, 1, 4);
    days_from_civil(jan4) - weekday(jan4) as i64 + 7 * (w as i64 - 1)
}

/// The ISO week a day is in.
fn iso_week(day: i64) -> (i32, u32) {
    let thursday = day + 3 - weekday(civil_from_days(day)) as i64;
    let (y, _, _) = civil_from_days(thursday);
    (y, ((thursday - days_from_civil((y, 1, 1))) / 7 + 1) as u32)
}

/// A field's value as a number that sorts like it: days for dates and
/// weeks (their Monday), months since year 0, milliseconds for times.
pub fn parse(kind: Kind, s: &str) -> Option<i64> {
    let s = s.trim();
    match kind {
        Kind::Date => parse_date(s).map(days_from_civil),
        Kind::Time => parse_time(s),
        Kind::DateTime => {
            let (d, t) = s.split_once(['T', ' '])?;
            Some(days_from_civil(parse_date(d)?) * MS_DAY + parse_time(t)?)
        }
        Kind::Month => {
            let (y, m) = s.split_once('-')?;
            let m = digits(m, 2).filter(|m| (1..=12).contains(m))?;
            let y: i32 = (y.len() >= 4 && y.len() <= 6 && y.bytes().all(|b| b.is_ascii_digit()))
                .then(|| y.parse().ok())
                .flatten()
                .filter(|&y| y >= 1)?;
            Some(y as i64 * 12 + m as i64 - 1)
        }
        Kind::Week => {
            let (y, w) = s.split_once("-W")?;
            let y: i32 = (y.len() >= 4 && y.len() <= 6 && y.bytes().all(|b| b.is_ascii_digit()))
                .then(|| y.parse().ok())
                .flatten()
                .filter(|&y| y >= 1)?;
            let w = digits(w, 2).filter(|&w| w >= 1 && w <= weeks_in_year(y))?;
            Some(week_monday(y, w))
        }
    }
}

/// The value for a number from [`parse`].
pub fn format(kind: Kind, k: i64) -> String {
    match kind {
        Kind::Date => format_date(civil_from_days(k)),
        Kind::Time => format_time(k.rem_euclid(MS_DAY)),
        Kind::DateTime => format!(
            "{}T{}",
            format_date(civil_from_days(k.div_euclid(MS_DAY))),
            format_time(k.rem_euclid(MS_DAY))
        ),
        Kind::Month => format!("{:04}-{:02}", k.div_euclid(12), k.rem_euclid(12) + 1),
        Kind::Week => {
            let (y, w) = iso_week(k);
            format!("{y:04}-W{w:02}")
        }
    }
}

/// What an empty field shows.
pub fn placeholder(kind: Kind) -> &'static str {
    match kind {
        Kind::Date => "yyyy-mm-dd",
        Kind::Time => "hh:mm",
        Kind::DateTime => "yyyy-mm-dd hh:mm",
        Kind::Month => "yyyy-mm",
        Kind::Week => "yyyy-Www",
    }
}

/// Why a value isn't one (Firefox's words where it has them).
pub fn bad_input_message(kind: Kind) -> &'static str {
    match kind {
        Kind::Date => "Please enter a valid date.",
        Kind::Time => "Please enter a valid time.",
        Kind::DateTime => "Please enter a valid date and time.",
        Kind::Month => "Please enter a valid month.",
        Kind::Week => "Please enter a valid week.",
    }
}

/// The field's `min` and `max`.
pub fn bounds(e: &dom::ElementData, kind: Kind) -> (Option<i64>, Option<i64>) {
    let b = |a: &str| e.attrs.get(a).and_then(|v| parse(kind, v));
    (b("min"), b("max"))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil((y, m, d): Date) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> Date {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// The weekday, Monday = 0.
fn weekday(date: Date) -> u32 {
    // 1970-01-01 was a Thursday.
    (days_from_civil(date) + 3).rem_euclid(7) as u32
}

pub fn add_days(date: Date, n: i64) -> Date {
    civil_from_days(days_from_civil(date) + n)
}

/// Today in UTC (pages' scripts see UTC too).
pub fn today() -> Date {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    civil_from_days(secs.div_euclid(86_400))
}

/// This month (UTC).
pub fn today_month() -> (i32, u32) {
    let (y, m, _) = today();
    (y, m)
}

fn today_days() -> i64 {
    days_from_civil(today())
}

/// The day a value falls on (none for times).
fn day_of(kind: Kind, value: &str) -> Option<i64> {
    let k = parse(kind, value)?;
    match kind {
        Kind::Date | Kind::Week => Some(k),
        Kind::DateTime => Some(k.div_euclid(MS_DAY)),
        Kind::Time | Kind::Month => None,
    }
}

/// The month a field's picker opens on: its value's, or else today's. A
/// month field's picker shows a year, as (year, 1).
pub fn opening(e: &dom::ElementData, value: &str) -> (i32, u32) {
    let kind = kind(e).unwrap_or(Kind::Date);
    if kind == Kind::Month {
        let y = parse(kind, value).map_or(today().0, |k| k.div_euclid(12) as i32);
        return (y, 1);
    }
    let (y, m, _) = civil_from_days(day_of(kind, value).unwrap_or_else(today_days));
    (y, m)
}

fn shift_month((y, m): (i32, u32), by: i32) -> (i32, u32) {
    let i = y * 12 + m as i32 - 1 + by;
    (i.div_euclid(12).max(1), (i.rem_euclid(12) + 1) as u32)
}

/// A colour field's value: `#rrggbb` in lowercase, black if it isn't one.
pub fn color_value(raw: &str) -> String {
    let t = raw.trim();
    let ok = t.len() == 7 && t.starts_with('#') && t[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        t.to_ascii_lowercase()
    } else {
        "#000000".into()
    }
}

fn el(tag: &str, class: &str, uid: u32, children: Vec<Node>) -> Node {
    let mut attrs = AttrMap::new();
    attrs.insert("class".into(), class.into());
    let mut n = dom::elem(tag.into(), attrs, children);
    n.uid = uid;
    for c in &mut n.children {
        if c.uid == 0 {
            c.uid = uid;
        }
    }
    n
}

fn text(s: &str) -> Node {
    dom::text(s.to_string())
}

fn styled(mut n: Node, style: &str) -> Node {
    if let dom::NodeType::Element(e) = &mut n.node_type {
        e.attrs.insert("style".into(), style.into());
    }
    n
}

const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn cell(label: &str, class: &str, uid: u32) -> Node {
    el("brickle-cell", class, uid, vec![text(label)])
}

fn row(class: &str, cells: Vec<Node>) -> Node {
    el("brickle-row", class, 0, cells)
}

/// The ‹ title › row.
fn head(title: &str) -> Node {
    row(
        "-brickle-cal-head",
        vec![
            cell("‹", "-brickle-cell -brickle-cal-nav", PREV),
            cell(title, "-brickle-cal-title", 0),
            cell("›", "-brickle-cell -brickle-cal-nav", NEXT),
        ],
    )
}

fn foot(today: Option<&str>) -> Node {
    let mut cells = vec![];
    if let Some(t) = today {
        cells.push(cell(t, "-brickle-cell -brickle-cal-button", TODAY));
    }
    cells.push(cell("Clear", "-brickle-cell -brickle-cal-button", CLEAR));
    row("-brickle-cal-foot", cells)
}

fn cell_class(chosen: bool, today: bool, off: bool) -> String {
    let mut class = "-brickle-cell".to_string();
    if chosen {
        class.push_str(" -brickle-chosen");
    } else if today {
        class.push_str(" -brickle-cal-today");
    }
    if off {
        class.push_str(" -brickle-off");
    }
    class
}

/// Whether a day can't be picked: outside the field's `min`/`max` (for
/// weeks, its week; for dates and times, all of it).
fn day_off(kind: Kind, (lo, hi): (Option<i64>, Option<i64>), day: i64) -> bool {
    let (first, last) = match kind {
        Kind::Week => {
            let monday = day - weekday(civil_from_days(day)) as i64;
            (monday, monday)
        }
        Kind::DateTime => (day * MS_DAY, (day + 1) * MS_DAY - 1),
        _ => (day, day),
    };
    lo.is_some_and(|lo| last < lo) || hi.is_some_and(|hi| first > hi)
}

/// The picker for field `e`, showing `month` (a month field: its year).
pub fn popup(e: &dom::ElementData, month: (i32, u32)) -> Node {
    match kind(e) {
        Some(Kind::Time) => {
            let mut rows = clock(e, Kind::Time);
            rows.push(foot(None));
            el("brickle-options", "-brickle-options -brickle-calendar", 0, rows)
        }
        Some(Kind::Month) => months(e, month.0),
        _ => calendar(e, month),
    }
}

/// A month calendar for a date, week or date-and-time field.
pub fn calendar(e: &dom::ElementData, month: (i32, u32)) -> Node {
    let kind = kind(e).unwrap_or(Kind::Date);
    let (y, m) = month;
    let value = e.attrs.get("value").map_or("", String::as_str);
    let chosen = |day: i64| match kind {
        Kind::Week => parse(kind, value).is_some_and(|mon| (mon..mon + 7).contains(&day)),
        _ => day_of(kind, value) == Some(day),
    };
    let bounds = bounds(e, kind);
    let now = today_days();
    let mut rows = vec![head(&format!("{} {y}", MONTHS[(m - 1) as usize]))];
    rows.push(row(
        "-brickle-cal-row",
        ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
            .iter()
            .map(|d| cell(d, "-brickle-cell -brickle-cal-day", 0))
            .collect(),
    ));
    let lead = weekday((y, m, 1));
    let mut week: Vec<Node> = (0..lead).map(|_| cell("\u{a0}", "-brickle-cell", 0)).collect();
    for d in 1..=days_in_month(y, m) {
        let day = days_from_civil((y, m, d));
        let off = day_off(kind, bounds, day);
        let class = cell_class(chosen(day), day == now, off);
        week.push(cell(&d.to_string(), &class, if off { 0 } else { BASE + d }));
        if week.len() == 7 {
            rows.push(row("-brickle-cal-row", std::mem::take(&mut week)));
        }
    }
    if !week.is_empty() {
        while week.len() < 7 {
            week.push(cell("\u{a0}", "-brickle-cell", 0));
        }
        rows.push(row("-brickle-cal-row", week));
    }
    if kind == Kind::DateTime {
        rows.extend(clock(e, kind));
    }
    rows.push(foot(Some(if kind == Kind::Week { "This week" } else { "Today" })));
    el("brickle-options", "-brickle-options -brickle-calendar", 0, rows)
}

/// Hours, then minutes in fives, for a time or date-and-time field.
fn clock(e: &dom::ElementData, kind: Kind) -> Vec<Node> {
    let time = time_of(kind, e.attrs.get("value").map_or("", String::as_str));
    let title = |t: &str| row("-brickle-cal-head", vec![cell(t, "-brickle-clock-title", 0)]);
    let mut rows = vec![title("Hour")];
    for r in 0..4 {
        let cells = (r * 6..r * 6 + 6)
            .map(|h| {
                let chosen = time.is_some_and(|t| t / MS_HOUR == h as i64);
                cell(&format!("{h:02}"), &cell_class(chosen, false, false), HOUR + h)
            })
            .collect();
        rows.push(row("-brickle-cal-row", cells));
    }
    rows.push(title("Minute"));
    for r in 0..2 {
        let cells = (r * 6..r * 6 + 6)
            .map(|i| {
                let chosen = time.is_some_and(|t| t % MS_HOUR / MS_MINUTE == i as i64 * 5);
                cell(
                    &format!(":{:02}", i * 5),
                    &cell_class(chosen, false, false),
                    MINUTE + i,
                )
            })
            .collect();
        rows.push(row("-brickle-cal-row", cells));
    }
    rows
}

/// A year's months, for a month field.
fn months(e: &dom::ElementData, y: i32) -> Node {
    let value = e.attrs.get("value").and_then(|v| parse(Kind::Month, v));
    let (lo, hi) = bounds(e, Kind::Month);
    let (ty, tm, _) = today();
    let now = ty as i64 * 12 + tm as i64 - 1;
    let mut rows = vec![head(&y.to_string())];
    for r in 0..4 {
        let cells = (r * 3..r * 3 + 3)
            .map(|i| {
                let k = y as i64 * 12 + i as i64;
                let off = lo.is_some_and(|lo| k < lo) || hi.is_some_and(|hi| k > hi);
                let class = cell_class(value == Some(k), k == now, off) + " -brickle-month-cell";
                cell(
                    SHORT_MONTHS[i as usize],
                    &class,
                    if off { 0 } else { MONTH_CELL + 1 + i },
                )
            })
            .collect();
        rows.push(row("-brickle-cal-row", cells));
    }
    rows.push(foot(Some("This month")));
    el("brickle-options", "-brickle-options -brickle-calendar", 0, rows)
}

/// The time part of a time or date-and-time value.
fn time_of(kind: Kind, value: &str) -> Option<i64> {
    let k = parse(kind, value)?;
    Some(match kind {
        Kind::DateTime => k.rem_euclid(MS_DAY),
        _ => k,
    })
}

/// The palette for a colour field.
pub fn palette(e: &dom::ElementData) -> Node {
    let chosen = color_value(e.attrs.get("value").map_or("", String::as_str));
    let rows = PALETTE
        .chunks(8)
        .enumerate()
        .map(|(r, row)| {
            let cells = row
                .iter()
                .enumerate()
                .map(|(c, colour)| {
                    let mut class = "-brickle-swatch-cell".to_string();
                    if *colour == chosen {
                        class.push_str(" -brickle-chosen");
                    }
                    let i = (r * 8 + c) as u32;
                    styled(
                        el("brickle-cell", &class, SWATCH + i, vec![text("\u{a0}")]),
                        &format!("background-color:{colour}"),
                    )
                })
                .collect();
            el("brickle-row", "-brickle-cal-row", 0, cells)
        })
        .collect();
    el("brickle-options", "-brickle-options -brickle-palette", 0, rows)
}

/// What a click on picker cell `uid` does to the open field.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// Set the field to this and close the picker.
    Set(String),
    /// Set the field to this and keep the picker open (a date or hour
    /// picked, a time still to come).
    Keep(String),
    /// Show another month, keeping the picker open.
    Month((i32, u32)),
    /// Not a cell of this picker.
    Nothing,
}

/// The click on `uid` in the picker of field `id`, which shows `month`.
pub fn pick(dom: &Dom, id: u32, uid: u32, month: (i32, u32)) -> Pick {
    let Some(e) = dom.element(id) else {
        return Pick::Nothing;
    };
    if is_color(e) {
        return match uid.checked_sub(SWATCH).and_then(|i| PALETTE.get(i as usize)) {
            Some(c) => Pick::Set((*c).to_string()),
            None => Pick::Nothing,
        };
    }
    let Some(kind) = kind(e) else {
        return Pick::Nothing;
    };
    let value = crate::forms::value(dom, id);
    let (lo, hi) = bounds(e, kind);
    let allowed = |k: i64| !lo.is_some_and(|lo| k < lo) && !hi.is_some_and(|hi| k > hi);
    let set = |k: i64| Pick::Set(format(kind, k));
    let has_days = matches!(kind, Kind::Date | Kind::Week | Kind::DateTime);
    let time = time_of(kind, &value);
    let day = day_of(kind, &value).unwrap_or_else(today_days);
    let at = |day: i64, t: i64| {
        if kind == Kind::DateTime {
            day * MS_DAY + t
        } else {
            t
        }
    };
    match uid {
        PREV | NEXT if kind != Kind::Time => {
            let by = if kind == Kind::Month { 12 } else { 1 };
            Pick::Month(shift_month(month, if uid == PREV { -by } else { by }))
        }
        CLEAR => Pick::Set(String::new()),
        TODAY => {
            let k = match kind {
                Kind::Date => today_days(),
                Kind::Week => today_days() - weekday(today()) as i64,
                Kind::Month => {
                    let (y, m, _) = today();
                    y as i64 * 12 + m as i64 - 1
                }
                Kind::DateTime => today_days() * MS_DAY + time.unwrap_or(0),
                Kind::Time => return Pick::Nothing,
            };
            if allowed(k) || (kind == Kind::Week && !day_off(kind, (lo, hi), k)) {
                if kind == Kind::DateTime {
                    Pick::Keep(format(kind, k))
                } else {
                    set(k)
                }
            } else {
                Pick::Nothing
            }
        }
        u if has_days && (BASE + 1..=BASE + 31).contains(&u) => {
            let d = u - BASE;
            if d > days_in_month(month.0, month.1) {
                return Pick::Nothing;
            }
            let picked = days_from_civil((month.0, month.1, d));
            if day_off(kind, (lo, hi), picked) {
                return Pick::Nothing;
            }
            match kind {
                Kind::Week => set(picked - weekday(civil_from_days(picked)) as i64),
                Kind::DateTime => Pick::Keep(format(kind, picked * MS_DAY + time.unwrap_or(0))),
                _ => set(picked),
            }
        }
        u if kind == Kind::Month && (MONTH_CELL + 1..=MONTH_CELL + 12).contains(&u) => {
            let k = month.0 as i64 * 12 + (u - MONTH_CELL - 1) as i64;
            if allowed(k) {
                set(k)
            } else {
                Pick::Nothing
            }
        }
        u if matches!(kind, Kind::Time | Kind::DateTime) && (HOUR..HOUR + 24).contains(&u) => {
            let minutes = time.map_or(0, |t| t % MS_HOUR / MS_MINUTE * MS_MINUTE);
            Pick::Keep(format(kind, at(day, (u - HOUR) as i64 * MS_HOUR + minutes)))
        }
        u if matches!(kind, Kind::Time | Kind::DateTime) && (MINUTE..MINUTE + 12).contains(&u) => {
            let hour = time.map_or(0, |t| t / MS_HOUR * MS_HOUR);
            set(at(day, hour + (u - MINUTE) as i64 * 5 * MS_MINUTE))
        }
        _ => Pick::Nothing,
    }
}

/// Up/Down in a date or time field: a step later or earlier (a day, a
/// minute, a month or a week; from now if empty), within `min` and `max`.
pub fn step(value: &str, e: &dom::ElementData, up: bool) -> Option<String> {
    let kind = kind(e)?;
    let by = match kind {
        Kind::Date | Kind::Month => 1,
        Kind::Week => 7,
        Kind::Time | Kind::DateTime => MS_MINUTE,
    };
    let next = match parse(kind, value) {
        Some(k) => k + if up { by } else { -by },
        None => match kind {
            Kind::Date => today_days(),
            Kind::Week => today_days() - weekday(today()) as i64,
            Kind::Month => {
                let (y, m, _) = today();
                y as i64 * 12 + m as i64 - 1
            }
            Kind::Time => 12 * MS_HOUR,
            Kind::DateTime => today_days() * MS_DAY + 12 * MS_HOUR,
        },
    };
    let (lo, hi) = bounds(e, kind);
    let next = match (lo, hi) {
        (Some(lo), _) if next < lo => lo,
        (_, Some(hi)) if next > hi => hi,
        _ => next,
    };
    let s = format(kind, next);
    (s != value).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_date("2026-02-29"), None, "not a leap year");
        assert_eq!(parse_date("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_date("2026-1-05"), None);
        assert_eq!(parse_date("26-01-05"), None);
        assert_eq!(weekday((2026, 11, 19)), 3, "a Thursday");
        assert_eq!(add_days((2026, 12, 31), 1), (2027, 1, 1));
        assert_eq!(add_days((2024, 3, 1), -1), (2024, 2, 29));
        for z in [-800_000, -1, 0, 1, 20_000, 2_000_000] {
            assert_eq!(days_from_civil(civil_from_days(z)), z);
        }
        assert_eq!(shift_month((2026, 1), -1), (2025, 12));
        assert_eq!(shift_month((2026, 12), 1), (2027, 1));
    }

    #[test]
    fn colours() {
        assert_eq!(color_value("#FF8000"), "#ff8000");
        assert_eq!(color_value("red"), "#000000");
        assert_eq!(color_value(""), "#000000");
    }

    fn date_input(attrs: &str) -> (Dom, u32) {
        let d = Dom::from_tree(&crate::html::parse(&format!("<input id=d type=date {attrs}>")));
        let id = crate::forms::by_id(&d, "d").unwrap();
        (d, id)
    }

    #[test]
    fn calendar_picks() {
        let (d, id) = date_input("min=2026-03-10 max=2026-04-05");
        assert_eq!(pick(&d, id, BASE + 12, (2026, 3)), Pick::Set("2026-03-12".into()));
        assert_eq!(pick(&d, id, BASE + 9, (2026, 3)), Pick::Nothing, "before min");
        assert_eq!(
            pick(&d, id, BASE + 31, (2026, 4)),
            Pick::Nothing,
            "April has 30 days"
        );
        assert_eq!(pick(&d, id, NEXT, (2026, 3)), Pick::Month((2026, 4)));
        assert_eq!(pick(&d, id, CLEAR, (2026, 3)), Pick::Set(String::new()));
        let e = d.element(id).unwrap();
        assert_eq!(step("2026-04-05", e, true), None, "stays at max");
        assert_eq!(step("2026-03-11", e, false).as_deref(), Some("2026-03-10"));
    }

    #[test]
    fn times_months_and_weeks() {
        let round = |k: Kind, s: &str| parse(k, s).map(|v| format(k, v));
        assert_eq!(round(Kind::Time, "09:05").as_deref(), Some("09:05"));
        assert_eq!(round(Kind::Time, "23:59:30.5").as_deref(), Some("23:59:30.500"));
        assert_eq!(parse(Kind::Time, "24:00"), None);
        assert_eq!(parse(Kind::Time, "9:05"), None);
        assert_eq!(
            round(Kind::DateTime, "2026-11-19 08:30").as_deref(),
            Some("2026-11-19T08:30")
        );
        assert_eq!(parse(Kind::DateTime, "2026-11-19"), None);
        assert_eq!(round(Kind::Month, "2026-02").as_deref(), Some("2026-02"));
        assert_eq!(parse(Kind::Month, "2026-13"), None);
        // 2026 starts on a Thursday, so it has a week 53; 2025 doesn't.
        assert_eq!(round(Kind::Week, "2026-W53").as_deref(), Some("2026-W53"));
        assert_eq!(parse(Kind::Week, "2025-W53"), None);
        assert_eq!(
            parse(Kind::Week, "2026-W01").map(civil_from_days),
            Some((2025, 12, 29)),
            "week 1 has the year's first Thursday"
        );
        assert_eq!(iso_week(days_from_civil((2027, 1, 1))), (2026, 53));
        assert!(parse(Kind::Time, "08:00") < parse(Kind::Time, "08:00:01"));
    }

    fn input(ty: &str, attrs: &str) -> (Dom, u32) {
        let d = Dom::from_tree(&crate::html::parse(&format!("<input id=d type={ty} {attrs}>")));
        let id = crate::forms::by_id(&d, "d").unwrap();
        (d, id)
    }

    #[test]
    fn clock_month_and_week_picks() {
        let (d, id) = input("time", "value=09:20");
        assert_eq!(pick(&d, id, HOUR + 14, (2026, 1)), Pick::Keep("14:20".into()));
        assert_eq!(pick(&d, id, MINUTE + 9, (2026, 1)), Pick::Set("09:45".into()));
        let (d, id) = input("datetime-local", "value=2026-11-19T09:20");
        assert_eq!(
            pick(&d, id, BASE + 3, (2026, 12)),
            Pick::Keep("2026-12-03T09:20".into())
        );
        assert_eq!(
            pick(&d, id, MINUTE, (2026, 11)),
            Pick::Set("2026-11-19T09:00".into())
        );
        let (d, id) = input("month", "min=2026-03");
        assert_eq!(
            pick(&d, id, MONTH_CELL + 2, (2026, 1)),
            Pick::Nothing,
            "before min"
        );
        assert_eq!(
            pick(&d, id, MONTH_CELL + 5, (2026, 1)),
            Pick::Set("2026-05".into())
        );
        assert_eq!(pick(&d, id, NEXT, (2026, 1)), Pick::Month((2027, 1)));
        let (d, id) = input("week", "");
        assert_eq!(pick(&d, id, BASE + 19, (2026, 11)), Pick::Set("2026-W47".into()));
        let e = d.element(id).unwrap();
        assert_eq!(step("2026-W53", e, true).as_deref(), Some("2027-W01"));
        let (d, id) = input("time", "max=10:00");
        assert_eq!(step("10:00", d.element(id).unwrap(), true), None);
        assert_eq!(
            step("09:00", d.element(id).unwrap(), false).as_deref(),
            Some("08:59")
        );
    }

    #[test]
    fn calendar_layout() {
        let (d, id) = date_input("value=2026-11-19");
        let cal = calendar(d.element(id).unwrap(), (2026, 11));
        let t = cal.text_content();
        assert!(t.starts_with("‹November 2026›MoTuWeThFrSaSu"), "{t}");
        // November 2026 starts on a Sunday: six blanks, then the 1st.
        let rows: Vec<&Node> = cal.children.iter().collect();
        assert_eq!(rows[2].children.len(), 7);
        assert_eq!(rows[2].children[6].text_content(), "1");
        let mut chosen = Vec::new();
        cal.walk(&mut chosen, &|n| {
            n.element()
                .and_then(|e| e.attrs.get("class"))
                .is_some_and(|c| c.contains("-brickle-chosen"))
        });
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].uid, BASE + 19);
    }
}
