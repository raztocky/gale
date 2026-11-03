//! Bundled fonts and text measurement.
//!
//! Six primary fonts (DejaVu Sans/Serif/Mono, regular and bold) cover Latin,
//! Greek, Cyrillic, Hebrew, Arabic and many symbols. Characters they lack
//! fall back, in order, to Noto subsets for Chinese/Japanese, Korean,
//! Devanagari, Thai, (monochrome) emoji and symbols; see
//! `assets/fonts/fallback/build.sh`. Fonts are parsed lazily, so a page only
//! pays for the CJK font (about 250 ms) if it actually contains CJK text.

use crate::style::{ComputedStyle, Family};
use fontdue::{Font, FontSettings};
use std::sync::OnceLock;

/// Number of fonts: indices 0..PRIMARY are DejaVu, the rest are fallbacks.
pub const COUNT: usize = 12;
pub const PRIMARY: usize = 6;

static FONT_DATA: [&[u8]; COUNT] = [
    include_bytes!("../assets/fonts/DejaVuSans.ttf"),
    include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf"),
    include_bytes!("../assets/fonts/DejaVuSerif.ttf"),
    include_bytes!("../assets/fonts/DejaVuSerif-Bold.ttf"),
    include_bytes!("../assets/fonts/DejaVuSansMono.ttf"),
    include_bytes!("../assets/fonts/DejaVuSansMono-Bold.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansCJK-Subset.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansKR-Subset.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansDevanagari.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansThai.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoEmoji.ttf"),
    include_bytes!("../assets/fonts/fallback/NotoSansSymbols2.ttf"),
];

static FONTS: [OnceLock<Font>; COUNT] = [const { OnceLock::new() }; COUNT];

/// The font at `i` (parsed on first use). Out-of-range indices give font 0.
pub fn font(i: usize) -> &'static Font {
    let i = if i < COUNT { i } else { 0 };
    FONTS[i].get_or_init(|| Font::from_bytes(FONT_DATA[i], FontSettings::default()).expect("bundled font"))
}

/// Parse the primary fonts now (the renderer does this before its sandbox
/// is sealed, so normal pages never parse fonts later).
pub fn preload() {
    for i in 0..PRIMARY {
        font(i);
    }
}

/// Character coverage of the fallback fonts, read straight from their
/// `cmap` tables: deciding which font a character needs doesn't require
/// parsing the (large) font itself.
fn has_glyph(i: usize, ch: char) -> bool {
    use read_fonts::{tables::cmap::Cmap, FontRef, TableProvider};
    static CMAPS: [OnceLock<Option<Cmap<'static>>>; COUNT] = [const { OnceLock::new() }; COUNT];
    CMAPS[i]
        .get_or_init(|| FontRef::new(FONT_DATA[i]).ok().and_then(|f| f.cmap().ok()))
        .as_ref()
        .and_then(|c| c.map_codepoint(ch))
        .is_some_and(|g| g.to_u32() != 0)
}

/// The font that should draw `ch` when the style asks for `primary`.
pub fn resolve(primary: usize, ch: char) -> usize {
    if (ch as u32) < 0x80 || ch.is_whitespace() || primary >= PRIMARY {
        return primary;
    }
    if font(primary).lookup_glyph_index(ch) != 0 {
        return primary;
    }
    // DejaVu Sans covers more scripts than Serif or Mono.
    let sans = primary & 1; // regular or bold sans
    if sans != primary && font(sans).lookup_glyph_index(ch) != 0 {
        return sans;
    }
    (PRIMARY..COUNT).find(|&i| has_glyph(i, ch)).unwrap_or(primary)
}

pub fn index(style: &ComputedStyle) -> usize {
    let fam = match style.family {
        Family::Sans => 0,
        Family::Serif => 2,
        Family::Mono => 4,
    };
    fam + style.bold as usize
}

pub fn measure(font_idx: usize, size: f32, text: &str) -> f32 {
    text.chars()
        .map(|c| font(resolve(font_idx, c)).metrics(c, size).advance_width)
        .sum()
}

/// (ascent, descent) in px, both positive.
pub fn vmetrics(font_idx: usize, size: f32) -> (f32, f32) {
    match font(font_idx).horizontal_line_metrics(size) {
        Some(m) => (m.ascent, -m.descent),
        None => (size * 0.8, size * 0.2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_picks_a_font_that_has_the_glyph() {
        assert_eq!(resolve(2, 'a'), 2);
        assert_eq!(resolve(0, 'ж'), 0, "DejaVu has Cyrillic");
        assert_eq!(resolve(0, 'ש'), 0, "DejaVu has Hebrew");
        for (ch, want) in [
            ('中', 6),
            ('の', 6),
            ('한', 7),
            ('क', 8),
            ('ก', 9),
            ('🐈', 10),
            ('⌖', 11),
        ] {
            let got = resolve(0, ch);
            assert_eq!(got, want, "{ch}");
            assert_ne!(font(got).lookup_glyph_index(ch), 0, "{ch} has a glyph");
        }
        // Serif text missing a glyph still gets the sans-serif fallback chain.
        assert_eq!(resolve(3, '中'), 6);
    }
}
