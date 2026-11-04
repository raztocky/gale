//! Text selection on a rendered page: where each character is, and the text
//! between two points. This runs in the browser process, on text runs the
//! renderer sent (and `ipc` checked).

use crate::layout::Rect;
use std::collections::BTreeMap;

/// A piece of text as painted, in page coordinates. `offsets` holds the x
/// of each character boundary relative to `rect.x`: one more than the
/// number of characters, starting at 0 and never decreasing.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub rect: Rect,
    pub text: String,
    pub offsets: Vec<f32>,
}

impl TextRun {
    fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    fn x_at(&self, i: usize) -> f32 {
        self.rect.x + self.offsets.get(i).copied().unwrap_or(0.0)
    }
}

/// A point between two characters: the run, then the boundary in it
/// (0 to its number of characters). Runs are in reading order, so
/// positions compare in reading order too.
pub type Pos = (usize, usize);

/// Rectangles indexed by 64px horizontal band, so looking up what's at a
/// point checks a few of them instead of every one on the page. Only bands
/// with something in them are visited, however far apart they are (a
/// hostile renderer can put text anywhere).
#[derive(Debug, Default)]
pub struct Bands {
    map: BTreeMap<i64, Vec<usize>>,
    /// Rectangles too tall to list in every band they cross.
    tall: Vec<usize>,
}

const BAND_H: f32 = 64.0;
/// Bands one rectangle is listed in at most; taller ones go in `tall`, and
/// past `MAX_TALL` of those, the rest aren't found.
const MAX_SPAN: i64 = 64;
const MAX_TALL: usize = 4096;

fn band(y: f32) -> i64 {
    // Far enough out that sums and differences of bands can't overflow
    // (NaN goes to 0).
    (y / BAND_H).floor().clamp(-1e15, 1e15) as i64
}

impl Bands {
    pub fn new(rects: impl IntoIterator<Item = Rect>) -> Bands {
        let mut b = Bands::default();
        for (i, r) in rects.into_iter().enumerate() {
            let (top, bottom) = (band(r.y), band(r.y + r.h));
            if bottom - top > MAX_SPAN {
                if b.tall.len() < MAX_TALL {
                    b.tall.push(i);
                }
            } else {
                for k in top..=bottom {
                    b.map.entry(k).or_default().push(i);
                }
            }
        }
        b
    }

    /// The rectangles that may reach height `y`, in their original order.
    pub fn at(&self, y: f32) -> Vec<usize> {
        self.between(y, y)
    }

    /// The rectangles that may reach heights `y0..=y1`, in order.
    pub fn between(&self, y0: f32, y1: f32) -> Vec<usize> {
        let (b0, b1) = (band(y0), band(y1));
        let mut v = self.tall.clone();
        if b0 <= b1 {
            for ix in self.map.range(b0..=b1).map(|(_, ix)| ix) {
                v.extend(ix);
            }
        }
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// The boundary nearest a page point: on the line under it if there is one,
/// else on the nearest line. `bands` indexes the runs' rectangles.
pub fn hit(runs: &[TextRun], bands: &Bands, x: f32, y: f32) -> Option<Pos> {
    let gap = |v: f32, lo: f32, hi: f32| {
        if v < lo {
            lo - v
        } else if v > hi {
            v - hi
        } else {
            0.0
        }
    };
    let dist = |r: &Rect| (gap(y, r.y, r.y + r.h), gap(x, r.x, r.x + r.w));
    // The nearest run, ties going to the earliest, looking outwards from `y`
    // through the bands with runs in them until no further band could hold
    // a nearer one.
    let mut best: Option<((f32, f32), usize)> = None;
    let consider = |best: &mut Option<((f32, f32), usize)>, i: usize| {
        if let Some(r) = runs.get(i) {
            let d = (dist(&r.rect), i);
            if best.is_none_or(|b| d.partial_cmp(&b) == Some(std::cmp::Ordering::Less)) {
                *best = Some(d);
            }
        }
    };
    bands.tall.iter().for_each(|&i| consider(&mut best, i));
    let b0 = band(y);
    let mut up = bands.map.range(..b0).rev().peekable();
    let mut down = bands.map.range(b0..).peekable();
    loop {
        // Whichever side's next band is closer.
        let next = match (up.peek(), down.peek()) {
            (None, None) => break,
            (Some((&ku, _)), Some((&kd, _))) => {
                if b0 - ku < kd - b0 {
                    up.next()
                } else {
                    down.next()
                }
            }
            (Some(_), None) => up.next(),
            (None, Some(_)) => down.next(),
        };
        let Some((&kb, ix)) = next else { break };
        // Runs listed in this band or further out are at least this many
        // bands away (less one, for where in its band `y` is).
        let k = (kb - b0).abs() - 1;
        if best.is_some_and(|((dy, _), _)| dy <= k as f32 * BAND_H) {
            break;
        }
        ix.iter().for_each(|&i| consider(&mut best, i));
    }
    let (_, i) = best?;
    let run = &runs[i];
    let lx = x - run.rect.x;
    let n = run.len();
    let o = &run.offsets;
    let c = (0..n).find(|&k| lx < (o[k] + o[k + 1]) / 2.0).unwrap_or(n);
    Some((i, c))
}

fn ordered(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Is `b` on a later line than `a` (they don't share most of a line)?
fn new_line(a: &Rect, b: &Rect) -> bool {
    let overlap = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    overlap < a.h.min(b.h) * 0.5
}

/// Each selected run with the range of characters selected in it.
fn spans(runs: &[TextRun], a: Pos, b: Pos) -> impl Iterator<Item = (&TextRun, usize, usize)> {
    let (a, b) = ordered(a, b);
    let last = b.0.min(runs.len().saturating_sub(1));
    runs.get(a.0..=last)
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(move |(k, r)| {
            let i = a.0 + k;
            let from = if i == a.0 { a.1.min(r.len()) } else { 0 };
            let to = if i == b.0 { b.1.min(r.len()) } else { r.len() };
            (r, from, to.max(from))
        })
}

/// Can `c` go on the clipboard? Characters that are invisible but change
/// what text means or how it shows are dropped, so pasted text is what you
/// saw: control characters, direction overrides (which can make `exe.txt`
/// show as `txt.exe`), zero-width spaces and joiners-that-aren't, soft
/// hyphens, and Unicode tag characters (used to hide text in plain sight).
/// Zero-width (non-)joiners stay: emoji and some scripts need them.
fn copyable(c: char) -> bool {
    !matches!(c,
        '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}'
        | '\u{ad}' | '\u{61c}' | '\u{180e}'
        | '\u{200b}' | '\u{200e}' | '\u{200f}'
        | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}'
        | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}')
}

/// The selected text, with a newline wherever the selection moves to a new
/// line, no spaces at the ends of lines, and no invisible characters.
pub fn text(runs: &[TextRun], a: Pos, b: Pos) -> String {
    let mut out = String::new();
    let mut prev: Option<&Rect> = None;
    for (r, from, to) in spans(runs, a, b) {
        if prev.is_some_and(|p| new_line(p, &r.rect)) {
            out.truncate(out.trim_end_matches(' ').len());
            out.push('\n');
        }
        prev = Some(&r.rect);
        out.extend(r.text.chars().skip(from).take(to - from).filter(|c| copyable(*c)));
    }
    out.trim_end().to_string()
}

/// The rectangles to highlight, for the runs between `y0` and `y1`.
pub fn highlights(runs: &[TextRun], bands: &Bands, a: Pos, b: Pos, y0: f32, y1: f32) -> Vec<Rect> {
    let (a, b) = ordered(a, b);
    bands
        .between(y0, y1)
        .into_iter()
        .filter(|&i| i >= a.0 && i <= b.0)
        .filter_map(|i| {
            let r = runs.get(i)?;
            let from = if i == a.0 { a.1.min(r.len()) } else { 0 };
            let to = if i == b.0 { b.1.min(r.len()) } else { r.len() };
            let x = r.x_at(from);
            let w = r.x_at(to) - x;
            (to > from && w > 0.0 && r.rect.y < y1 && r.rect.y + r.rect.h > y0).then_some(Rect {
                x,
                y: r.rect.y,
                w,
                h: r.rect.h,
            })
        })
        .collect()
}

/// The word around `p`: its run, plus the runs it touches on the same line
/// with no space between (a word in two styles, or split at a hyphen).
pub fn word(runs: &[TextRun], p: Pos) -> Option<(Pos, Pos)> {
    let run = runs.get(p.0)?;
    let is_word = |r: &TextRun| !r.text.trim().is_empty();
    let joined = |a: &TextRun, b: &TextRun| {
        is_word(a)
            && is_word(b)
            && !new_line(&a.rect, &b.rect)
            && (a.rect.x + a.rect.w - b.rect.x).abs() < 0.5
    };
    let (mut start, mut end) = (p.0, p.0);
    if is_word(run) {
        while start > 0 && joined(&runs[start - 1], &runs[start]) {
            start -= 1;
        }
        while end + 1 < runs.len() && joined(&runs[end], &runs[end + 1]) {
            end += 1;
        }
    }
    Some(((start, 0), (end, runs[end].len())))
}

/// The page's text flattened for searching: lowercased characters, each
/// with where it came from. A space stands in between lines.
pub struct FindText {
    chars: Vec<char>,
    at: Vec<Pos>,
}

/// Most matches found, so a one-letter search on a huge page stays quick.
pub const MAX_MATCHES: usize = 1000;

impl FindText {
    pub fn new(runs: &[TextRun]) -> FindText {
        let mut t = FindText {
            chars: Vec::new(),
            at: Vec::new(),
        };
        let mut prev: Option<&Rect> = None;
        for (i, r) in runs.iter().enumerate() {
            if prev.is_some_and(|p| new_line(p, &r.rect))
                && t.chars.last().is_some_and(|c| !c.is_whitespace())
            {
                t.chars.push(' ');
                t.at.push((i, 0));
            }
            prev = Some(&r.rect);
            for (k, c) in r.text.chars().enumerate().filter(|(_, c)| copyable(*c)) {
                t.chars.push(fold(c));
                t.at.push((i, k));
            }
        }
        t
    }

    /// Where `query` appears (ignoring case), as selections, in reading order.
    pub fn find(&self, runs: &[TextRun], query: &str) -> Vec<(Pos, Pos)> {
        let q: Vec<char> = query.chars().map(fold).collect();
        if q.is_empty() || q.len() > self.chars.len() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i + q.len() <= self.chars.len() && out.len() < MAX_MATCHES {
            if self.chars[i..i + q.len()] == q[..] {
                let start = self.at[i];
                // Just past the last character matched.
                let (run, k) = self.at[i + q.len() - 1];
                let end = (run, (k + 1).min(runs.get(run).map_or(0, |r| r.len())));
                out.push((start, end));
                i += q.len();
            } else {
                i += 1;
            }
        }
        out
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Everything on the page.
pub fn all(runs: &[TextRun]) -> Option<(Pos, Pos)> {
    let last = runs.len().checked_sub(1)?;
    Some(((0, 0), (last, runs[last].len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs of 10px-wide characters; each item is (text, x, line).
    fn runs(items: &[(&str, f32, usize)]) -> Vec<TextRun> {
        items
            .iter()
            .map(|&(t, x, line)| TextRun {
                rect: Rect {
                    x,
                    y: line as f32 * 20.0,
                    w: t.chars().count() as f32 * 10.0,
                    h: 18.0,
                },
                text: t.into(),
                offsets: (0..=t.chars().count()).map(|i| i as f32 * 10.0).collect(),
            })
            .collect()
    }

    fn two_lines() -> Vec<TextRun> {
        runs(&[
            ("Hello", 0.0, 0),
            (" ", 50.0, 0),
            ("world", 60.0, 0),
            ("again", 0.0, 1),
        ])
    }

    fn bands(r: &[TextRun]) -> Bands {
        Bands::new(r.iter().map(|t| t.rect))
    }

    #[test]
    fn hits_the_nearest_boundary() {
        let r = two_lines();
        let b = bands(&r);
        assert_eq!(hit(&r, &b, 3.0, 5.0), Some((0, 0)));
        assert_eq!(hit(&r, &b, 7.0, 5.0), Some((0, 1)));
        assert_eq!(hit(&r, &b, 64.0, 5.0), Some((2, 0)));
        // Past the end of a line, and between lines.
        assert_eq!(hit(&r, &b, 500.0, 5.0), Some((2, 5)));
        assert_eq!(hit(&r, &b, 22.0, 30.0), Some((3, 2)));
        assert_eq!(hit(&r, &b, 22.0, 500.0), Some((3, 2)));
        assert_eq!(hit(&[], &Bands::default(), 0.0, 0.0), None);
    }

    #[test]
    fn selected_text_in_either_direction() {
        let r = two_lines();
        assert_eq!(text(&r, (0, 1), (2, 3)), "ello wor");
        assert_eq!(text(&r, (2, 3), (0, 1)), "ello wor");
        assert_eq!(text(&r, (0, 0), (3, 5)), "Hello world\nagain");
        assert_eq!(text(&r, (0, 2), (0, 2)), "");
    }

    #[test]
    fn line_ends_lose_their_spaces() {
        let r = runs(&[("a", 0.0, 0), (" ", 10.0, 0), ("b", 0.0, 1)]);
        assert_eq!(text(&r, (0, 0), (2, 1)), "a\nb");
    }

    #[test]
    fn highlight_only_whats_selected_and_visible() {
        let r = two_lines();
        let b = bands(&r);
        let h = highlights(&r, &b, (0, 1), (3, 2), 0.0, 1000.0);
        assert_eq!(h.len(), 4);
        assert_eq!((h[0].x, h[0].w), (10.0, 40.0));
        assert_eq!((h[3].x, h[3].w), (0.0, 20.0));
        assert_eq!(highlights(&r, &b, (0, 1), (3, 2), 19.0, 40.0).len(), 1);
    }

    #[test]
    fn double_click_selects_a_word() {
        // "foo" + "bar" in another style touch; the space doesn't.
        let r = runs(&[
            ("foo", 0.0, 0),
            ("bar", 30.0, 0),
            (" ", 60.0, 0),
            ("baz", 70.0, 0),
        ]);
        assert_eq!(word(&r, (1, 1)), Some(((0, 0), (1, 3))));
        assert_eq!(word(&r, (3, 0)), Some(((3, 0), (3, 3))));
        assert_eq!(all(&r), Some(((0, 0), (3, 3))));
    }

    #[test]
    fn invisible_characters_are_not_copied() {
        // A direction override, a zero-width space, a soft hyphen, an escape
        // and tag characters spelling "hi"; a zero-width joiner stays.
        let t = "a\u{202e}b\u{200b}c\u{ad}d\u{1b}e\u{e0068}\u{e0069}f\u{200d}g\tz";
        let r = runs(&[(t, 0.0, 0)]);
        let n = t.chars().count();
        assert_eq!(text(&r, (0, 0), (0, n)), "abcdef\u{200d}g\tz");
    }

    #[test]
    fn indexed_hit_matches_checking_every_run() {
        // Lines of words, a tall block, and runs far down the page.
        let mut items: Vec<(String, f32, usize)> = Vec::new();
        for line in 0..40 {
            for w in 0..6 {
                items.push((
                    format!("w{line}x{w}"),
                    w as f32 * 60.0 + (line % 3) as f32 * 7.0,
                    line,
                ));
            }
        }
        items.push(("far".into(), 30.0, 5000));
        let items: Vec<(&str, f32, usize)> = items.iter().map(|(t, x, l)| (t.as_str(), *x, *l)).collect();
        let mut r = runs(&items);
        r.push(TextRun {
            rect: Rect {
                x: 500.0,
                y: 0.0,
                w: 10.0,
                h: 20_000.0,
            },
            text: "t".into(),
            offsets: vec![0.0, 10.0],
        });
        let b = bands(&r);
        let all = Bands {
            tall: (0..r.len()).collect(),
            ..Bands::default()
        };
        for y in (-200..120_000).step_by(37) {
            for x in [-50.0, 3.0, 61.0, 200.0, 333.3, 700.0] {
                let y = y as f32;
                assert_eq!(hit(&r, &b, x, y), hit(&r, &all, x, y), "at {x},{y}");
            }
        }
    }

    #[test]
    fn find_across_runs_and_lines() {
        // "Hello" + " " + "world" on one line, "again" on the next.
        let r = two_lines();
        let f = FindText::new(&r);
        assert_eq!(f.find(&r, "WORLD"), vec![((2, 0), (2, 5))]);
        assert_eq!(f.find(&r, "lo wo"), vec![((0, 3), (2, 2))]);
        // A line break counts as a space.
        assert_eq!(f.find(&r, "world again"), vec![((2, 0), (3, 5))]);
        assert_eq!(f.find(&r, "a").len(), 2);
        assert!(f.find(&r, "").is_empty() && f.find(&r, "missing").is_empty());
        let many = runs(&[(&"ab".repeat(5000), 0.0, 0)]);
        assert_eq!(FindText::new(&many).find(&many, "a").len(), MAX_MATCHES);
    }

    #[test]
    fn runs_far_apart_dont_hang() {
        // Found by fuzzing: bands were walked one by one between the
        // furthest runs, which a hostile renderer can put 10^30 px apart.
        let run = |y: f32, h: f32| TextRun {
            rect: Rect {
                x: 0.0,
                y,
                w: 10.0,
                h,
            },
            text: "a".into(),
            offsets: vec![0.0, 10.0],
        };
        let r = vec![
            run(-1e30, 10.0),
            run(1e30, 10.0),
            run(0.0, f32::INFINITY),
            run(f32::NAN, 1.0),
        ];
        let b = bands(&r);
        for y in [0.0, -1e30, 1e30, 5e29, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let _ = hit(&r, &b, 5.0, y);
        }
        assert_eq!(hit(&r, &b, 2.0, 1e30 + 1.0), Some((1, 0)));
        let _ = highlights(&r, &b, (0, 0), (3, 1), f32::NEG_INFINITY, f32::INFINITY);
        assert!(b.between(f32::NEG_INFINITY, f32::INFINITY).len() >= 2);
    }

    #[test]
    fn out_of_range_positions_are_safe() {
        let r = two_lines();
        assert_eq!(text(&r, (0, 99), (99, 99)), " world\nagain");
        assert!(highlights(&r, &bands(&r), (99, 0), (99, 0), 0.0, 100.0).is_empty());
        assert_eq!(word(&r, (99, 0)), None);
    }
}
