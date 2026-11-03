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

## Supported versions

Only the latest commit on `main`.
