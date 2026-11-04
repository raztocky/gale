#![no_main]
//! Selection and find-in-page run in the browser on text runs a possibly
//! compromised renderer sent. Any runs `ipc` would accept (offsets start at
//! 0 and never decrease, but may be huge, infinite or NaN, and needn't match
//! where the rects are) must never panic, whatever is clicked or searched.
use brickle::layout::Rect;
use brickle::select::{self, Bands, FindText, TextRun};
use libfuzzer_sys::fuzz_target;

struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn u8(&mut self) -> u8 {
        let (b, rest) = self.0.split_first().map_or((0, &[][..]), |(b, r)| (*b, r));
        self.0 = rest;
        b
    }
    fn f32(&mut self) -> f32 {
        // Mostly small, ordinary numbers, sometimes any bit pattern.
        match self.u8() % 8 {
            0 => f32::from_bits(u32::from_le_bytes([self.u8(), self.u8(), self.u8(), self.u8()])),
            _ => (self.u8() as i8) as f32 * 4.0,
        }
    }
    fn text(&mut self) -> String {
        let n = (self.u8() % 12) as usize;
        let chars = ['a', 'B', ' ', 'é', '中', '\u{200b}', '\n', '\u{ad}', 'ß', 'İ', '🐈', '-'];
        (0..n).map(|_| chars[(self.u8() % chars.len() as u8) as usize]).collect()
    }
}

fuzz_target!(|data: &[u8]| {
    let mut b = Bytes(data);
    let mut runs = Vec::new();
    for _ in 0..(b.u8() % 24) {
        let rect = Rect {
            x: b.f32(),
            y: b.f32(),
            w: b.f32(),
            h: b.f32(),
        };
        let text = b.text();
        let mut offsets = vec![0.0f32];
        for _ in 0..text.chars().count() {
            let last = *offsets.last().unwrap();
            offsets.push(last + b.f32().abs());
        }
        runs.push(TextRun { rect, text, offsets });
    }
    let bands = Bands::new(runs.iter().map(|r| r.rect));
    let mut points = Vec::new();
    for _ in 0..4 {
        let (x, y) = (b.f32(), b.f32());
        points.extend(select::hit(&runs, &bands, x, y));
    }
    if let Some((a, z)) = select::all(&runs) {
        points.push(a);
        points.push(z);
    }
    for &p in &points {
        if let Some((a, z)) = select::word(&runs, p) {
            let _ = select::text(&runs, a, z);
        }
        for &q in &points {
            let (a, z) = if p <= q { (p, q) } else { (q, p) };
            let _ = select::text(&runs, a, z);
            let _ = select::highlights(&runs, &bands, a, z, b.f32(), b.f32());
        }
    }
    let _ = bands.between(b.f32(), b.f32());
    let find = FindText::new(&runs);
    let query = b.text();
    for (a, z) in find.find(&runs, &query) {
        assert!(a <= z);
        let _ = select::text(&runs, a, z);
        let _ = select::highlights(&runs, &bands, a, z, f32::NEG_INFINITY, f32::INFINITY);
    }
});
