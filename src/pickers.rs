//! Date and colour pickers for `<input type=date>` and `<input type=color>`:
//! a month calendar or a palette under the field, drawn into the page like
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

fn input_type(e: &dom::ElementData) -> Option<String> {
    (e.tag == "input").then(|| {
        e.attrs
            .get("type")
            .map(|t| t.trim().to_ascii_lowercase())
            .unwrap_or_default()
    })
}

pub fn is_date(e: &dom::ElementData) -> bool {
    input_type(e).as_deref() == Some("date")
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

/// The month a field's calendar opens on: its date's, or else today's.
pub fn opening_month(value: &str) -> (i32, u32) {
    let (y, m, _) = parse_date(value).unwrap_or_else(today);
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

/// The calendar for a date field showing `month`; days outside the
/// field's `min`/`max` are greyed out and can't be picked.
pub fn calendar(e: &dom::ElementData, month: (i32, u32)) -> Node {
    let (y, m) = month;
    let chosen = e.attrs.get("value").and_then(|v| parse_date(v));
    let min = e.attrs.get("min").and_then(|v| parse_date(v));
    let max = e.attrs.get("max").and_then(|v| parse_date(v));
    let now = today();
    let cell = |label: &str, class: &str, uid: u32| el("brickle-cell", class, uid, vec![text(label)]);
    let mut rows = vec![el(
        "brickle-row",
        "-brickle-cal-head",
        0,
        vec![
            cell("‹", "-brickle-cell -brickle-cal-nav", PREV),
            el(
                "brickle-cell",
                "-brickle-cal-title",
                0,
                vec![text(&format!("{} {y}", MONTHS[(m - 1) as usize]))],
            ),
            cell("›", "-brickle-cell -brickle-cal-nav", NEXT),
        ],
    )];
    rows.push(el(
        "brickle-row",
        "-brickle-cal-row",
        0,
        ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
            .iter()
            .map(|d| cell(d, "-brickle-cell -brickle-cal-day", 0))
            .collect(),
    ));
    let lead = weekday((y, m, 1));
    let days = days_in_month(y, m);
    let mut week: Vec<Node> = (0..lead).map(|_| cell("\u{a0}", "-brickle-cell", 0)).collect();
    for d in 1..=days {
        let date = (y, m, d);
        let off = min.is_some_and(|lo| date < lo) || max.is_some_and(|hi| date > hi);
        let mut class = "-brickle-cell".to_string();
        if chosen == Some(date) {
            class.push_str(" -brickle-chosen");
        } else if date == now {
            class.push_str(" -brickle-cal-today");
        }
        if off {
            class.push_str(" -brickle-off");
        }
        week.push(cell(&d.to_string(), &class, if off { 0 } else { BASE + d }));
        if week.len() == 7 {
            rows.push(el(
                "brickle-row",
                "-brickle-cal-row",
                0,
                std::mem::take(&mut week),
            ));
        }
    }
    if !week.is_empty() {
        while week.len() < 7 {
            week.push(cell("\u{a0}", "-brickle-cell", 0));
        }
        rows.push(el("brickle-row", "-brickle-cal-row", 0, week));
    }
    rows.push(el(
        "brickle-row",
        "-brickle-cal-foot",
        0,
        vec![
            cell("Today", "-brickle-cell -brickle-cal-button", TODAY),
            cell("Clear", "-brickle-cell -brickle-cal-button", CLEAR),
        ],
    ));
    el("brickle-options", "-brickle-options -brickle-calendar", 0, rows)
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
    if !is_date(e) {
        return Pick::Nothing;
    }
    let min = e.attrs.get("min").and_then(|v| parse_date(v));
    let max = e.attrs.get("max").and_then(|v| parse_date(v));
    let allowed = |d: Date| !min.is_some_and(|lo| d < lo) && !max.is_some_and(|hi| d > hi);
    match uid {
        PREV => Pick::Month(shift_month(month, -1)),
        NEXT => Pick::Month(shift_month(month, 1)),
        CLEAR => Pick::Set(String::new()),
        TODAY if allowed(today()) => Pick::Set(format_date(today())),
        u if (BASE + 1..=BASE + 31).contains(&u) => {
            let d = (month.0, month.1, u - BASE);
            if d.2 <= days_in_month(month.0, month.1) && allowed(d) {
                Pick::Set(format_date(d))
            } else {
                Pick::Nothing
            }
        }
        _ => Pick::Nothing,
    }
}

/// Up/Down in a date field: a day later or earlier (from today if empty),
/// within `min` and `max`.
pub fn step_date(value: &str, e: &dom::ElementData, up: bool) -> Option<String> {
    let next = match parse_date(value) {
        Some(d) => add_days(d, if up { 1 } else { -1 }),
        None => today(),
    };
    let min = e.attrs.get("min").and_then(|v| parse_date(v));
    let max = e.attrs.get("max").and_then(|v| parse_date(v));
    let next = match (min, max) {
        (Some(lo), _) if next < lo => lo,
        (_, Some(hi)) if next > hi => hi,
        _ => next,
    };
    let s = format_date(next);
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
        assert_eq!(step_date("2026-04-05", e, true), None, "stays at max");
        assert_eq!(step_date("2026-03-11", e, false).as_deref(), Some("2026-03-10"));
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
