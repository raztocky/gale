//! Bundled fonts (DejaVu) and text measurement.

use crate::style::{ComputedStyle, Family};
use fontdue::{Font, FontSettings};
use std::sync::OnceLock;

static FONT_DATA: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/DejaVuSans.ttf"),
    include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf"),
    include_bytes!("../assets/fonts/DejaVuSerif.ttf"),
    include_bytes!("../assets/fonts/DejaVuSerif-Bold.ttf"),
    include_bytes!("../assets/fonts/DejaVuSansMono.ttf"),
    include_bytes!("../assets/fonts/DejaVuSansMono-Bold.ttf"),
];

pub fn fonts() -> &'static [Font] {
    static FONTS: OnceLock<Vec<Font>> = OnceLock::new();
    FONTS.get_or_init(|| {
        FONT_DATA
            .iter()
            .map(|d| Font::from_bytes(*d, FontSettings::default()).expect("bundled font"))
            .collect()
    })
}

pub fn index(style: &ComputedStyle) -> usize {
    let fam = match style.family {
        Family::Sans => 0,
        Family::Serif => 2,
        Family::Mono => 4,
    };
    fam + style.bold as usize
}

pub fn measure(font: usize, size: f32, text: &str) -> f32 {
    let f = &fonts()[font];
    text.chars().map(|c| f.metrics(c, size).advance_width).sum()
}

/// (ascent, descent) in px, both positive.
pub fn vmetrics(font: usize, size: f32) -> (f32, f32) {
    match fonts()[font].horizontal_line_metrics(size) {
        Some(m) => (m.ascent, -m.descent),
        None => (size * 0.8, size * 0.2),
    }
}
