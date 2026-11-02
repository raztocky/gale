# Brickle Browser

A web browser engine written from scratch in Rust. It doesn't use Chromium, WebKit or Gecko: the
HTML parser, CSS engine, layout and rasterizer are all in `src/`.

![Brickle rendering a test page](docs/window.png)

## Run it

```sh
cargo run --release                         # opens the built-in home page
cargo run --release -- https://example.com  # open a URL
cargo run --release -- page.html            # open a local file
cargo run --release -- render page.html -o page.png -w 1024   # headless full-page PNG
cargo run --release -- dom page.html        # dump the parsed DOM
```

**Linux Mint:** works out of the box on X11 (Cinnamon). If the build complains about missing
libraries, run `sudo apt install build-essential libxkbcommon-dev`.

**Controls:** use `Ctrl+L` or click the address bar to type a URL. Scroll with the mouse wheel,
arrow keys, `Space` or `PgUp`/`PgDn`. Click links to follow them. `Backspace` or `Alt+←` goes back,
`Alt+→` goes forward, and `Ctrl+R` or `F5` reloads.

## Pipeline

```
bytes ─▶ html.rs ─▶ dom.rs ─▶ style.rs ◀─ css.rs (UA sheet + <style> + <link> + style="")
                                 │
                                 ▼
                             layout.rs ─▶ paint.rs ─▶ pixels (window via minifb, or PNG)
```

| Module | What it does |
|---|---|
| `html.rs` | Tokenizer and tree builder. Handles implied end tags (`<p>`, `<li>`, `<td>`…), void elements, raw text (`<script>`/`<style>`), entities, and `<html>/<head>/<body>` normalization |
| `css.rs` | Rules, selectors (type, `.class`, `#id`, `*`, `[attr]`, `:root`, `:link`, descendant and `>` combinators), `@media`, `!important`, colors, units |
| `style.rs` | Selector matching, specificity, the cascade (UA < author < inline), inheritance, shorthand expansion and computed values. The UA sheet is `ua.css` |
| `layout.rs` | Block and inline formatting contexts, box model, auto margins, `max-width`, margin collapsing between siblings, word wrapping, `text-align`, `<pre>`, list markers, inline-blocks (baseline-aligned) and a basic row flexbox |
| `paint.rs` | Display list and a software rasterizer: alpha-blended rects, anti-aliased glyphs (fontdue), synthetic italics |
| `net.rs` | `file://`, `http(s)://` (ureq) and relative URL resolution |
| `gui.rs` | Browser window: address bar, scrolling, link hover and click, history |

Fonts are bundled DejaVu (see `assets/fonts/LICENSE-DejaVu.txt`).

## Security

Brickle runs no JavaScript and has no cookies, storage or downloads, so the attack surface is small.
On top of that:

- **Web pages can't read your files.** A page loaded over `http(s)` can never load or link to
  `file://` URLs. The rule also covers reload, back and forward. Only `http`, `https` and `file`
  are supported at all.
- **HTTPS-first.** `http://` addresses are tried over `https://` first, and pages that end up on
  plain HTTP are marked **Not secure** in the address bar.
- **Anti-spoofing address bar.** The real host is shown in bold and in punycode, so a lookalike
  like `аpple.com` shows as `xn--pple-43d.com`. `user@` prefixes are hidden, so
  `google.com@evil.com` shows as `evil.com`. Pages can't draw over the browser's own UI.
- **Resource limits against hostile pages:**
  - Responses and files up to 8 MB, 5 redirects, 20 s timeout, text content types only.
  - At most 16 external stylesheets and 5,000 rules per sheet.
  - Element nesting depth 256 and 100,000 nodes per page.
  - Selectors up to 32 parts, and matching can't blow up exponentially.
  - Fonts capped at 256 px, and the glyph cache is bounded.
- `NO_PROXY` is honoured when going through a proxy.

Still to do: fuzzing, `cargo-deny` in CI, and a sandboxed renderer process, which is needed
before images or JavaScript.

## Not yet

JavaScript, images, real table layout, floats, positioning (`absolute`/`fixed`), flex wrapping and
alignment, `calc()`/`var()`, web fonts, forms, and text selection.
