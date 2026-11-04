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
- images are decoded only in the renderer (`src/image.rs`): image decoders are a classic source
  of memory-safety bugs, so the browser never runs one on a website's bytes. It only receives
  finished RGBA pixels, which it checks (sizes, references, total pixels, painting cost) before
  painting. Image sizes are checked from their headers before decoding, every decoder gets a
  memory limit, and a decoder panic only loses that image.

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
using its own record of the page's origin (`Network::script_request`). Which images, stylesheets
and scripts a script added to the page is reported by the renderer, so a compromised renderer
could pass one off as part of the page; it still can't reach anything the sandbox forbids, or a
request the other rules block. Loop-iteration and recursion limits can't be caught by scripts, work
runs in time-boxed batches, and a renderer that still hangs is killed and the page reloaded
without JavaScript. The `js` fuzz target drives random DOM operations through the whole
pipeline. The `image` fuzz target feeds arbitrary bytes to the image decoders and resizer, and
`svg` does the same for SVG drawings, text included.

## Supported versions

Only the latest commit on `main`.
