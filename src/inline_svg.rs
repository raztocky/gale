//! Inline `<svg>` in HTML, drawn as an image: the element and everything
//! inside it are written back out as an SVG document (with the names the
//! HTML parser lowercased given their SVG spelling again, and the page's
//! CSS `fill`, `stroke`, `color` and `display: none` written in as
//! attributes), which goes through the same sandboxed decoder as any other
//! SVG image. Scripts, `<foreignObject>` and outside references stay out.

use crate::dom::NodeType;
use crate::style::{Display, StyledNode};

/// Largest SVG document written for one element; bigger ones aren't drawn.
pub const MAX_MARKUP: usize = 1024 * 1024;
/// Deepest nesting written.
const MAX_DEPTH: usize = 64;

/// Element names SVG spells with capitals.
const TAGS: &[&str] = &[
    "clipPath",
    "feBlend",
    "feColorMatrix",
    "feComponentTransfer",
    "feComposite",
    "feConvolveMatrix",
    "feDiffuseLighting",
    "feDisplacementMap",
    "feDistantLight",
    "feDropShadow",
    "feFlood",
    "feFuncA",
    "feFuncB",
    "feFuncG",
    "feFuncR",
    "feGaussianBlur",
    "feImage",
    "feMerge",
    "feMergeNode",
    "feMorphology",
    "feOffset",
    "fePointLight",
    "feSpecularLighting",
    "feSpotLight",
    "feTile",
    "feTurbulence",
    "foreignObject",
    "linearGradient",
    "radialGradient",
    "textPath",
];

/// Attribute names SVG spells with capitals.
const ATTRS: &[&str] = &[
    "attributeName",
    "attributeType",
    "baseFrequency",
    "baseProfile",
    "calcMode",
    "clipPathUnits",
    "diffuseConstant",
    "edgeMode",
    "filterUnits",
    "glyphRef",
    "gradientTransform",
    "gradientUnits",
    "kernelMatrix",
    "kernelUnitLength",
    "keyPoints",
    "keySplines",
    "keyTimes",
    "lengthAdjust",
    "limitingConeAngle",
    "markerHeight",
    "markerUnits",
    "markerWidth",
    "maskContentUnits",
    "maskUnits",
    "numOctaves",
    "pathLength",
    "patternContentUnits",
    "patternTransform",
    "patternUnits",
    "pointsAtX",
    "pointsAtY",
    "pointsAtZ",
    "preserveAlpha",
    "preserveAspectRatio",
    "primitiveUnits",
    "refX",
    "refY",
    "repeatCount",
    "repeatDur",
    "requiredExtensions",
    "requiredFeatures",
    "specularConstant",
    "specularExponent",
    "spreadMethod",
    "startOffset",
    "stdDeviation",
    "stitchTiles",
    "surfaceScale",
    "systemLanguage",
    "tableValues",
    "targetX",
    "targetY",
    "textLength",
    "viewBox",
    "viewTarget",
    "xChannelSelector",
    "yChannelSelector",
    "zoomAndPan",
];

fn spelled<'a>(name: &'a str, table: &[&'static str]) -> &'a str {
    table
        .iter()
        .find(|t| t.eq_ignore_ascii_case(name))
        .copied()
        .unwrap_or(name)
}

fn escape(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn hex(c: crate::css::Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// The SVG document for an inline `<svg>` element, or None if it's too big.
pub fn markup(svg: &StyledNode) -> Option<String> {
    let mut out = String::new();
    write(svg, None, 0, &mut out).then_some(out)
}

/// Writes `n` (whose parent's style is `parent`); false once over the size
/// limit.
fn write(n: &StyledNode, parent: Option<&StyledNode>, depth: usize, out: &mut String) -> bool {
    if out.len() > MAX_MARKUP {
        return false;
    }
    let e = match &n.node.node_type {
        NodeType::Text(t) => {
            out.push_str(&escape(t, false));
            return true;
        }
        NodeType::Comment(_) => return true,
        NodeType::Element(e) => e,
    };
    if depth > MAX_DEPTH || matches!(e.tag.as_str(), "script" | "foreignobject" | "iframe") {
        return true;
    }
    let tag = spelled(&e.tag, TAGS);
    out.push('<');
    out.push_str(tag);
    let s = &n.style;
    let p = parent.map(|p| &p.style);
    let root = parent.is_none();
    if root {
        out.push_str(" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
    }
    // CSS beats presentation attributes, so what the page's CSS set (the
    // computed value where it differs from the parent's) replaces them.
    let paint = |v: &Option<std::rc::Rc<str>>| {
        v.as_ref().map(|v| {
            let lower = v.to_ascii_lowercase();
            if lower.contains("currentcolor") {
                lower.replace("currentcolor", &hex(s.color))
            } else {
                v.to_string()
            }
        })
    };
    let fill = (root || p.is_some_and(|p| p.fill != s.fill))
        .then(|| paint(&s.fill))
        .flatten();
    let stroke = (root || p.is_some_and(|p| p.stroke != s.stroke))
        .then(|| paint(&s.stroke))
        .flatten();
    let color = (root || p.is_some_and(|p| p.color != s.color)).then(|| hex(s.color));
    let hidden = !root && s.display == Display::None;
    let mut attrs: Vec<(&String, &String)> = e.attrs.iter().collect();
    attrs.sort();
    for (k, v) in attrs {
        let k = k.as_str();
        let set_by_css = (k == "fill" && fill.is_some())
            || (k == "stroke" && stroke.is_some())
            || (k == "color" && color.is_some())
            || (k == "display" && hidden);
        // Sizes come from the layout, scripts never run, and nothing
        // outside the document is loaded.
        // (The root keeps a plain width and height when there's no viewBox
        // to give its shape.)
        let sized_by_box = e.attrs.contains_key("viewbox") || v.contains('%');
        let skip = (root
            && (matches!(k, "style" | "class") || (matches!(k, "width" | "height") && sized_by_box)))
            || k.starts_with("on")
            || k.starts_with("xmlns")
            || (k.ends_with("href") && !v.trim_start().starts_with('#'));
        if set_by_css
            || skip
            || !k
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | '_'))
        {
            continue;
        }
        out.push_str(&format!(" {}=\"{}\"", spelled(k, ATTRS), escape(v, true)));
    }
    for (k, v) in [("fill", fill), ("stroke", stroke), ("color", color)] {
        if let Some(v) = v {
            out.push_str(&format!(" {k}=\"{}\"", escape(&v, true)));
        }
    }
    if hidden {
        out.push_str(" display=\"none\"");
    }
    out.push('>');
    for c in &n.children {
        if !write(c, Some(n), depth + 1, out) {
            return false;
        }
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
    true
}

/// A `data:` address for `markup`, decoded without a request.
pub fn data_url(markup: &str) -> String {
    format!("data:image/svg+xml,{}", markup.replace('%', "%25"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{style_tree, StyleContext};

    fn svg_of(html: &str) -> String {
        let dom = crate::html::parse(html);
        let ctx = StyleContext {
            text_scale: 1.0,
            sheets: vec![],
            viewport: (800.0, 600.0),
        };
        let styled = style_tree(&dom, &ctx);
        fn find<'a>(n: &'a StyledNode<'a>) -> Option<&'a StyledNode<'a>> {
            if n.node.tag() == Some("svg") {
                return Some(n);
            }
            n.children.iter().find_map(find)
        }
        markup(find(&styled).expect("svg")).expect("markup")
    }

    #[test]
    fn svg_names_get_their_capitals_back_and_scripts_stay_out() {
        let m = svg_of(
            "<svg viewBox='0 0 4 4' onload='x()'><linearGradient id=g gradientUnits=userSpaceOnUse></linearGradient>\
             <script>x()</script><use href='https://e.com/a.svg#i'/><use href='#g'/>\
             <foreignObject><p>hi</p></foreignObject></svg>",
        );
        assert!(m.contains("viewBox=\"0 0 4 4\""), "{m}");
        assert!(
            m.contains("<linearGradient gradientUnits=\"userSpaceOnUse\" id=\"g\">"),
            "{m}"
        );
        assert!(m.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""), "{m}");
        for gone in ["onload", "script", "e.com", "foreignObject", "hi"] {
            assert!(!m.contains(gone), "{gone} in {m}");
        }
        assert!(m.contains("href=\"#g\""), "{m}");
    }
}
