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
- [x] Font fallback: bundled Noto subsets for CJK, Korean, Devanagari, Thai, emoji, symbols
- [ ] More fallback scripts on request (Bengali, Tamil, Ethiopic, ...); color emoji
- [ ] Bidirectional text (Arabic, Hebrew) with `unicode-bidi`
- [x] Unicode line breaking (UAX #14) with `unicode-linebreak`
- [ ] Thai/Lao/Khmer word breaking (needs a dictionary)
- [ ] Draw the browser's own UI text in a sandboxed helper too

- [ ] Replace fontdue: it depends on `ttf-parser`, which is now unmaintained (RUSTSEC-2026-0192).
      A rasterizer on Google's maintained fontations stack (`skrifa` outlines) would also line up
      with HarfRust, which uses the same font-reading crates

## Performance / robustness
- [x] Paint pages into tiles once and scroll by copying pixels
- [ ] Paint off the window thread, like loading, so even a slow paint can't freeze the window

## Project
- [ ] Choose a license
- [ ] App icon and home-page art of Brickle the cat; `.desktop` file for the Linux menu
- [ ] Bump `FIREFOX_VERSION` in `src/net.rs` with Firefox releases
