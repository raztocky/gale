# Brickle's privacy rules

These are commitments, not settings. A change that breaks one of them is a bug.
Where a rule can be checked automatically, the test that enforces it is listed.

## 1. No telemetry

Brickle never sends information about you, your browsing or your use of the browser to
anyone. That means no analytics, crash reports, usage statistics, "phone home" checks, update
pings, experiments or A/B tests. There is no account and no sync.

## 2. No network requests you didn't ask for

Brickle connects to the network only when **you** load a page (typing an address, clicking a
link, reloading, going back or forward) and for that page's own resources, or when you run
`brickle filters update`. It doesn't prefetch or preconnect, it doesn't update anything in the
background, and it makes no requests at startup.

What you type in the address bar goes nowhere until you press Enter: there are no suggestions.
If it isn't an address (it has spaces, or is a single word with no dot), Enter searches for it on
[DuckDuckGo's HTML version](https://html.duckduckgo.com/html/), which works without JavaScript.
That search is an ordinary page load, with all the rules here applied.

What you type into a page's form is only sent when you submit the form (pressing Enter in a field,
or clicking its submit button), as a request you see in the request panel like any other. Brickle
doesn't save form data or passwords anywhere.

Every request a page makes is listed in the **request panel** (F12 or Ctrl+Shift+E, or click the
"N blocked · M requests" button). Each entry shows whether it was sent, blocked by a filter, or
denied by you, plus any tracking parameters removed and any HTTPS upgrade. Click a row to block
that host, or to unblock it. Your choices are saved as filter rules in
`~/.config/brickle/rules.txt` and always take priority over the filter lists.
`brickle requests URL` prints the same list in the terminal, and `brickle filters update` prints
every request it makes.

*Enforced by:* `tests/privacy.rs`
- `no_network_without_a_request_from_you`: runs Brickle with every proxy variable pointed at a
  trap that records connection attempts, and fails on any.
- `only_net_rs_talks_to_the_network`: fails if any code outside `src/net.rs` opens sockets.
- `blocking_stripping_and_firefox_headers`: checks that blocked and denied requests never reach
  the server.

## 3. No AI

Brickle has no AI features: no assistants, chatbots, summaries, "smart" suggestions,
translation models or AI-based anything. It never sends page content or your activity to an
AI service, and it doesn't ship or download models.

*Enforced by:* `deny.toml` bans known AI/LLM client and inference crates from the dependency
tree. CI runs `cargo deny check` on every push.

## 4. Blend in, don't stand out

Brickle identifies as **vanilla Firefox on Linux**, with the same `User-Agent`, `Accept`,
`Accept-Language` (`en-US,en;q=0.5`), `Accept-Encoding`, `Sec-Fetch-*` and `Priority` headers
Firefox sends, in Firefox's order. A unique name like "Brickle/0.1" would make you trackable on
its own. The version is `FIREFOX_VERSION` in `src/net.rs`; update it when Firefox releases.

Limits, honestly:
- Brickle's TLS handshake comes from rustls, not Firefox's NSS, so services doing TLS
  fingerprinting (JA3/JA4) can tell the difference.
- Brickle sends no `Referer` header at all, which is stricter than Firefox's default.
- JavaScript is off unless you allow a site (rule 9). With it off there is very little else to
  fingerprint. With it on, scripts see Firefox's `navigator` values, a fixed 1920x1080 screen,
  UTC as the time zone, and the window size rounded down to steps of 200x100 px (like Firefox's
  letterboxing). There's no canvas, WebGL, audio or font list to probe.
- Fonts are the same for everyone. Brickle draws every page with its own bundled fonts and never
  reads the ones installed on your computer, and it doesn't download web fonts (`@font-face`), so
  neither which fonts you have nor which ones a page fetches can identify you. Pages asking for
  Arial, Times New Roman, Courier New or system fonts get look-alikes with the same letter widths. A determined script can
  still tell Brickle from Firefox, for example by which APIs exist.

## 5. HTTPS-only

Every `http://` address is upgraded to `https://`. If a site doesn't support HTTPS, Brickle
shows a warning page and loads plain HTTP only if you click "Continue to HTTP site", for that
site, for this session. Pages loaded that way are marked **Not secure**. Secure pages can't
load insecure stylesheets (mixed content). Redirects are re-checked at every hop, so a site
can't redirect you down to HTTP. Local network addresses (`localhost`, `192.168.x.x`, …) are
exempt, as in Firefox.

A certificate that fails verification is not the same as "no HTTPS": it can mean someone is
intercepting the connection. Brickle shows a certificate error page with no way to continue,
and never offers plain HTTP instead.

*Enforced by:* `https_only_asks_before_using_plain_http` and
`bad_certificates_are_refused_not_downgraded` in `tests/privacy.rs`.

## 6. Content blocking

Brickle has a built-in blocker that reads the same lists as uBlock Origin: EasyList,
EasyPrivacy, and uBlock's filters, Privacy and Unbreak lists. Blocked requests are never sent.
Element-hiding rules hide ads and trackers on the page, and pages can't override them. A page
that is itself on a list shows a warning first, with an option to visit it anyway.

Lists are **not** bundled and are never updated automatically, because an automatic update
would be a request you didn't ask for. Run `brickle filters update` to download or refresh them.
It tries the official sources first, then uBlock Origin's GitHub mirror, and prints each request.
`brickle filters status` shows what's installed.

## 7. Tracking parameters are stripped

Before any request, Brickle removes known tracking parameters (`utm_*`, `fbclid`, `gclid`,
`msclkid`, `mc_eid`, `igshid`, `_hsenc`, … see `src/urlclean.rs`), plus any `$removeparam`
rules from the filter lists. The request panel shows what was removed. Credentials embedded in
a link (`https://user:pass@host/`) are removed too, and never sent.

## 8. Cookies

- **Session-only by default.** Cookies live in memory and are deleted when Brickle closes.
- **Keep list.** Sites you choose (the "Keep after closing" button in the request panel, or
  `brickle cookies keep SITE`) keep their cookies across restarts, so you stay logged in.
  The list is `~/.config/brickle/keep-cookies.txt`; their cookies are saved, readable only by
  you, in `~/.local/share/brickle/cookies.txt`.
- **No third-party cookies, ever.** A request carries cookies only when it goes to the same site
  as the page you're on, and only that site can set them. Cookies are also partitioned by the
  site you're visiting, as with Firefox's Total Cookie Protection.
- **No bounce tracking.** Cookies set while a link redirects you through another site on the way
  to the real page are dropped.
- **Safer defaults.** A cookie without `SameSite` is treated as `Lax`; `Secure` cookies only go
  over HTTPS (plain HTTP to your own computer, `localhost`, is the one exception; other
  local-network devices don't count); a plain-HTTP response can't overwrite, shadow or delete a
  `Secure` cookie; lifetimes are capped at 400 days; cookies for a whole public suffix (like
  `.co.uk` or `.github.io`) are refused.
- **Visible.** The request panel shows the cookies each request sent, set and was refused, and a
  "Forget" button deletes a site's cookies.
- **Out of the renderer's reach.** Only the browser process holds cookies; sandboxed page
  renderers never see them.

Brickle stores no other site data: no local storage, no cache.

*Enforced by:* `cookies_first_party_only_and_session_only` in `tests/privacy.rs`, and the tests
in `src/cookies.rs`.

## 9. JavaScript: off unless you allow a site

- **Off by default.** Pages' scripts aren't downloaded or run. `<noscript>` content is shown
  instead.
- **Allowed per site.** Use the "JavaScript" button in the request panel, or
  `brickle js allow SITE`. The list is `~/.config/brickle/js-sites.txt`. `--js` allows it
  everywhere for one run.
- **Scripts' requests go through the browser.** `fetch` and `XMLHttpRequest` work, but the
  browser makes every request, with the same checks as for anything else: they're listed in the
  request panel (as "xhr"), and your rules, the blocker (including `$xmlhttprequest` rules),
  HTTPS-only and the local-network block all apply. Scripts can't set headers that are the
  browser's business (cookies, origin, referrer, user agent and the like).
- **Other sites' data stays theirs (CORS).** A script may ask another site for something, but only
  sees the answer if that site allows it (`Access-Control-Allow-Origin`). Otherwise the response
  is withheld and the panel says so. Requests that would need a CORS "preflight" aren't sent at
  all yet.
- **No beacons.** `navigator.sendBeacon` sends nothing, and an image a script makes but doesn't
  put on the page (`new Image().src = ...`, the usual tracking pixel) loads nothing. Images a
  script adds to the page load like any other image (rule 10).
- **Cookies for scripts** follow the cookie rules (rule 8): no third-party cookies, ever.
  `document.cookie` shows only the page's own non-HttpOnly cookies; scripts can't see, create or
  replace HttpOnly ones (those are how sites keep you logged in). `localStorage` and
  `sessionStorage` only last as long as the page.
- **Navigation from scripts** (`location.href = ...`) goes through the same checks as a link,
  only to web addresses, and a page that keeps redirecting itself is stopped.
- **Sandboxed, interpreted, bounded.** Scripts run in the sandboxed renderer, in Boa, a
  JavaScript interpreter written in Rust with no JIT. Loops and recursion are capped. If scripts
  hang or crash, the page is shown without them after at most 15 seconds.

*Enforced by:* `javascript_is_off_by_default_and_its_requests_follow_the_rules` in
`tests/privacy.rs`, and the tests in `src/js.rs` and `src/cookies.rs`.

## 10. Images

Images on a page (`<img>`, and CSS `background-image`s) are requests like any other: listed in the request panel (as "img"),
with your rules, the blocker (including `$image` rules, which most tracking pixels match),
tracking-parameter stripping, HTTPS-only, the local-network block and the cookie rules (no
third-party cookies) applied to each. Brickle sends Firefox's headers for images. Each image is
fetched once per page, and a page gets at most 300 images and 64 MB of them.

Only PNG, JPEG, GIF and WebP are shown (GIFs and WebPs only their first frame). SVG images are
refused for now: an SVG is a document that can carry scripts. `data:` images are decoded
without any request.

*Enforced by:* `images_follow_the_rules` in `tests/privacy.rs`, and the tests in
`src/network.rs` and `src/filter.rs`.

## 11. The clipboard is yours

Brickle reads the clipboard only when you press `Ctrl+V` or `Shift+Insert`, and only to type what's
on it into the address bar or the form field you're in, as if you'd typed it yourself. It writes
the clipboard only when you press `Ctrl+C` (to copy the address). Only the browser process can
reach the clipboard; sandboxed pages can't, and scripts have no clipboard API
(`navigator.clipboard` is missing and `document.execCommand` does nothing).

What you copy from a page is what you saw. Text a page hides from you isn't selectable: text
that's transparent or the colour of what's behind it, smaller than 4px, pushed off the page, clipped
away by an `overflow` box, or covered by something opaque painted over it. Copied text also loses
invisible characters that change what it means or how it shows: control characters, direction
overrides, zero-width spaces, soft hyphens and Unicode tag characters. So a command you copy
can't carry a hidden `; curl … | sh`.

*Enforced by:* `hidden_text_is_not_copied` in `src/page.rs` and `invisible_characters_are_not_copied`
in `src/select.rs`.

## What Brickle does store

- `~/.config/brickle/rules.txt`: the block and allow rules you make in the request panel.
- `~/.local/share/brickle/filters/`: downloaded filter lists.
- `~/.config/brickle/js-sites.txt`: the sites where you allowed JavaScript.
- `~/.config/brickle/keep-cookies.txt` and `~/.local/share/brickle/cookies.txt`: the sites whose
  cookies you keep, and those cookies.
- `~/.config/brickle/text-size` and `interface-size`: your text and interface sizes, one number
  each, the same for every site.

There is no history file. Back/forward history exists only in memory while the window is open.
