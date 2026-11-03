//! Text selection on a rendered page: where each character is, and the text
//! between two points. This runs in the browser process, on text runs the
//! renderer sent (and `ipc` checked).

use crate::layout::Rect;

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

/// The boundary nearest a page point: on the line under it if there is one,
/// else on the nearest line.
pub fn hit(runs: &[TextRun], x: f32, y: f32) -> Option<Pos> {
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
    let (i, run) = runs.iter().enumerate().min_by(|(_, a), (_, b)| {
        dist(&a.rect)
            .partial_cmp(&dist(&b.rect))
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;
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

/// The selected text, with a newline wherever the selection moves to a new
/// line, and no spaces at the ends of lines.
pub fn text(runs: &[TextRun], a: Pos, b: Pos) -> String {
    let mut out = String::new();
    let mut prev: Option<&Rect> = None;
    for (r, from, to) in spans(runs, a, b) {
        if prev.is_some_and(|p| new_line(p, &r.rect)) {
            out.truncate(out.trim_end_matches(' ').len());
            out.push('\n');
        }
        prev = Some(&r.rect);
        out.extend(r.text.chars().skip(from).take(to - from));
    }
    out.trim_end().to_string()
}

/// The rectangles to highlight, for the runs between `y0` and `y1`.
pub fn highlights(runs: &[TextRun], a: Pos, b: Pos, y0: f32, y1: f32) -> Vec<Rect> {
    spans(runs, a, b)
        .filter(|(r, from, to)| to > from && r.rect.y < y1 && r.rect.y + r.rect.h > y0)
        .map(|(r, from, to)| {
            let x = r.x_at(from);
            Rect {
                x,
                y: r.rect.y,
                w: r.x_at(to) - x,
                h: r.rect.h,
            }
        })
        .filter(|r| r.w > 0.0)
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

    #[test]
    fn hits_the_nearest_boundary() {
        let r = two_lines();
        assert_eq!(hit(&r, 3.0, 5.0), Some((0, 0)));
        assert_eq!(hit(&r, 7.0, 5.0), Some((0, 1)));
        assert_eq!(hit(&r, 64.0, 5.0), Some((2, 0)));
        // Past the end of a line, and between lines.
        assert_eq!(hit(&r, 500.0, 5.0), Some((2, 5)));
        assert_eq!(hit(&r, 22.0, 30.0), Some((3, 2)));
        assert_eq!(hit(&r, 22.0, 500.0), Some((3, 2)));
        assert_eq!(hit(&[], 0.0, 0.0), None);
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
        let h = highlights(&r, (0, 1), (3, 2), 0.0, 1000.0);
        assert_eq!(h.len(), 4);
        assert_eq!((h[0].x, h[0].w), (10.0, 40.0));
        assert_eq!((h[3].x, h[3].w), (0.0, 20.0));
        assert_eq!(highlights(&r, (0, 1), (3, 2), 19.0, 40.0).len(), 1);
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
    fn out_of_range_positions_are_safe() {
        let r = two_lines();
        assert_eq!(text(&r, (0, 99), (99, 99)), " world\nagain");
        assert!(highlights(&r, (99, 0), (99, 0), 0.0, 100.0).is_empty());
        assert_eq!(word(&r, (99, 0)), None);
    }
}
