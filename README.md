# Gale

A web browser engine written from scratch in Rust. It doesn't use Chromium, WebKit or Gecko: the
HTML parser, CSS engine, layout and rasterizer are all in `src/`.

![Gale rendering a test page](docs/window.png)

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

## Not yet

JavaScript, images, real table layout, floats, positioning (`absolute`/`fixed`), flex wrapping and
alignment, `calc()`/`var()`, web fonts, forms, and text selection.
