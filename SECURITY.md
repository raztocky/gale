# Security policy

## Reporting a vulnerability

Please **don't** open a public issue for security problems. Report them privately through
GitHub: on the repository page, open **Security → Report a vulnerability**. If that button
isn't there, open an issue that only asks for a private contact, with no details.

Helpful things to include:
- what an attacker can do, and what they need (for example, "a web page can read local files")
- a minimal page, filter list or steps that reproduce it
- the commit you tested, your Linux distribution and kernel version, and the output of
  `brickle sandbox-test`

You'll get an acknowledgement, and a fix or an explanation, as soon as practical. This is a small
hobby project, so there's no bug bounty.

## What counts

The ones we most want to hear about:
- web content escaping the renderer sandbox, or a renderer able to harm the browser process
  through the IPC channel
- a web page reading local files, reaching your local network, or downgrading to HTTP without
  the warning
- requests that bypass the request log, your rules or the filter lists, or any network request
  Brickle makes without being asked (see [PRIVACY.md](PRIVACY.md))
- address-bar spoofing
- crashes, hangs or memory blow-ups from hostile HTML, CSS or filter lists

## How the renderer boundary is defended

Each page is parsed and laid out by a sandboxed renderer process (see `src/sandbox.rs`). The
browser treats everything it sends as hostile (`src/tab.rs`, `src/ipc.rs`):
- messages are validated on decode and capped in size, count and painting cost;
- the protocol is strictly request/response: incoming frames go through a channel that holds
  none, an unsolicited message gets the renderer killed, and each exchange has a deadline and a
  byte budget;
- writes go through their own thread, so a renderer that stops reading can't block the browser;
- Stop (Esc) kills the renderer and abandons its network requests;
- all of a page's loading (document, redirects, stylesheets) shares one deadline;
- the renderer can only *ask* for subresources; the browser applies every policy to each request
  and holds the cookies;
- the renderer can hand the browser clipboard text (what you selected in a form field) only in
  reply to your `Ctrl+C` or `Ctrl+X`, capped in size and cleaned of invisible characters;
- images are decoded only in the renderer (`src/image.rs`): image decoders are a classic source
  of memory-safety bugs, so the browser never runs one on a website's bytes. It only receives
  finished RGBA pixels, which it checks (sizes, references, total pixels, painting cost) before
  painting. Image sizes are checked from their headers before decoding, every decoder gets a
  memory limit, and a decoder panic only loses that image. Text shaping (HarfRust) runs there
  too, on at most 4,096 bytes of text at a time; the browser draws its own interface text without
  it.

`hostile_renderer_tests` in `src/tab.rs` runs fake renderers that flood, stall, send partial,
oversized, out-of-order and unsolicited messages, stop reading, and ask for local files, local
network devices and blocked trackers. Security decisions about URLs all use one parser (the
`url` crate's WHATWG implementation, via `net::canonical`).

## JavaScript

Scripts run only in the sandboxed renderer, in Boa (an interpreter, no JIT: the sandbox forbids
executable memory anyway). What a script can reach is the page's own DOM through a small set of
native functions in `src/js.rs`, which validate every node id and string. Scripts' network
requests and cookie changes are queued and answered by the browser process, which applies the
network policy, CORS (with preflights) and the rule that scripts only send to their own site,
using its own record of the page's origin (`Network::script_request`), again at every redirect hop
before it's followed. The page's Content Security Policy (`src/csp.rs`) is enforced in the
renderer, where scripts run: which scripts run, inline handlers, `eval` (through Boa's
`HostEnsureCanCompileStrings` hook) and `connect-src`. The browser follows redirects, so the
renderer hands it the policy's source lists with each script and script request, and it checks
every hop against them. `integrity` hashes are checked in the renderer, `nosniff` in the browser.
The policy guards the page against injected scripts,
not against a compromised renderer, which the browser-side rules are for. Which images, stylesheets
and scripts a script added to the page is reported by the renderer, so a compromised renderer
could pass one off as part of the page; it still can't reach anything the sandbox forbids, or a
request the other rules block. Loop-iteration and recursion limits can't be caught by scripts, work
runs in time-boxed batches, and a renderer that still hangs is killed and the page reloaded
without JavaScript. The `js` fuzz target drives random DOM operations through the whole
pipeline. The `image` fuzz target feeds arbitrary bytes to the image decoders and resizer, and
`svg` does the same for SVG drawings, text included.

## Supported versions

Only the latest commit on `main`.

## Decoder containment and sandbox requirements

With sandboxing enabled, each image (including inline `data:` images) is
parsed in a fresh decoder process. The browser brokers the bytes, validates
returned dimensions and exact RGBA length, and never runs the image parser.
The decoder installs the renderer's syscall/filesystem restrictions before
reading input, with a 512 MiB address-space limit and two CPU seconds. The
browser additionally enforces a three-second wall deadline, cancellation,
an 8 MiB input cap and a 2048-by-2048 output cap. It kills and reaps failed
workers, leaving the image blank while subsequent images can load. These
budgets can reject large or unusually expensive legitimate images. The
explicit `--no-sandbox` development backend retains in-process decoding.

Startup prints the sandbox probe's layer status; the request panel also
shows it. This reports a fresh local probe, rather than a claim supplied by
the page renderer. Individual children still enforce the configured policy.
Use `--strict-sandbox` to require seccomp, resource limits, namespaces and
fully enforced Landlock. Use, for example,
`--strict-sandbox=seccomp,rlimits` to require selected layers. Missing required
layers fail closed before a page opens; strict mode cannot accompany
`--no-sandbox`. Partial Landlock does not satisfy a full Landlock requirement.
No security status probe sends a network request.

## Document transitions

Loader work and replies carry a browser-owned document identity, shared
with its cancellation token. Foreground and background tabs reject replies
from another document, and Stop permanently invalidates that identity.
Rejected replies cannot update the title, DOM/layout, clipboard or script
navigation. New navigation also drops pending input from the old document.
Render/input exchanges bind cancellation to their current document token.
The tile painter independently checks its existing generation IDs; completed
tiles from older scenes are discarded. None of these identities are assigned
or authorized by page scripts.

Regression coverage is in `tests/hardening.rs` and the decoder, sandbox,
network, GUI and painter unit tests. The slow SVG fixture is a synthetic fuzz
input, with no external resources fetched. It exercises skipping an expensive
inline image while the next image still paints. These tests check specific
boundaries and do not establish complete browser security.


## Resource policy and stylesheet integrity

Image and style policies apply with JavaScript off as well as on. Headers and initial
meta policies intersect, with `default-src` fallbacks. Inline style elements support
nonces/hashes, and style attributes require inline permission (or `unsafe-hashes` plus
a matching hash). The renderer strips denied attributes from its displayed tree and
holds the initial policy across DOM refreshes. Meta policies are conservatively applied
to the whole static document, including content preceding a meta element; sequential
HTML-parser timing is not implemented. Unsupported CSP directives remain in TODO.md.

External images, CSS backgrounds and stylesheets have every redirect checked. The
browser independently retains header policies for these resource types: an omitted
renderer source list cannot remove them. Nonce-authorized stylesheet links carry the
nonce separately. Stylesheet integrity is checked against decompressed response bytes
before text conversion, and the renderer checks before applying or caching CSS. Cache
keys include integrity metadata, so another link with a different hash cannot reuse an
unchecked sheet. Cross-origin integrity stylesheets require CORS and omit cookies;
existing redirect, mixed-content and request authorization still apply. Stylesheet text
is currently UTF-8 only; invalid UTF-8 is refused rather than verified after lossy conversion.
Data-URL stylesheets are checked before application too. Failed checks send no reports. Policy/header/source count or size overflow fails closed;
restrictions are never silently dropped to fit a limit.

## Security inventory and local audit records

`brickle security status` is read-only and offline. It reports build time/revision,
lockfile SHA-256, local filter file ages (stale after seven days), validated CRLite
availability and age, and saved exceptions. Build age over thirty days is flagged;
this is a maintenance reminder, not a remote latest-version or vulnerability check.
Filter mtimes describe local files, not verified upstream publication dates.

Dependency audit status defaults to **unknown**. An optional manually supplied local
record at the data directory's `security-audit.json` can record a completed audit:

```json
{"lock_sha256":"SHA256_OF_BUILT_CARGO_LOCK","checked_at":0,"advisory_db_updated_at":0,"vulnerabilities":0}
```

The timestamps are Unix seconds. Only records for this binary's embedded lockfile,
with valid clock ordering, are displayed. Audit and database ages over seven days are
marked stale. This is a local operator record, not a signed attestation or an audit
performed by Brickle. Creating/rebuilding the browser does not create an audit record
or imply that an old advisory database is current. Downloads and browser/dependency
updates remain explicitly user-initiated; there is no background update or telemetry.
