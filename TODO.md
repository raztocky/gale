# To do

Planned work, roughly in priority order. Items marked *discuss* need a decision first.

## Features
- [x] Images (PNG/JPEG/WebP/GIF/SVG), decoded inside the sandboxed renderer
- [x] SVG images: text, in the bundled fonts
- [x] SVG images: drawn again at the size shown (`<img>` and inline `<svg>`; CSS backgrounds still at their own size)
- [x] Images: CSS `background-image`
- [x] Images: `<picture>` sources (`media`, `type`) and `srcset` (`x`/`w` with `sizes`), chosen for a fixed 1x density
- [x] Images: animated GIFs (animated WebP shows its first frame), `loading=lazy`
- [x] Floats and positioning (`float`, `position: absolute/fixed/relative`)
- [x] Real table layout
- [x] Flexbox, CSS grid, `@media` queries, `aspect-ratio`, percentage heights
- [x] Grid rows (`grid-template-rows`, `grid-auto-rows`, `grid-row`, `grid-area`, named areas)
- [x] `::before`/`::after` content (strings, `attr()`, quotes), and `<details>` that open on a click
- [x] CSS counters (`counter()`/`counters()` in `content`, `counter-reset`/`-increment`/`-set`, `list-item`), `grid-template`/`grid` shorthands
- [x] `grid-auto-flow` (`column`, `dense`) and `grid-auto-columns`
- [x] Show `<noscript>` content, and follow `<meta http-equiv=refresh>` redirects
- [x] Cookies: session-only, first-party only, per-site keep list
- [x] JavaScript: Boa in the sandboxed renderer, off unless you allow a site (see below)

## Text
- [x] Text shaping with **HarfRust** (the HarfBuzz project's safe-Rust port), in the renderer:
      kerning, ligatures, Arabic/Indic/Thai shaping
- [x] Font fallback: bundled Noto subsets for CJK, Korean, Devanagari, Thai, emoji, symbols
- [ ] More fallback scripts on request (Bengali, Tamil, Ethiopic, ...); color emoji
- [x] Right-to-left words read from the right (runs mirrored within each line)
- [ ] Full bidirectional text (paragraph direction, `dir="rtl"`, numbers inside RTL), with `unicode-bidi`
- [x] Unicode line breaking (UAX #14) with `unicode-linebreak`
- [ ] Thai/Lao/Khmer word breaking (needs a dictionary)
- [x] Page titles in the tab bar are drawn by the page's own sandboxed renderer; the browser only
      rasterizes its own text and checked URLs

- [x] Replace fontdue: it depends on `ttf-parser`, which is now unmaintained (RUSTSEC-2026-0192).
      A rasterizer on Google's maintained fontations stack (`skrifa` outlines) would also line up
      with HarfRust, which uses the same font-reading crates
- [x] Scrolling inside `overflow: auto/scroll` boxes with the wheel (`Shift` for sideways), and `max-height`
- [x] Scrolling inside boxes with the keyboard, and by dragging their scrollbar; `scrollTop`, `scrollTo()`, `scrollIntoView()` and `scroll` events for scripts
- [x] Request panel: notes when a page's scripts read enough browser details to look like fingerprinting
- [ ] Request panel: more trackers from a public list, once its licence is checked

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
- [x] CORS preflight (`OPTIONS`) for non-simple cross-origin requests, and checks at every redirect hop.
- [x] `fetch`'s `mode` and `redirect`, enforced by the browser.
- [x] Content Security Policy for scripts (`script-src`, `-elem`, `-attr`, `connect-src`, `default-src`), checked at every redirect hop.
- [x] Subresource Integrity for scripts; `nosniff` for scripts and stylesheets.
- [x] Subresource Integrity for stylesheets, including cache reuse and cross-origin CORS.
- [x] CSP for images and styles (`img-src`, `style-src`, `-elem`, `-attr`), with default fallbacks and redirect checks.
- [x] `form-action` (checked by the browser at every redirect) and `base-uri`, with `<base href>`.
  (`frame-src` and `upgrade-insecure-requests` have nothing to do: Brickle shows no frames, and
  HTTPS-only already upgrades every address.)
- [x] Scripts can't send to other sites (requests, or files they put on the page) unless you allow it.
- [x] `document.cookie` for non-HttpOnly cookies, answered by the browser.
- [x] JavaScript modules (`type=module`, static `import`, literal `import()`, import maps; `nomodule` skipped).
- [x] `import()` of an address built at run time (fetched when asked, as a script's request).
- [ ] CORS and the strict MIME type check for modules, `<link rel=modulepreload>`.
- [x] Keyboard input and form controls (typing into fields, submitting forms).
- [x] More selectors for `querySelector` (sibling combinators, `:not()`, attribute values),
  shared with CSS.
- [x] `getComputedStyle()` from the page's stylesheets (sizes from the layout once there is one).
- [x] `MutationObserver` callbacks (scripts' changes only, delivered in a microtask).
- [x] Sizes for scripts before the page is first shown (an early layout, at most 16 per load).
- [ ] Faster startup for the JS fuzzer (snapshot the prelude).
- [ ] Big framework apps that don't finish starting in time with Boa (yahoo.com's React app
  takes over 10 s; its feed stays as placeholders). The 32-script limit happens to keep it
  from starting; a long script is now stopped at the time budget instead of the renderer
  being killed.

## Forms

Done: text fields (with a caret and selection), passwords, textareas, checkboxes, radio buttons,
selects (with a drop-down), buttons, labels, Tab between fields, Enter to submit, GET/POST (url-encoded, multipart, text/plain), and the
`input`/`change`/`submit` events for scripts.
- [x] A real drop-down for `<select>`, and `↑`/`↓` to change it.
- [x] Moving the caret, selecting text in a field, copy, cut and paste.
- [x] Placing the caret and selecting in a field with the mouse (click, drag, double-click for a word).
- [x] File uploads (`<input type=file>`): your desktop's dialog (zenity or kdialog); the browser
  keeps the file and puts it in the form it sends; scripts see only its name, size and type.
- [ ] Several files at once (`multiple`), and files in forms scripts send (`FormData`).
- [x] Form validation before sending (`required`, `type=email`/`url`, `pattern`, `minlength`/`maxlength`, `min`/`max`/`step`, `novalidate`), with `checkValidity()`, `setCustomValidity()` and `validity` for scripts; arrow keys step number fields.
- [x] Date picker (calendar, typing, arrow keys) and colour picker (palette); `:valid`, `:invalid`, `:required` and `:optional` in CSS and for scripts.
- [x] Pickers for `time` (a clock of hours and five-minute steps), `datetime-local`, `month` and `week`, with validation and arrow keys.
- [x] `:user-invalid` and `:user-valid` (after you change a field and leave it, or try to send its form).
- [x] Ask before re-sending a POST on reload, back or forward (a page reached by a redirect after the POST reloads freely, as in Firefox).

## Tabs

Done: tab strip, new/close/switch (keyboard and mouse), links in background tabs, each tab with
its own renderer, history, scroll and form focus.
- [x] Reopen closed tab (`Ctrl+Shift+T`), with its history (kept in memory only).
- [ ] Drag tabs to reorder.
- [x] Bookmarks (`Ctrl+D`) and a bookmarks toolbar (on by default, `Ctrl+Shift+B`).
- [ ] Bookmark folders, renaming and reordering; a menu for bookmarks that don't fit the toolbar.
- [x] Discard the renderers of tabs left in the background for a long time (10 minutes, or past 6 live background tabs; they reload when shown).
