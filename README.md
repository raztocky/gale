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
cargo run --release -- sandbox-test         # check the renderer sandbox works on your kernel
```

Add `--no-sandbox` to run everything in one process (for debugging).

**Linux Mint:** works out of the box on X11 (Cinnamon). If the build complains about missing
libraries, run `sudo apt install build-essential libxkbcommon-dev`.

**Controls:** use `Ctrl+L` or click the address bar to type a URL. Scroll with the mouse wheel,
arrow keys, `Space` or `PgUp`/`PgDn`. Click links to follow them. `Backspace` or `Alt+←` goes back,
`Alt+→` goes forward, and `Ctrl+R` or `F5` reloads.

## Architecture

```
 browser process (trusted)                     renderer process (sandboxed, one per page)
 ─────────────────────────                     ──────────────────────────────────────────
 window, address bar, history                  html.rs ─▶ dom.rs ─▶ style.rs ◀─ css.rs
 net.rs: fetches pages + stylesheets  ◀─ipc─▶                           │
 tab.rs: security policy, timeouts             layout.rs ─▶ display list
 paint.rs: rasterizes the display list
```

The browser fetches everything and enforces the security rules. Untrusted HTML and CSS are parsed
and laid out only in the renderer, which can't open files, use the network or run programs.
It sends back a display list, which the browser validates before painting.

| Module | What it does |
|---|---|
| `html.rs` | Tokenizer and tree builder. Handles implied end tags (`<p>`, `<li>`, `<td>`…), void elements, raw text (`<script>`/`<style>`), entities, and `<html>/<head>/<body>` normalization |
| `css.rs` | Rules, selectors (type, `.class`, `#id`, `*`, `[attr]`, `:root`, `:link`, descendant and `>` combinators), `@media`, `!important`, colors, units |
| `style.rs` | Selector matching, specificity, the cascade (UA < author < inline), inheritance, shorthand expansion and computed values. The UA sheet is `ua.css` |
| `layout.rs` | Block and inline formatting contexts, box model, auto margins, `max-width`, margin collapsing between siblings, word wrapping, `text-align`, `<pre>`, list markers, inline-blocks (baseline-aligned) and a basic row flexbox |
| `paint.rs` | Display list and a software rasterizer: alpha-blended rects, anti-aliased glyphs (fontdue), synthetic italics |
| `net.rs` | `file://` and `http(s)://` loading (ureq), the load policy, and relative URL resolution |
| `tab.rs` | Browser side of a page: spawns and supervises the renderer, answers its fetch requests |
| `renderer.rs` | The renderer process's message loop |
| `ipc.rs` | Length-limited binary protocol between the two. The browser treats every renderer message as hostile |
| `sandbox.rs` | Landlock + seccomp + rlimits for the renderer, and the `sandbox-test` probes |
| `gui.rs` | Browser window: address bar, scrolling, link hover and click, history |

Fonts are bundled DejaVu (see `assets/fonts/LICENSE-DejaVu.txt`).

## Security

Brickle runs no JavaScript and has no cookies, storage or downloads, so the attack surface is small.
On top of that:

- **Sandboxed renderer (Linux).** Each page is parsed in its own process, locked down with:
  - **Landlock:** no filesystem access, no TCP, no signals or abstract sockets outside the sandbox.
  - **seccomp-bpf:** a syscall allowlist, so `open`, `socket`, `execve` and the like fail.
  - **rlimits:** 4 GB of memory and 120 s of CPU time.

  The browser kills a renderer that hangs for 20 s or sends a malformed message, then shows an
  error page. Run `brickle sandbox-test` to see it block file reads, network and exec on your
  machine.
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
- **No `unsafe` code** in Brickle itself (`#![forbid(unsafe_code)]`).
- **Fuzzed.** `fuzz/` has [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the
  HTML parser, the CSS parser, the whole render pipeline, and the IPC decoder. CI fuzzes each one on
  every push. To run them locally: `cargo +nightly fuzz run html` (or `css`, `render`, `ipc`).
- **Dependency auditing.** [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) checks
  RustSec advisories, licenses and sources in CI on every push and weekly (config in `deny.toml`).
- `NO_PROXY` is honoured when going through a proxy.

On non-Linux systems the sandbox isn't implemented yet, so you have to run with `--no-sandbox`.

## Not yet

JavaScript, images, real table layout, floats, positioning (`absolute`/`fixed`), flex wrapping and
alignment, `calc()`/`var()`, web fonts, forms, and text selection.
