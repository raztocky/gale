# To do

Planned work, roughly in priority order. Items marked *discuss* need a decision first.

## Features
- [x] Images (PNG/JPEG/WebP/GIF/SVG), decoded inside the sandboxed renderer
- [x] SVG images: text, in the bundled fonts
- [ ] SVG images: drawing at the size shown rather than the SVG's own size
- [x] Images: CSS `background-image`
- [ ] Images: `<picture>`/`srcset` choice by size, animation, lazy loading, inline `<svg>` in HTML
- [x] Floats and positioning (`float`, `position: absolute/fixed/relative`)
- [x] Real table layout
- [x] Flexbox, CSS grid columns, `@media` queries, `aspect-ratio`, percentage heights
- [ ] Grid rows (`grid-template-rows`, `grid-row`, named areas), `::before`/`::after` content
- [x] Show `<noscript>` content, and follow `<meta http-equiv=refresh>` redirects
- [x] Cookies: session-only, first-party only, per-site keep list
- [x] JavaScript: Boa in the sandboxed renderer, off unless you allow a site (see below)

## Text
- [ ] Text shaping with **HarfRust** (the HarfBuzz project's safe-Rust port), in the renderer:
      kerning, ligatures, Arabic/Indic/Thai shaping (*discuss*)
- [x] Font fallback: bundled Noto subsets for CJK, Korean, Devanagari, Thai, emoji, symbols
- [ ] More fallback scripts on request (Bengali, Tamil, Ethiopic, ...); color emoji
- [ ] Bidirectional text (Arabic, Hebrew) with `unicode-bidi`
- [x] Unicode line breaking (UAX #14) with `unicode-linebreak`
- [ ] Thai/Lao/Khmer word breaking (needs a dictionary)
- [x] Page titles in the tab bar are drawn by the page's own sandboxed renderer; the browser only
      rasterizes its own text and checked URLs

- [x] Replace fontdue: it depends on `ttf-parser`, which is now unmaintained (RUSTSEC-2026-0192).
      A rasterizer on Google's maintained fontations stack (`skrifa` outlines) would also line up
      with HarfRust, which uses the same font-reading crates

## Performance / robustness
- [x] Paint pages into tiles once and scroll by copying pixels
- [x] Paint off the window thread, like loading, so even a slow paint can't freeze the window (fixed and sticky layers are still painted on it)

## Project
- [ ] Choose a license
- [x] App icon of Brickle the cat (`assets/brickle.svg`)
- [ ] Home-page art; `.desktop` file for the Linux menu
- [ ] Bump `FIREFOX_VERSION` and `FIREFOX_RELEASED` in `src/net.rs` now and then: Brickle
      assumes up to six releases have come out since (about three months), then asks to be updated

## JavaScript

Done: Boa in the sandboxed renderer, off by default, allowed per site, DOM, events, timers,
clicks, and network requests only through the browser's checks. Rules that still apply to
what's next:
- Cookie selection and network authorization stay in the browser process, using the browser's
  own record of the page's origin, never what the renderer claims.
- [x] `fetch`/XHR through the browser, with same-origin policy, CORS and credentials rules.
- [x] CORS preflight (`OPTIONS`) for non-simple cross-origin requests.
- [x] Scripts can't send to other sites (requests, or files they put on the page) unless you allow it.
- [x] `document.cookie` for non-HttpOnly cookies, answered by the browser.
- [ ] JavaScript modules (`type=module`, `import`).
- [x] Keyboard input and form controls (typing into fields, submitting forms).
- [x] More selectors for `querySelector` (sibling combinators, `:not()`, attribute values),
  shared with CSS.
- [ ] `MutationObserver` callbacks; `getComputedStyle` beyond inline styles.
- [ ] Faster startup for the JS fuzzer (snapshot the prelude).

## Forms

Done: text fields, passwords, textareas, checkboxes, radio buttons, selects, buttons, labels,
Tab between fields, Enter to submit, GET/POST (url-encoded, multipart, text/plain), and the
`input`/`change`/`submit` events for scripts.
- [ ] A real drop-down for `<select>` (clicks cycle through the options for now).
- [ ] Moving the caret, selecting text, copy and paste.
- [ ] File uploads (`<input type=file>`).
- [ ] Form validation (`required`, `pattern`), `<input type=number>` steppers, date pickers.
- [x] Ask before re-sending a POST on reload, back or forward (a page reached by a redirect after the POST reloads freely, as in Firefox).

## Tabs

Done: tab strip, new/close/switch (keyboard and mouse), links in background tabs, each tab with
its own renderer, history, scroll and form focus.
- [ ] Reopen closed tab (`Ctrl+Shift+T`), drag to reorder.
- [x] Discard the renderers of tabs left in the background for a long time (10 minutes, or past 6 live background tabs; they reload when shown).
