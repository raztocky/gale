//! Bundled fonts and text measurement.
//!
//! Sixteen primary fonts: Liberation Sans, Serif and Mono (the same letter
//! widths as Arial, Times New Roman and Courier New, so pages lay out as
//! their authors expect) and Inter (for `system-ui` and friends), each in
//! regular, bold, italic and bold italic. Every copy of Brickle has exactly
//! these fonts and never looks at the system's, so fonts can't tell users
//! apart.
//!
//! Characters they lack fall back, in order, to DejaVu Sans (Greek, Hebrew,
//! Arabic, many symbols), then Noto subsets for Chinese/Japanese, Korean,
//! Devanagari, Thai, (monochrome) emoji and symbols; see
//! `assets/fonts/fallback/build.sh`. Fonts are parsed lazily, so a page only
//! pays for the CJK font (about 250 ms) if it actually contains CJK text.

use crate::style::{ComputedStyle, Family};
use fontdue::{Font, FontSettings};
use std::sync::OnceLock;

/// Number of fonts: indices 0..PRIMARY are the styled families (four per
/// family: regular, bold, italic, bold italic), the rest are fallbacks.
pub const COUNT: usize = 24;
pub const PRIMARY: usize = 16;
/// DejaVu Sans, the first fallback (regular; bold follows it).
const DEJAVU: usize = PRIMARY;

static FONT_DATA: [&[u8]; COUNT] = [
    include_bytes!("../assets/fonts/LiberationSans-Regular.ttf"),
    include_bytes!("../assets/fonts/LiberationSans-Bold.ttf"),
    include_bytes!("../assets/fonts/LiberationSans-Italic.ttf"),
    include_bytes!("../assets/fonts/LiberationSans-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-Regular.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-Bold.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-Italic.ttf"),
    include_bytes!("../assets/fonts/LiberationSerif-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-Regular.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-Bold.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-Italic.ttf"),
    include_bytes!("../assets/fonts/LiberationMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../assets/fonts/Inter-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/DejaVuSans.ttf"),
    include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf"),
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

/// Parse the most used fonts now (the renderer does this before its
/// sandbox is sealed): regular and bold Sans and Serif. The rest are
/// parsed when a page first needs them.
pub fn preload() {
    for i in [0, 1, 4, 5] {
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
    // DejaVu Sans, keeping bold text bold.
    let dejavu = DEJAVU + (primary & 1);
    if has_glyph(dejavu, ch) {
        return dejavu;
    }
    (DEJAVU + 2..COUNT).find(|&i| has_glyph(i, ch)).unwrap_or(primary)
}

/// Whether italic text drawn in font `i` needs slanting by hand: the
/// fallbacks have no italic versions.
pub fn fake_italic(i: usize) -> bool {
    i >= PRIMARY
}

pub fn index(style: &ComputedStyle) -> usize {
    let fam = match style.family {
        Family::Sans => 0,
        Family::Serif => 4,
        Family::Mono => 8,
        Family::System => 12,
    };
    fam + 2 * style.italic as usize + style.bold as usize
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
        assert_eq!(resolve(4, 'a'), 4);
        assert_eq!(resolve(0, 'ж'), 0, "Liberation has Cyrillic");
        assert_eq!(resolve(4, 'ש'), 4, "Liberation Serif has Hebrew");
        assert_eq!(resolve(1, 'ش'), 17, "bold Arabic from bold DejaVu Sans");
        for (ch, want) in [
            ('中', 18),
            ('の', 18),
            ('한', 19),
            ('क', 20),
            ('ก', 21),
            ('🐈', 22),
            ('⌖', 23),
        ] {
            let got = resolve(0, ch);
            assert_eq!(got, want, "{ch}");
            assert_ne!(font(got).lookup_glyph_index(ch), 0, "{ch} has a glyph");
        }
        // Any family missing a glyph gets the same fallback chain.
        assert_eq!(resolve(15, '中'), 18);
    }

    #[test]
    fn every_style_has_its_own_font() {
        let mut s = ComputedStyle::root((800.0, 600.0));
        let mut seen = std::collections::HashSet::new();
        for family in [Family::Sans, Family::Serif, Family::Mono, Family::System] {
            for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                s.family = family;
                s.bold = bold;
                s.italic = italic;
                let i = index(&s);
                assert!(i < PRIMARY && !fake_italic(i));
                assert!(seen.insert(i));
                assert_ne!(font(i).lookup_glyph_index('g'), 0);
            }
        }
    }
}
