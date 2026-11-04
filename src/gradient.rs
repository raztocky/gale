//! CSS gradients: `linear-gradient()`, `radial-gradient()` and their
//! `repeating-` forms, parsed when computing styles and drawn into a
//! bitmap in the renderer, which then paints it like a background image.

use crate::css::{self, Color, Unit, Value};
use crate::image::Bitmap;
use crate::style::Len;

/// Most colour stops a gradient may have.
pub const MAX_STOPS: usize = 64;
/// Longest side of a gradient's bitmap; bigger boxes stretch it, which a
/// smooth gradient hides.
pub const MAX_SIDE: u32 = 512;

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// The gradient line's direction: an angle (degrees clockwise from "to
    /// top"), or a corner (x, y, each -1 or 1), whose angle depends on the
    /// box.
    Linear { angle: f32, corner: Option<(f32, f32)> },
    Radial {
        circle: bool,
        size: Size,
        at: (Len, Len),
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    ClosestSide,
    FarthestSide,
    ClosestCorner,
    FarthestCorner,
    /// Radii (one for a circle).
    Explicit(Len, Len),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub shape: Shape,
    /// Colours and where they are (px or % along the gradient line); a
    /// missing position is worked out from its neighbours.
    pub stops: Vec<(Color, Option<Len>)>,
    pub repeating: bool,
}

/// Parse a gradient function as written (lowercased), with `em` lengths in
/// `font_size`. None if it isn't one or makes no sense.
pub fn parse(text: &str, font_size: f32) -> Option<Gradient> {
    let text = text.trim();
    let (repeating, rest) = match text.strip_prefix("repeating-") {
        Some(r) => (true, r),
        None => (false, text),
    };
    let (linear, inner) = if let Some(r) = rest.strip_prefix("linear-gradient(") {
        (true, r)
    } else {
        (false, rest.strip_prefix("radial-gradient(")?)
    };
    let inner = inner.strip_suffix(')')?;
    let args = split_top(inner, ',');
    let len = |v: &Value| match v {
        Value::Length(n, Unit::Em | Unit::Rem) => Some(Len::Px(n * font_size)),
        Value::Length(n, _) => Some(Len::Px(*n)),
        Value::Percent(p) => Some(Len::Percent(*p)),
        Value::Number(n) if *n == 0.0 => Some(Len::Px(0.0)),
        _ => None,
    };
    let mut args = args.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).peekable();
    let first = *args.peek()?;
    // Unless it starts with a colour, the first argument says which way the
    // gradient goes, and has to make sense.
    let is_stop = matches!(css::parse_values(first).first(), Some(Value::Color(_)));
    let shape = match (linear, is_stop) {
        (true, true) => Shape::Linear {
            angle: 180.0,
            corner: None,
        },
        (false, true) => Shape::Radial {
            circle: false,
            size: Size::FarthestCorner,
            at: (Len::Percent(50.0), Len::Percent(50.0)),
        },
        (true, false) => linear_direction(first)?,
        (false, false) => radial_shape(first, &len)?,
    };
    if !is_stop {
        args.next();
    }
    let mut stops = Vec::new();
    for a in args {
        let vals = css::parse_values(a);
        let Some(Value::Color(c)) = vals.first() else {
            // Colour hints (a bare position) and unknown colours.
            continue;
        };
        let at: Vec<Len> = vals[1..].iter().filter_map(len).take(2).collect();
        match at.as_slice() {
            [] => stops.push((*c, None)),
            [p] => stops.push((*c, Some(*p))),
            [p, q, ..] => {
                stops.push((*c, Some(*p)));
                stops.push((*c, Some(*q)));
            }
        }
        if stops.len() > MAX_STOPS {
            return None;
        }
    }
    if stops.is_empty() {
        return None;
    }
    if stops.len() == 1 {
        stops.push(stops[0]);
    }
    Some(Gradient {
        shape,
        stops,
        repeating,
    })
}

/// Split at `sep` outside parentheses.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn linear_direction(arg: &str) -> Option<Shape> {
    if let Some(sides) = arg.strip_prefix("to ") {
        let (mut x, mut y) = (0.0f32, 0.0f32);
        for w in sides.split_whitespace() {
            match w {
                "left" => x = -1.0,
                "right" => x = 1.0,
                "top" => y = -1.0,
                "bottom" => y = 1.0,
                _ => return None,
            }
        }
        return Some(match (x, y) {
            (0.0, 0.0) => return None,
            (0.0, y) => Shape::Linear {
                angle: if y < 0.0 { 0.0 } else { 180.0 },
                corner: None,
            },
            (x, 0.0) => Shape::Linear {
                angle: if x > 0.0 { 90.0 } else { 270.0 },
                corner: None,
            },
            (x, y) => Shape::Linear {
                angle: 0.0,
                corner: Some((x, y)),
            },
        });
    }
    let angle = |unit: &str, per_turn: f32| {
        arg.strip_suffix(unit)
            .and_then(|n| n.parse::<f32>().ok())
            .filter(|n| n.is_finite())
            .map(|n| n * 360.0 / per_turn)
    };
    let deg = angle("deg", 360.0)
        .or_else(|| angle("grad", 400.0))
        .or_else(|| angle("rad", std::f32::consts::TAU))
        .or_else(|| angle("turn", 1.0))
        .or_else(|| (arg == "0").then_some(0.0))?;
    Some(Shape::Linear {
        angle: deg.rem_euclid(360.0),
        corner: None,
    })
}

fn radial_shape(arg: &str, len: &dyn Fn(&Value) -> Option<Len>) -> Option<Shape> {
    let (head, at) = match arg.find("at ").filter(|&i| i == 0 || arg[..i].ends_with(' ')) {
        Some(i) => (&arg[..i], Some(&arg[i + 3..])),
        None => (arg, None),
    };
    let (mut circle, mut size, mut lens) = (None, None, Vec::new());
    for w in head.split_whitespace() {
        match w {
            "circle" => circle = Some(true),
            "ellipse" => circle = Some(false),
            "closest-side" => size = Some(Size::ClosestSide),
            "farthest-side" => size = Some(Size::FarthestSide),
            "closest-corner" => size = Some(Size::ClosestCorner),
            "farthest-corner" => size = Some(Size::FarthestCorner),
            w => lens.push(css::parse_values(w).first().and_then(len)?),
        }
    }
    match lens.as_slice() {
        [] => {}
        [r] => {
            size = Some(Size::Explicit(*r, *r));
            circle = Some(true);
        }
        [a, b] => size = Some(Size::Explicit(*a, *b)),
        _ => return None,
    }
    let at = match at {
        None => (Len::Percent(50.0), Len::Percent(50.0)),
        Some(pos) => position(pos, len)?,
    };
    Some(Shape::Radial {
        circle: circle.unwrap_or(false),
        size: size.unwrap_or(Size::FarthestCorner),
        at,
    })
}

/// A position like `center`, `top left`, `30% 0` or `right 10px` (the last
/// read as an offset from the left, for simplicity).
fn position(pos: &str, len: &dyn Fn(&Value) -> Option<Len>) -> Option<(Len, Len)> {
    let (mut x, mut y) = (None, None);
    let mut loose = Vec::new();
    for w in pos.split_whitespace() {
        match w {
            "left" => x = Some(Len::Percent(0.0)),
            "right" => x = Some(Len::Percent(100.0)),
            "top" => y = Some(Len::Percent(0.0)),
            "bottom" => y = Some(Len::Percent(100.0)),
            "center" => loose.push(Len::Percent(50.0)),
            w => loose.push(css::parse_values(w).first().and_then(len)?),
        }
    }
    let mut loose = loose.into_iter();
    let x = x.or_else(|| loose.next()).unwrap_or(Len::Percent(50.0));
    let y = y.or_else(|| loose.next()).unwrap_or(Len::Percent(50.0));
    Some((x, y))
}

/// Premultiplied RGBA, 0..=255.
type Rgba = [f32; 4];

fn premul(c: Color) -> Rgba {
    let a = c.a as f32 / 255.0;
    [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, c.a as f32]
}

/// Draw `g` filling a `w`x`h` CSS px box, into a bitmap of at most
/// [`MAX_SIDE`] per side (straight, not premultiplied, RGBA).
pub fn render(g: &Gradient, w: f32, h: f32) -> Option<Bitmap> {
    if !(w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite()) {
        return None;
    }
    let bw = (w.ceil() as u32).clamp(1, MAX_SIDE);
    let bh = (h.ceil() as u32).clamp(1, MAX_SIDE);
    let (sx, sy) = (w / bw as f32, h / bh as f32);
    // `t` along the gradient for a point, and the gradient line's length.
    let (line_len, t_at): (f32, Box<dyn Fn(f32, f32) -> f32>) = match &g.shape {
        Shape::Linear { angle, corner } => {
            let (dx, dy) = match corner {
                Some((cx, cy)) => {
                    let (x, y) = (cx * h, cy * w);
                    let n = (x * x + y * y).sqrt();
                    (x / n, y / n)
                }
                None => {
                    let r = angle.to_radians();
                    (r.sin(), -r.cos())
                }
            };
            let l = (w * dx).abs() + (h * dy).abs();
            let l = l.max(f32::MIN_POSITIVE);
            let (cx, cy) = (w / 2.0, h / 2.0);
            (l, Box::new(move |x, y| ((x - cx) * dx + (y - cy) * dy) / l + 0.5))
        }
        Shape::Radial { circle, size, at } => {
            let (cx, cy) = (at.0.or_zero(w), at.1.or_zero(h));
            let (sides_x, sides_y) = ([cx.abs(), (w - cx).abs()], [cy.abs(), (h - cy).abs()]);
            let (near_x, far_x) = (sides_x[0].min(sides_x[1]), sides_x[0].max(sides_x[1]));
            let (near_y, far_y) = (sides_y[0].min(sides_y[1]), sides_y[0].max(sides_y[1]));
            let (rx, ry) = match (size, circle) {
                (Size::Explicit(a, b), _) => (a.or_zero(w), b.or_zero(h)),
                (Size::ClosestSide, true) => (near_x.min(near_y), near_x.min(near_y)),
                (Size::FarthestSide, true) => (far_x.max(far_y), far_x.max(far_y)),
                (Size::ClosestCorner, true) => {
                    let r = near_x.hypot(near_y);
                    (r, r)
                }
                (Size::FarthestCorner, true) => {
                    let r = far_x.hypot(far_y);
                    (r, r)
                }
                (Size::ClosestSide, false) => (near_x, near_y),
                (Size::FarthestSide, false) => (far_x, far_y),
                (Size::ClosestCorner, false) => (near_x * 2f32.sqrt(), near_y * 2f32.sqrt()),
                (Size::FarthestCorner, false) => (far_x * 2f32.sqrt(), far_y * 2f32.sqrt()),
            };
            let (rx, ry) = (rx.max(0.01), ry.max(0.01));
            (rx, Box::new(move |x, y| ((x - cx) / rx).hypot((y - cy) / ry)))
        }
    };
    // Stop positions as fractions of the line, filling in missing ones.
    let n = g.stops.len();
    let mut pos: Vec<Option<f32>> = g
        .stops
        .iter()
        .map(|(_, p)| p.map(|p| p.or_zero(line_len) / line_len))
        .collect();
    pos[0] = pos[0].or(Some(0.0));
    pos[n - 1] = pos[n - 1].or(Some(1.0));
    let mut last = f32::NEG_INFINITY;
    for p in pos.iter_mut().flatten() {
        *p = p.max(last);
        last = *p;
    }
    let mut i = 0;
    while i < n {
        if pos[i].is_none() {
            let a = i - 1;
            let b = (i..n).find(|&j| pos[j].is_some()).unwrap_or(n - 1);
            let (pa, pb) = (pos[a].unwrap_or(0.0), pos[b].unwrap_or(1.0));
            for (k, p) in pos.iter_mut().enumerate().take(b).skip(i) {
                *p = Some(pa + (pb - pa) * (k - a) as f32 / (b - a) as f32);
            }
            i = b;
        }
        i += 1;
    }
    let stops: Vec<(f32, Rgba)> = pos
        .iter()
        .zip(&g.stops)
        .map(|(p, (c, _))| (p.unwrap_or(0.0), premul(*c)))
        .collect();
    let (lo, hi) = (stops[0].0, stops[n - 1].0);
    let period = hi - lo;
    let color_at = |t: f32| -> Rgba {
        let t = if g.repeating && period > 1e-4 {
            lo + (t - lo).rem_euclid(period)
        } else {
            t
        };
        if t <= stops[0].0 {
            return stops[0].1;
        }
        for win in stops.windows(2) {
            let ((pa, ca), (pb, cb)) = (win[0], win[1]);
            if t <= pb {
                let f = if pb > pa { (t - pa) / (pb - pa) } else { 1.0 };
                return std::array::from_fn(|k| ca[k] + (cb[k] - ca[k]) * f);
            }
        }
        stops[n - 1].1
    };
    let mut rgba = Vec::with_capacity(bw as usize * bh as usize * 4);
    for row in 0..bh {
        for col in 0..bw {
            let c = color_at(t_at((col as f32 + 0.5) * sx, (row as f32 + 0.5) * sy));
            let a = c[3];
            let un = |v: f32| {
                if a > 0.0 {
                    (v * 255.0 / a).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                }
            };
            rgba.extend_from_slice(&[un(c[0]), un(c[1]), un(c[2]), a.round().clamp(0.0, 255.0) as u8]);
        }
    }
    Some(Bitmap { w: bw, h: bh, rgba })
}

/// One colour standing in for the gradient, for when there's no room to
/// draw it: the average of its stops.
pub fn average(g: &Gradient) -> Color {
    let n = g.stops.len() as f32;
    let sum = g.stops.iter().fold([0.0f32; 4], |acc, (c, _)| {
        let p = premul(*c);
        std::array::from_fn(|k| acc[k] + p[k] / n)
    });
    let a = sum[3];
    let un = |v: f32| {
        if a > 0.0 {
            (v * 255.0 / a).round().clamp(0.0, 255.0) as u8
        } else {
            0
        }
    };
    Color {
        r: un(sum[0]),
        g: un(sum[1]),
        b: un(sum[2]),
        a: a.round().clamp(0.0, 255.0) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(b: &Bitmap, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * b.w + x) * 4) as usize;
        [b.rgba[i], b.rgba[i + 1], b.rgba[i + 2], b.rgba[i + 3]]
    }

    #[test]
    fn parses_the_common_forms() {
        let g = parse(
            "linear-gradient(to right, #fff, rgba(0, 0, 0, 0.5) 30%, red 40% 60%)",
            16.0,
        )
        .unwrap();
        assert_eq!(
            g.shape,
            Shape::Linear {
                angle: 90.0,
                corner: None
            }
        );
        assert_eq!(g.stops.len(), 4, "the two-position stop counts twice");
        assert_eq!(g.stops[1].1, Some(Len::Percent(30.0)));
        let g = parse("linear-gradient(0.25turn, red, blue)", 16.0).unwrap();
        assert!(matches!(g.shape, Shape::Linear { angle, .. } if (angle - 90.0).abs() < 1e-3));
        let g = parse("radial-gradient(circle at top left, red, blue 2em)", 16.0).unwrap();
        assert_eq!(
            g.shape,
            Shape::Radial {
                circle: true,
                size: Size::FarthestCorner,
                at: (Len::Percent(0.0), Len::Percent(0.0))
            }
        );
        assert_eq!(g.stops[1].1, Some(Len::Px(32.0)));
        assert!(
            parse(
                "repeating-linear-gradient(45deg, red 0 10px, blue 10px 20px)",
                16.0
            )
            .unwrap()
            .repeating
        );
        assert!(parse("radial-gradient(red, blue)", 16.0).is_some());
        for bad in [
            "linear-gradient()",
            "linear-gradient(to right)",
            "conic-gradient(red, blue)",
            "red",
        ] {
            assert_eq!(parse(bad, 16.0), None, "{bad}");
        }
    }

    #[test]
    fn linear_goes_from_first_to_last_colour() {
        let g = parse("linear-gradient(to right, #000, #fff)", 16.0).unwrap();
        let b = render(&g, 100.0, 10.0).unwrap();
        assert!(px(&b, 0, 5)[0] < 5 && px(&b, 99, 5)[0] > 250);
        let mid = px(&b, 50, 5)[0];
        assert!((120..136).contains(&mid), "{mid}");
        // Straight down by default.
        let g = parse("linear-gradient(red, blue)", 16.0).unwrap();
        let b = render(&g, 10.0, 100.0).unwrap();
        assert!(px(&b, 5, 0)[0] > 240 && px(&b, 5, 99)[2] > 240);
    }

    #[test]
    fn corners_and_hard_stops() {
        let g = parse("linear-gradient(to bottom right, red 50%, blue 50%)", 16.0).unwrap();
        let b = render(&g, 200.0, 100.0).unwrap();
        assert_eq!(px(&b, 0, 0), [255, 0, 0, 255]);
        assert_eq!(px(&b, 199, 99), [0, 0, 255, 255]);
        // The bottom-left and top-right corners are on the middle line.
        let near = |p: [u8; 4]| p[0] > 0 && p[2] > 0 || p == [255, 0, 0, 255] || p == [0, 0, 255, 255];
        assert!(near(px(&b, 0, 99)) && near(px(&b, 199, 0)));
    }

    #[test]
    fn transparent_fades_without_going_grey() {
        let g = parse("linear-gradient(to right, transparent, #f00)", 16.0).unwrap();
        let b = render(&g, 100.0, 1.0).unwrap();
        let mid = px(&b, 50, 0);
        assert_eq!(&mid[..3], &[255, 0, 0], "{mid:?}");
        assert!((120..136).contains(&mid[3]));
    }

    #[test]
    fn radial_and_repeating() {
        let g = parse("radial-gradient(circle closest-side, #fff, #000)", 16.0).unwrap();
        let b = render(&g, 100.0, 100.0).unwrap();
        assert!(px(&b, 50, 50)[0] > 245 && px(&b, 0, 0)[0] < 5);
        let g = parse("repeating-linear-gradient(to right, #000 0, #fff 10px)", 16.0).unwrap();
        let b = render(&g, 100.0, 1.0).unwrap();
        assert!(px(&b, 0, 0)[0] < 30 && px(&b, 10, 0)[0] < 30 && px(&b, 9, 0)[0] > 200);
    }

    #[test]
    fn big_boxes_get_small_bitmaps_and_nonsense_is_safe() {
        let g = parse("linear-gradient(red, blue)", 16.0).unwrap();
        let b = render(&g, 5000.0, 30000.0).unwrap();
        assert_eq!((b.w, b.h), (MAX_SIDE, MAX_SIDE));
        for (w, h) in [(0.0, 10.0), (f32::NAN, 1.0), (f32::INFINITY, 5.0), (-3.0, 4.0)] {
            assert!(render(&g, w, h).is_none());
        }
        let g = parse("radial-gradient(0px 0px at 0 0, red, blue)", 16.0).unwrap();
        assert!(render(&g, 10.0, 10.0).is_some());
        let g = parse("repeating-linear-gradient(red 5px, blue 5px)", 16.0).unwrap();
        assert!(render(&g, 10.0, 10.0).is_some(), "a zero-length period");
    }
}
