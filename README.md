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
| Reload / stop | `Ctrl+R` or `F5` / `Esc` |

## Architecture

```
 browser process (trusted)                     renderer process (sandboxed, one per page)
 ─────────────────────────                     ──────────────────────────────────────────
 window, address bar, history                  html.rs ─▶ dom.rs ─▶ style.rs ◀─ css.rs
 network.rs: rules, blocker, HTTPS ◀─ipc─▶                           │
 tab.rs: security policy, timeouts             layout.rs ─▶ draw ops ─▶ glyph atlas (fontdue)
 paint.rs: paints rects + glyph bitmaps
```

The browser fetches everything and enforces the security rules. Untrusted HTML and CSS are parsed
and laid out only in the renderer, which can't open files, use the network or run programs.
It sends back rectangles and pre-rasterized glyph bitmaps, which the browser validates before
painting.

| Module | What it does |
|---|---|
| `html.rs` | Tokenizer and tree builder. Handles implied end tags (`<p>`, `<li>`, `<td>`…), void elements, raw text (`<script>`/`<style>`), entities, and `<html>/<head>/<body>` normalization |
| `css.rs` | Rules, selectors (type, `.class`, `#id`, `*`, `[attr]`, `:root`, `:link`, descendant and `>` combinators), `@media`, `!important`, colors, units |
| `style.rs` | Selector matching, specificity, the cascade (UA < author < inline), inheritance, shorthand expansion and computed values. The UA sheet is `ua.css` |
| `layout.rs` | Block and inline formatting contexts, box model, auto margins, `max-width`, margin collapsing between siblings, Unicode line breaking (UAX #14), `text-align`, `<pre>`, list markers, inline-blocks (baseline-aligned) and a basic row flexbox |
| `font.rs` | Bundled fonts, per-character fallback, lazy loading |
| `paint.rs` | Draw ops, glyph-atlas rasterization (in the renderer, via fontdue), and the browser's software painter: alpha-blended rects and glyph bitmaps, cached in 512px tiles so scrolling just copies pixels |
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

Fonts: DejaVu (see `assets/fonts/LICENSE-DejaVu.txt`), plus Noto subsets for Chinese, Japanese,
Korean, Devanagari, Thai, emoji and symbols, under the SIL Open Font License
(`assets/fonts/fallback/OFL.txt`). Rebuild them with `assets/fonts/fallback/build.sh`. Fallback fonts
are only parsed when a page actually uses them.

## Security

Brickle runs no JavaScript. Cookies are first-party only and deleted on close unless you keep a site (see PRIVACY.md). Privacy protections are
described in [PRIVACY.md](PRIVACY.md). Security measures:

- **Sandboxed renderer (Linux).** Each page is parsed, laid out and its text rasterized in its own
  process, locked down with:
  - **namespaces** (where the system allows unprivileged user namespaces): its own network and
    IPC namespace, so there's no network to reach even past the other layers.
  - **Landlock:** no filesystem access, no TCP, no signals or abstract sockets outside the sandbox.
  - **seccomp-bpf:** a syscall allowlist, so `open`, `socket` and the like fail. `clone` can only
    create threads, memory can never be made executable, and attacker-only calls (`execve`,
    `ptrace`, `mount`, `bpf`, …) kill the renderer outright.
  - **rlimits:** 4 GB of memory and 120 s of CPU time.

  The browser only ever receives rectangles and finished glyph bitmaps from a renderer, and checks
  them (sizes, counts, references, and a per-region paint budget so it can't make every repaint
  crawl) before painting. It never runs the font rasterizer on anything
  a page controls. It kills a renderer that hangs for 20 s or sends a malformed message, then
  shows an error page. Run `brickle sandbox-test` to see it block file reads, network and exec on your
  machine.
- **Web pages can't reach your local network.** A public website can't make Brickle send
  requests to your router, printer or other devices (`192.168.x.x`, `localhost`, …). This is
  checked after the DNS lookup, on the exact address Brickle connects to, so a site can't sneak
  past it by pointing its domain at your network (DNS rebinding). A link from a website to a
  local device asks first. (Through a proxy, the proxy does the DNS lookup, so only literal local
  addresses and names are caught.)
- **Web pages can't read your files.** A page loaded over `http(s)` can never load or link to
  `file://` URLs. The rule also covers reload, back and forward. Only `http`, `https` and `file`
  are supported at all.
- **Anti-spoofing address bar.** The real host is shown in bold and in punycode, so a lookalike
  like `аpple.com` shows as `xn--pple-43d.com`. `user@` prefixes are hidden, so
  `google.com@evil.com` shows as `evil.com`. Pages can't draw over the browser's own UI.
- **Resource limits against hostile pages:**
  - Responses and files up to 8 MB, 5 redirects, 20 s per request, text content types only.
  - A page and its stylesheets get 30 s and 32 MB in total.
  - At most 16 external stylesheets and 5,000 rules per sheet.
  - Element nesting depth 256 and 100,000 nodes per page.
  - Selectors up to 32 parts, and matching can't blow up exponentially.
  - Fonts capped at 256 px, and the glyph cache is bounded.
- **A slow or hostile page can't freeze the window.** Loading and layout run on a background
  thread; `Esc` stops a load.
- **No `unsafe` code** in Brickle itself (`#![forbid(unsafe_code)]`).
- **Fuzzed.** `fuzz/` has [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the
  HTML parser, the CSS parser, the whole render pipeline, the IPC decoder and the filter engine.
  CI fuzzes each one on every push. To run them locally: `cargo +nightly fuzz run html` (or `css`,
  `render`, `ipc`, `filter`).
- **Dependency auditing.** [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) checks
  RustSec advisories, licenses and sources, and bans AI crates, in CI on every push and weekly
  (config in `deny.toml`). CI actions are pinned to commit hashes. Dependabot proposes updates,
  each waiting 7 days after release so hijacked releases have time to be caught.
- **Static analysis.** CodeQL scans the Rust code and the workflows, zizmor audits the CI
  workflows, and OpenSSF Scorecard rates the project's practices. Results are under the repo's
  Security tab.

- **Verifiable releases.** Release binaries are built only by GitHub Actions, with SHA-256 checksums
  and a signed build-provenance attestation:
  `gh attestation verify brickle-*-x86_64-linux.tar.gz --repo raztocky/gale`.
  Brickle never updates itself.

To report a vulnerability, see [SECURITY.md](SECURITY.md).

On non-Linux systems the sandbox isn't implemented yet, so you have to run with `--no-sandbox`.

## Not yet

JavaScript, images, real table layout, floats, positioning (`absolute`/`fixed`), flex wrapping and
alignment, `calc()`/`var()`, web fonts, forms, and text selection.
