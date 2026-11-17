//! Which picture an `<img>` shows: `<picture>`'s `<source>`s (by `media`
//! and `type`), then `srcset` with `x` or `w` descriptors and `sizes`,
//! then `src`.
//!
//! Brickle always chooses for one fixed pixel density instead of the
//! screen's: which file a page sees requested would otherwise tell the
//! site your display's pixel ratio.

use crate::css::Media;
use crate::dom::ElementData;

/// The pixel density pictures are chosen for (see the module docs).
pub const DENSITY: f32 = 1.0;
/// Most `srcset` candidates looked at.
const MAX_CANDIDATES: usize = 64;

/// Image types Brickle can show (`<source type>`).
fn supported(mime: &str) -> bool {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    matches!(
        m.as_str(),
        "" | "image/png"
            | "image/apng"
            | "image/jpeg"
            | "image/jpg"
            | "image/pjpeg"
            | "image/gif"
            | "image/webp"
            | "image/svg+xml"
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Descriptor {
    Density(f32),
    Width(f32),
}

/// `srcset`'s candidates: (address, descriptor). Addresses can hold commas
/// (`data:`), so this follows the HTML spec's splitting rather than
/// splitting at every comma.
fn candidates(srcset: &str) -> Vec<(&str, Option<Descriptor>)> {
    let mut out = Vec::new();
    let mut rest = srcset;
    while out.len() < MAX_CANDIDATES {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        if rest.is_empty() {
            break;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (mut url, after) = rest.split_at(end);
        rest = after;
        let mut descriptors = "";
        if url.ends_with(',') {
            url = url.trim_end_matches(',');
        } else {
            // Descriptors run to the next comma outside parentheses.
            let mut depth = 0i32;
            let mut stop = rest.len();
            for (i, c) in rest.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth <= 0 => {
                        stop = i;
                        break;
                    }
                    _ => {}
                }
            }
            descriptors = &rest[..stop];
            rest = &rest[stop..];
        }
        if url.is_empty() {
            continue;
        }
        let mut d = None;
        let mut bad = false;
        for t in descriptors.split_whitespace() {
            let parsed = if let Some(n) = t.strip_suffix('x') {
                n.parse().ok().filter(|v: &f32| *v > 0.0).map(Descriptor::Density)
            } else if let Some(n) = t.strip_suffix('w') {
                n.parse().ok().filter(|v: &f32| *v > 0.0).map(Descriptor::Width)
            } else if t.ends_with('h') {
                continue; // a height hint, not used
            } else {
                None
            };
            match parsed {
                Some(p) if d.is_none() => d = Some(p),
                _ => bad = true,
            }
        }
        if !bad
            && d.is_none_or(|d| match d {
                Descriptor::Density(v) | Descriptor::Width(v) => v.is_finite(),
            })
        {
            out.push((url, d));
        }
    }
    out
}

/// `sizes`: how wide the picture will be shown, in CSS px.
fn shown_width(sizes: Option<&str>, viewport: (f32, f32)) -> f32 {
    let length = |t: &str| -> Option<f32> {
        let t = t.trim().to_ascii_lowercase();
        let num = |s: &str| {
            s.trim()
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
        };
        if let Some(n) = t.strip_suffix("vw") {
            num(n).map(|v| v * viewport.0 / 100.0)
        } else if let Some(n) = t.strip_suffix("px") {
            num(n)
        } else if let Some(n) = t.strip_suffix("rem").or_else(|| t.strip_suffix("em")) {
            num(n).map(|v| v * 16.0)
        } else if t == "0" {
            Some(0.0)
        } else {
            None
        }
    };
    for item in sizes.unwrap_or("").split(',').take(MAX_CANDIDATES) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        // The length is the last token; anything before it is the condition.
        let (cond, len) = match item.rfind(|c: char| c.is_whitespace() || c == ')') {
            Some(i) if item[i..].starts_with(')') => (&item[..=i], &item[i + 1..]),
            Some(i) => (&item[..i], &item[i + 1..]),
            None => ("", item),
        };
        let Some(px) = length(len) else { continue };
        if cond.trim().is_empty() || Media::parse(cond).matches(viewport) {
            return px;
        }
    }
    viewport.0
}

/// The address to show from `srcset` (with `sizes`), or None if it has no
/// usable candidate.
/// The address to show from `srcset` (with `sizes`), and its pixel
/// density, or None if it has no usable candidate. `src` counts as the
/// `1x` candidate when `srcset` has none (and no widths).
fn pick<'a>(
    srcset: &'a str,
    sizes: Option<&str>,
    src: Option<&'a str>,
    viewport: (f32, f32),
) -> Option<(&'a str, f32)> {
    let mut list = candidates(srcset);
    let shown = shown_width(sizes, viewport).max(1.0);
    let density = |d: Option<Descriptor>| match d {
        None => 1.0,
        Some(Descriptor::Density(x)) => x,
        Some(Descriptor::Width(w)) => w / shown,
    };
    let has_1x = list
        .iter()
        .any(|(_, d)| matches!(d, None | Some(Descriptor::Density(1.0))));
    let has_w = list.iter().any(|(_, d)| matches!(d, Some(Descriptor::Width(_))));
    if let Some(src) = src.filter(|_| !has_1x && !has_w) {
        list.push((src, None));
    }
    // The smallest picture dense enough, or else the densest there is.
    let enough = list
        .iter()
        .filter(|(_, d)| density(*d) >= DENSITY)
        .min_by(|a, b| density(a.1).total_cmp(&density(b.1)));
    enough
        .or_else(|| list.iter().max_by(|a, b| density(a.1).total_cmp(&density(b.1))))
        .map(|(u, d)| (*u, density(*d)))
}

/// The address an `<img>` shows, as written (not yet resolved), and its
/// pixel density (a `2x` picture is shown at half its pixel size).
/// `sources` are the `<source>` elements before it in its `<picture>`.
pub fn choose<'a>(
    img: &'a ElementData,
    sources: &[&'a ElementData],
    viewport: (f32, f32),
) -> Option<(&'a str, f32)> {
    for s in sources {
        let a = &s.attrs;
        if a.get("media").is_some_and(|m| !Media::parse(m).matches(viewport))
            || a.get("type").is_some_and(|t| !supported(t))
        {
            continue;
        }
        let sizes = a.get("sizes").map(String::as_str);
        if let Some(c) = a.get("srcset").and_then(|set| pick(set, sizes, None, viewport)) {
            return Some(c);
        }
    }
    let a = &img.attrs;
    let src = a.get("src").map(|s| s.trim()).filter(|s| !s.is_empty());
    a.get("srcset")
        .and_then(|set| pick(set, a.get("sizes").map(String::as_str), src, viewport))
        .or_else(|| src.map(|s| (s, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::AttrMap;

    fn el(attrs: &[(&str, &str)]) -> ElementData {
        let mut m = AttrMap::new();
        for (k, v) in attrs {
            m.insert(k.to_string(), v.to_string());
        }
        ElementData {
            tag: "img".into(),
            attrs: m,
        }
    }

    const VIEW: (f32, f32) = (1000.0, 800.0);

    fn url<'a>(img: &'a ElementData, sources: &[&'a ElementData], view: (f32, f32)) -> Option<&'a str> {
        choose(img, sources, view).map(|c| c.0)
    }

    #[test]
    fn src_is_the_1x_candidate_and_densities_are_kept() {
        let img = el(&[("src", "a.png"), ("srcset", "a2.png 2x")]);
        assert_eq!(choose(&img, &[], VIEW), Some(("a.png", 1.0)));
        let img = el(&[("srcset", "a2.png 2x")]);
        assert_eq!(choose(&img, &[], VIEW), Some(("a2.png", 2.0)));
    }

    #[test]
    fn density_descriptors() {
        let img = el(&[("src", "a.png"), ("srcset", "b.png 1x, c.png 2x")]);
        assert_eq!(url(&img, &[], VIEW), Some("b.png"));
        let img = el(&[("srcset", "c.png 2x, d.png 3x")]);
        assert_eq!(url(&img, &[], VIEW), Some("c.png"), "the smallest dense enough");
        let img = el(&[("srcset", "e.png 0.5x")]);
        assert_eq!(url(&img, &[], VIEW), Some("e.png"), "else the densest");
    }

    #[test]
    fn width_descriptors_and_sizes() {
        let set = "s.jpg 400w, m.jpg 800w, l.jpg 1600w";
        let img = el(&[("srcset", set), ("sizes", "(max-width: 600px) 100vw, 50vw")]);
        assert_eq!(url(&img, &[], VIEW), Some("m.jpg"), "50vw of 1000px wants 500px");
        assert_eq!(url(&img, &[], (500.0, 800.0)), Some("m.jpg"), "100vw of 500px");
        let img = el(&[("srcset", set), ("sizes", "300px")]);
        assert_eq!(url(&img, &[], VIEW), Some("s.jpg"));
        let img = el(&[("srcset", set)]);
        assert_eq!(
            url(&img, &[], VIEW),
            Some("l.jpg"),
            "no sizes: the viewport's width"
        );
    }

    #[test]
    fn addresses_with_commas() {
        let img = el(&[("srcset", "data:image/png;base64,AAAA 1x, b.png 2x")]);
        assert_eq!(url(&img, &[], VIEW), Some("data:image/png;base64,AAAA"));
        // As the HTML spec reads it: only a comma ending an address splits.
        assert_eq!(
            candidates("a.png,b.png 2x"),
            [("a.png,b.png", Some(Descriptor::Density(2.0)))]
        );
        assert_eq!(candidates("a.png, b.png 2x").len(), 2);
    }

    #[test]
    fn picture_sources() {
        let img = el(&[("src", "fallback.jpg")]);
        let avif = el(&[("type", "image/avif"), ("srcset", "a.avif")]);
        let narrow = el(&[("media", "(max-width: 600px)"), ("srcset", "narrow.webp")]);
        let webp = el(&[("type", "image/webp"), ("srcset", "w1.webp 1x, w2.webp 2x")]);
        assert_eq!(url(&img, &[&avif, &narrow, &webp], VIEW), Some("w1.webp"));
        assert_eq!(
            url(&img, &[&avif, &narrow, &webp], (500.0, 800.0)),
            Some("narrow.webp")
        );
        assert_eq!(url(&img, &[&avif], VIEW), Some("fallback.jpg"));
    }
}
