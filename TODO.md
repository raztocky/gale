# To do

Planned work, roughly in priority order. Items marked *discuss* need a decision first.

## Features
- [ ] Images (PNG/JPEG/WebP/GIF), decoded inside the sandboxed renderer
- [ ] Floats and positioning (`float`, `position: absolute/fixed/relative`)
- [ ] Real table layout
- [ ] Show `<noscript>` content (Brickle never runs scripts)
- [ ] Cookies (*discuss*: third-party blocked by default, per-site storage)
- [ ] JavaScript (*discuss*: big project; would require same-origin policy and CSP first)

## Text
- [ ] Text shaping with **HarfRust** (the HarfBuzz project's safe-Rust port), in the renderer:
      kerning, ligatures, Arabic/Indic/Thai shaping (*discuss*)
- [ ] Font fallback for scripts DejaVu lacks (CJK, Devanagari, emoji): bundled Noto subset or
      system fonts passed safely into the sandbox
- [ ] Bidirectional text (Arabic, Hebrew) with `unicode-bidi`
- [ ] Unicode line breaking (UAX #14) with `unicode-linebreak`
- [ ] Draw the browser's own UI text in a sandboxed helper too

## Project
- [ ] Choose a license
- [ ] App icon and home-page art of Brickle the cat; `.desktop` file for the Linux menu
- [ ] Bump `FIREFOX_VERSION` in `src/net.rs` with Firefox releases
