# Brickle Browser

A web browser engine written from scratch in Rust. It doesn't use Chromium, WebKit or Gecko: the
HTML parser, CSS engine, layout and rasterizer are all in `src/`.

![Brickle rendering a test page](docs/window.png)

## Principles

**No telemetry. No AI. No requests you didn't ask for.** See [PRIVACY.md](PRIVACY.md) for the
full rules and the tests that enforce them. In short, Brickle has:

- a **request panel** that lists every request a page makes; click any row to block or allow that host
- a built-in **content blocker** using EasyList, EasyPrivacy and uBlock Origin's lists
- **HTTPS-only** mode, with a warning before ever using plain HTTP
- **tracking-parameter stripping** (`utm_*`, `fbclid`, `gclid`, …)
- a **vanilla Firefox** browser name and headers, so it doesn't stand out
- each page parsed in a **sandboxed process**

## Run it

```sh
cargo run --release -- filters update       # download the filter lists (once, and whenever you like)
cargo run --release                         # opens the built-in home page
cargo run --release -- https://example.com  # open a URL
cargo run --release -- page.html            # open a local file
cargo run --release -- requests https://example.com   # list every request a page makes
cargo run --release -- render page.html -o page.png -w 1024   # headless full-page PNG
cargo run --release -- dom page.html        # dump the parsed DOM
cargo run --release -- sandbox-test         # check the renderer sandbox works on your kernel
```

Debugging flags: `--no-sandbox` runs everything in one process, and `--no-filters` skips the
filter lists and your rules.

**Linux Mint:** works out of the box on X11 (Cinnamon). If the build complains about missing
libraries, run `sudo apt install build-essential libxkbcommon-dev`.

**Controls:**

| Action | Keys |
|---|---|
| Type a URL | `Ctrl+L` or click the address bar |
| Request panel | `F12`, `Ctrl+Shift+E`, or click "N blocked · M requests" |
| Scroll | mouse wheel, arrow keys, `Space`, `PgUp`/`PgDn` |
| Back / forward | `Backspace` or `Alt+←` / `Alt+→` |
| Reload | `Ctrl+R` or `F5` |

## Architecture

```
 browser process (trusted)                     renderer process (sandboxed, one per page)
 ─────────────────────────                     ──────────────────────────────────────────
 window, address bar, history                  html.rs ─▶ dom.rs ─▶ style.rs ◀─ css.rs
 network.rs: rules, blocker, HTTPS ◀─ipc─▶                           │
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
| `net.rs` | The only code that touches the network: single HTTP exchanges with Firefox's headers, decompression, URL helpers |
| `network.rs` | The network service: applies tracking stripping, your rules, the filter lists, HTTPS-only and redirect checks to every request, and keeps the request log |
| `filter.rs` | EasyList/uBlock filter engine: network rules, exceptions, options, cosmetic hiding, `$removeparam` |
| `lists.rs` | `brickle filters update` / `status` |
| `urlclean.rs` | Tracking-parameter stripping |
| `tab.rs` | Browser side of a page: spawns and supervises the renderer, answers its fetch requests |
| `renderer.rs` | The renderer process's message loop |
| `ipc.rs` | Length-limited binary protocol between the two. The browser treats every renderer message as hostile |
| `sandbox.rs` | Landlock + seccomp + rlimits for the renderer, and the `sandbox-test` probes |
| `gui.rs` | Browser window: address bar, scrolling, link hover and click, history |

Fonts are bundled DejaVu (see `assets/fonts/LICENSE-DejaVu.txt`).

## Security

Brickle runs no JavaScript and stores no cookies or other site data. Privacy protections are
described in [PRIVACY.md](PRIVACY.md). Security measures:

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
  HTML parser, the CSS parser, the whole render pipeline, the IPC decoder and the filter engine.
  CI fuzzes each one on every push. To run them locally: `cargo +nightly fuzz run html` (or `css`,
  `render`, `ipc`, `filter`).
- **Dependency auditing.** [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) checks
  RustSec advisories, licenses and sources, and bans AI crates, in CI on every push and weekly
  (config in `deny.toml`).

On non-Linux systems the sandbox isn't implemented yet, so you have to run with `--no-sandbox`.

## Not yet

JavaScript, images, real table layout, floats, positioning (`absolute`/`fixed`), flex wrapping and
alignment, `calc()`/`var()`, web fonts, forms, and text selection.
