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
- With no JavaScript there is very little else to fingerprint (no canvas, fonts list, screen
  size, etc.).

## 5. HTTPS-only

Every `http://` address is upgraded to `https://`. If a site doesn't support HTTPS, Brickle
shows a warning page and loads plain HTTP only if you click "Continue to HTTP site", for that
site, for this session. Pages loaded that way are marked **Not secure**. Secure pages can't
load insecure stylesheets (mixed content). Redirects are re-checked at every hop, so a site
can't redirect you down to HTTP. Local network addresses (`localhost`, `192.168.x.x`, …) are
exempt, as in Firefox.

*Enforced by:* `https_only_asks_before_using_plain_http` in `tests/privacy.rs`.

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
rules from the filter lists. The request panel shows what was removed.

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
  over HTTPS; lifetimes are capped at 400 days; cookies for a whole public suffix (like
  `.co.uk` or `.github.io`) are refused.
- **Visible.** The request panel shows the cookies each request sent, set and was refused, and a
  "Forget" button deletes a site's cookies.
- **Out of the renderer's reach.** Only the browser process holds cookies; sandboxed page
  renderers never see them.

Brickle stores no other site data: no local storage, no cache.

*Enforced by:* `cookies_first_party_only_and_session_only` in `tests/privacy.rs`, and the tests
in `src/cookies.rs`.

## What Brickle does store

- `~/.config/brickle/rules.txt`: the block and allow rules you make in the request panel.
- `~/.local/share/brickle/filters/`: downloaded filter lists.
- `~/.config/brickle/keep-cookies.txt` and `~/.local/share/brickle/cookies.txt`: the sites whose
  cookies you keep, and those cookies.

There is no history file. Back/forward history exists only in memory while the window is open.
