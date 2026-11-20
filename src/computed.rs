//! `getComputedStyle()`: Brickle's computed style for an element, written
//! as the CSS text browsers give scripts (`rgb(255, 0, 0)`, `16px`, `700`).
//! Sizes come from the layout when the element has a box.

use crate::css::Color;
use crate::layout::Rect;
use crate::style::{
    Align, Clear, ComputedStyle, Display, Family, Float, Justify, Len, ListStyle, Position, TextAlign,
};

/// The properties scripts can list (`length`, `item()`), in alphabetical
/// order as browsers list them.
pub const NAMES: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "background-color",
    "background-image",
    "border-bottom-color",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-top-color",
    "border-top-style",
    "border-top-width",
    "bottom",
    "box-sizing",
    "clear",
    "color",
    "column-gap",
    "direction",
    "display",
    "flex-basis",
    "flex-direction",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "height",
    "justify-content",
    "left",
    "line-height",
    "list-style-type",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "opacity",
    "order",
    "overflow",
    "overflow-x",
    "overflow-y",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "pointer-events",
    "position",
    "right",
    "row-gap",
    "text-align",
    "text-decoration-line",
    "text-transform",
    "top",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "z-index",
];

fn px(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let r = (v * 100.0).round() / 100.0;
    if r == r.trunc() {
        format!("{}px", r as i64)
    } else {
        format!("{r}px")
    }
}

fn number(v: f32) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

fn colour(c: Color) -> String {
    if c.a == 255 {
        format!("rgb({}, {}, {})", c.r, c.g, c.b)
    } else {
        format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, number(c.a as f32 / 255.0))
    }
}

fn len(l: Len) -> String {
    match l {
        Len::Px(v) => px(v),
        Len::Percent(p) => format!("{}%", number(p)),
        Len::Calc(a, p) => format!("calc({} + {}%)", px(a), number(p)),
        Len::Auto => "auto".into(),
    }
}

fn justify(j: Justify) -> &'static str {
    match j {
        Justify::Start => "flex-start",
        Justify::End => "flex-end",
        Justify::Center => "center",
        Justify::SpaceBetween => "space-between",
        Justify::SpaceAround => "space-around",
        Justify::SpaceEvenly => "space-evenly",
        Justify::Stretch => "stretch",
    }
}

fn align(a: Align) -> &'static str {
    match a {
        Align::Stretch => "stretch",
        Align::Start => "flex-start",
        Align::End => "flex-end",
        Align::Center => "center",
    }
}

/// A property of `s` as a script would read it, or None for one Brickle
/// doesn't compute. `rect` is the element's border box, if it has one.
pub fn value(s: &ComputedStyle, name: &str, rect: Option<Rect>) -> Option<String> {
    // Index into top/right/bottom/left arrays.
    let side = |n: &str| match n {
        "top" => Some(0),
        "right" => Some(1),
        "bottom" => Some(2),
        "left" => Some(3),
        _ => None,
    };
    let padding_px = |i: usize, cb: f32| s.padding[i].resolve(cb).unwrap_or(0.0);
    let v = match name {
        "display" => match s.display {
            Display::Flex if s.is_grid => "grid",
            Display::InlineFlex if s.is_grid => "inline-grid",
            Display::Block => "block",
            Display::Inline => "inline",
            Display::InlineBlock => "inline-block",
            Display::ListItem => "list-item",
            Display::Flex => "flex",
            Display::InlineFlex => "inline-flex",
            Display::Table => "table",
            Display::InlineTable => "inline-table",
            Display::TableRowGroup => "table-row-group",
            Display::TableRow => "table-row",
            Display::TableCell => "table-cell",
            Display::TableCaption => "table-caption",
            Display::None => "none",
        }
        .into(),
        "position" => match s.position {
            Position::Static => "static",
            Position::Relative => "relative",
            Position::Absolute => "absolute",
            Position::Fixed => "fixed",
            Position::Sticky => "sticky",
        }
        .into(),
        "visibility" => if s.visible { "visible" } else { "hidden" }.into(),
        "color" => colour(s.color),
        "background-color" => colour(s.background),
        "background-image" => match &s.background_image {
            Some(u) => format!("url(\"{}\")", u.replace('"', "%22")),
            None => "none".into(),
        },
        "font-size" => px(s.font_size),
        "font-weight" => if s.bold { "700" } else { "400" }.into(),
        "font-style" => if s.italic { "italic" } else { "normal" }.into(),
        "font-family" => match s.family {
            Family::Sans => "sans-serif",
            Family::Serif => "serif",
            Family::Mono => "monospace",
            Family::System => "system-ui",
        }
        .into(),
        "line-height" => px(s.line_height),
        "direction" => if s.rtl { "rtl" } else { "ltr" }.into(),
        "text-align" => match s.text_align {
            TextAlign::Start => "start",
            TextAlign::End => "end",
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        }
        .into(),
        "text-decoration-line" => match (s.underline, s.strike) {
            (true, true) => "underline line-through",
            (true, false) => "underline",
            (false, true) => "line-through",
            (false, false) => "none",
        }
        .into(),
        "text-transform" => if s.uppercase { "uppercase" } else { "none" }.into(),
        "white-space" => match (s.pre, s.nowrap) {
            (true, true) => "pre",
            (true, false) => "pre-wrap",
            (false, true) => "nowrap",
            (false, false) => "normal",
        }
        .into(),
        "list-style-type" => match s.list_style {
            ListStyle::Disc => "disc",
            ListStyle::Circle => "circle",
            ListStyle::Square => "square",
            ListStyle::Decimal => "decimal",
            ListStyle::DisclosureClosed => "disclosure-closed",
            ListStyle::DisclosureOpen => "disclosure-open",
            ListStyle::None => "none",
        }
        .into(),
        "overflow" | "overflow-x" | "overflow-y" => if s.scrolls {
            "auto"
        } else if s.overflow_clip {
            "hidden"
        } else {
            "visible"
        }
        .into(),
        "z-index" => s.z_index.map_or("auto".into(), |z| z.to_string()),
        "float" => match s.float {
            Float::None => "none",
            Float::Left => "left",
            Float::Right => "right",
        }
        .into(),
        "clear" => match s.clear {
            Clear::None => "none",
            Clear::Left => "left",
            Clear::Right => "right",
            Clear::Both => "both",
        }
        .into(),
        "box-sizing" => if s.border_box { "border-box" } else { "content-box" }.into(),
        "border-collapse" => if s.border_collapse { "collapse" } else { "separate" }.into(),
        "vertical-align" => format!("{:?}", s.vertical_align).to_ascii_lowercase(),
        "flex-direction" => match (s.flex_row, s.flex_reverse) {
            (true, false) => "row",
            (true, true) => "row-reverse",
            (false, false) => "column",
            (false, true) => "column-reverse",
        }
        .into(),
        "flex-wrap" => if s.flex_wrap { "wrap" } else { "nowrap" }.into(),
        "flex-grow" => number(s.flex_grow),
        "flex-shrink" => number(s.flex_shrink),
        "flex-basis" => len(s.flex_basis),
        "order" => s.order.to_string(),
        "justify-content" => justify(s.justify).into(),
        "align-content" => justify(s.align_content).into(),
        "align-items" => align(s.align_items).into(),
        "align-self" => s.align_self.map_or("auto", align).into(),
        "column-gap" => px(s.gap),
        "row-gap" => px(s.row_gap),
        "min-height" => len(s.min_height),
        "max-height" => match s.max_height {
            Len::Auto => "none".into(),
            l => len(l),
        },
        "max-width" => match s.max_width {
            Len::Auto => "none".into(),
            l => len(l),
        },
        "opacity" => "1".into(),
        "pointer-events" => "auto".into(),
        // The used size when laid out, as browsers give it.
        "width" | "height" => {
            let horizontal = name == "width";
            match rect.filter(|_| s.display != Display::None && s.display != Display::Inline) {
                Some(r) => {
                    let (outer, a, b) = if horizontal { (r.w, 1, 3) } else { (r.h, 0, 2) };
                    if s.border_box {
                        px(outer)
                    } else {
                        let cb = r.w;
                        px((outer
                            - padding_px(a, cb)
                            - padding_px(b, cb)
                            - s.border_width[a]
                            - s.border_width[b])
                            .max(0.0))
                    }
                }
                None => len(if horizontal { s.width } else { s.height }),
            }
        }
        n => {
            if let Some(rest) = n.strip_prefix("margin-") {
                len(s.margin[side(rest)?])
            } else if let Some(rest) = n.strip_prefix("padding-") {
                len(s.padding[side(rest)?])
            } else if let Some(rest) = n.strip_prefix("border-") {
                let (which, what) = rest.split_once('-')?;
                let i = side(which)?;
                match what {
                    "width" => px(s.border_width[i]),
                    "color" => colour(s.border_color[i]),
                    "style" => if s.border_width[i] > 0.0 { "solid" } else { "none" }.into(),
                    _ => return None,
                }
            } else if let Some(i) = side(n) {
                len(s.inset[i])
            } else if n.starts_with("--") {
                s.vars
                    .get(n)
                    .map(|v| v.iter().map(|x| x.to_css()).collect::<Vec<_>>().join(" "))?
            } else {
                return None;
            }
        }
    };
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_name_has_a_value() {
        let s = ComputedStyle::root((800.0, 600.0));
        for n in NAMES {
            assert!(value(&s, n, None).is_some(), "{n}");
        }
        assert!(NAMES.windows(2).all(|w| w[0] < w[1]), "sorted");
        assert_eq!(value(&s, "color", None).as_deref(), Some("rgb(0, 0, 0)"));
        assert_eq!(
            value(&s, "background-color", None).as_deref(),
            Some("rgba(0, 0, 0, 0)")
        );
        assert_eq!(value(&s, "nonsense", None), None);
    }
}
