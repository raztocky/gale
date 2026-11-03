//! A loaded page: DOM + stylesheets, and the render pipeline.

use crate::css::{self, Color, Stylesheet};
use crate::dom::Node;
use crate::layout::{self, BoxKind, LayoutBox, Rect};
use crate::paint::{self, DisplayList};
use crate::style::{self, StyleContext};
use crate::{font, html, net};

use std::sync::OnceLock;

pub struct Page {
    pub url: String,
    pub title: String,
    pub dom: Node,
    pub sheets: Vec<Stylesheet>,
    /// Hiding rules from the content blocker. Applied with user-level
    /// priority, so pages can't override them with `!important`.
    pub blocker: Stylesheet,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub list: DisplayList,
    /// Glyph bitmaps the list's `Glyphs` commands refer to.
    pub atlas: Vec<paint::GlyphBitmap>,
    pub height: f32,
    pub background: Color,
    pub links: Vec<(Rect, String)>,
}

/// Fetches a subresource (absolute URL) for the page. In the sandboxed
/// renderer this asks the browser process, which applies the security policy.
pub type Fetcher<'f> = dyn FnMut(&str) -> Result<String, String> + 'f;

fn ua_sheet() -> &'static Stylesheet {
    static UA: OnceLock<Stylesheet> = OnceLock::new();
    UA.get_or_init(|| css::parse(style::UA_CSS))
}

impl Page {
    /// Parse a document. External stylesheets are requested through `fetch`.
    pub fn parse(url: &str, src: &str, blocker_css: &str, fetch: &mut Fetcher) -> Page {
        let dom = html::parse(src);
        let mut sheets = Vec::new();
        let mut found = Vec::new();
        dom.walk(&mut found, &|n| matches!(n.tag(), Some("style") | Some("link")));
        for n in found {
            let Some(e) = n.element() else { continue };
            if e.tag == "style" {
                sheets.push(css::parse(&n.text_content()));
            } else if e
                .attrs
                .get("rel")
                .is_some_and(|r| r.to_ascii_lowercase().contains("stylesheet"))
            {
                if let Some(href) = e.attrs.get("href") {
                    let media = e
                        .attrs
                        .get("media")
                        .map(|m| m.to_ascii_lowercase())
                        .unwrap_or_default();
                    if media.contains("print") {
                        continue;
                    }
                    // Failures are shown in the request log.
                    if let Ok(text) = fetch(&net::resolve(url, href)) {
                        sheets.push(css::parse(&text));
                    }
                }
            }
        }
        let mut titles = Vec::new();
        dom.walk(&mut titles, &|n| n.tag() == Some("title"));
        let title = titles
            .first()
            .map(|t| t.text_content().split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        Page {
            url: url.to_string(),
            title,
            dom,
            sheets,
            blocker: css::parse(blocker_css),
        }
    }

    /// Parse a document we generated ourselves (error pages); no subresources.
    pub fn trusted(url: &str, src: &str) -> Page {
        Page::parse(url, src, "", &mut |_| Err("subresources disabled".into()))
    }

    pub fn render(&self, width: f32, viewport_h: f32) -> Rendered {
        let mut all = vec![(0u8, ua_sheet())];
        all.extend(self.sheets.iter().map(|s| (1u8, s)));
        all.push((3u8, &self.blocker));
        let ctx = StyleContext {
            sheets: all,
            viewport: (width, viewport_h),
        };
        let styled = style::style_tree(&self.dom, &ctx);
        let root = layout::layout_tree(&styled, width);

        // The root's (or body's) background paints the whole canvas.
        let mut background = styled.style.background;
        if background.a == 0 {
            if let Some(body) = styled.children.iter().find(|c| c.node.tag() == Some("body")) {
                background = body.style.background;
            }
        }
        if background.a == 0 {
            background = Color::WHITE;
        }

        let (list, atlas) = paint::rasterize(paint::build_display_list(&root));
        let mut links = Vec::new();
        collect_links(&root, &mut links);
        let mb = root.dims.margin_box();
        Rendered {
            list,
            atlas,
            height: mb.y + mb.h,
            background,
            links,
        }
    }
}

fn collect_links(b: &LayoutBox, out: &mut Vec<(Rect, String)>) {
    if let (Some(l), BoxKind::Block(_)) = (&b.link, &b.kind) {
        out.push((b.dims.border_box(), l.clone()));
    }
    if let BoxKind::Anonymous { frags, .. } = &b.kind {
        for f in frags {
            if let Some(l) = &f.link {
                let (a, d) = font::vmetrics(font::index(&f.style), f.style.font_size);
                out.push((
                    Rect {
                        x: f.x,
                        y: f.baseline - a,
                        w: f.w,
                        h: a + d,
                    },
                    l.clone(),
                ));
            }
        }
    }
    for c in &b.children {
        collect_links(c, out);
    }
}

impl Rendered {
    /// Topmost link under a page-space point.
    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        self.links
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, l)| l.as_str())
    }
}

pub fn error_page(url: &str, err: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;");
    format!(
        "<title>Error</title><body style='font-family: sans-serif; margin: 40px; color: #333'>\
         <h1>Couldn't load this page</h1><p><code>{}</code></p><p style='color:#a00'>{}</p></body>",
        esc(url),
        esc(err)
    )
}
