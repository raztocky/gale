# Compatibility checks

Normal CI runs original synthetic link-board, forum and news layouts entirely
through a loopback replay proxy. No publisher pages are fetched by `cargo test`.
These cases check readable **rendered text**, navigation links, a real PNG,
JavaScript remaining off and a blocked synthetic advertising resource. They
complement the hostile-input suite; they do not establish complete security or
pixel-perfect rendering.

Requirements on Linux: Python 3 standard library, OpenSSL, `prlimit` (util-linux),
and a normal sandbox-enabled Brickle build.

```sh
cargo test --locked --test compatibility
# Or inspect the PNGs and JSON:
cargo build --locked
python3 tests/compatibility/runner.py --binary target/debug/brickle
```

Results are under `target/compat-results/`; CI's replay uses `target/compat-ci/`.
`compat-probe` is a developer diagnostic called by the runner, not a test that
passes merely because the process returned zero. Its JSON includes text from
rendered text runs, HTTP document status, page height, links and request outcomes.
The PNG shows the first 4,096 rows. It refuses the unsandboxed backend and profiles
with saved JavaScript permissions. The runner creates a disposable profile for
every case; it never uses your account, saved cookies or normal profile.

## Opt-in current public pages

```sh
python3 tests/compatibility/runner.py --live --site hn --site sa --site guardian
```

This explicitly fetches public pages and a bounded selection of CSS/images, then
runs Brickle **against the local capture**. It is not a direct live Brickle session.
The approved capture hosts are listed in `PRESETS` in `runner.py`: Hacker News;
Something Awful's main/forum/image hosts; and The Guardian plus its guim asset
hosts. Redirects cannot expand that list. No story links are followed, no forms
submitted, no JavaScript downloaded/executed, and no logins or cookies sent.
Capture does not fetch ads/trackers from other domains or update filter lists.
Response cookies, auth and tracking identifiers from headers are not retained.

Captures and outputs stay under ignored `target/` paths. Publisher HTML and assets
are not committed. Captures retain original URLs, bodies, content types, redirects,
CSP, CORS and integrity metadata. SHA-256 verifies each recorded body on replay.

```sh
# Reproduce a particular capture without fetching it again:
python3 tests/compatibility/runner.py --snapshot target/compat-captures/hn.json
```

The local proxy terminates HTTPS with a temporary test CA trusted only by the
child process through `SSL_CERT_FILE`. It serves recorded responses and rejects
unknown origins/paths; **it has no upstream forwarding path**. Original hostnames,
origins, same-site relationships, TLS schemes, CSP sources and resource bytes are
preserved. This avoids rewriting stylesheets and accidentally invalidating SRI.
Temporary private keys are deleted at the end; system certificates and hosts files
are untouched. This proxy configuration is a containment measure for normal
browser traffic, not an OS-wide network namespace or a sandbox for compromised
browser-process code.

Capture limits: 24 resources, 64 attempted fetches, 2 MiB per response, 12 MiB total,
a 60-second case budget checked between bounded reads and a 10-second socket
read timeout. A stalled read can extend the capture deadline by that timeout.
Only HTML, CSS and supported image MIME types are accepted.

Replay limits: 64 proxy CONNECT attempts / GET requests, 45 seconds per browser
process group, 4 MiB stdout/stderr, 32 MiB output files and a 2 GiB soft / 4 GiB hard
browser address-space limit. The existing renderer and decoder limits remain in
place; these are per-process limits, not an aggregate cgroup memory quota. All
browser descendants in the process group are killed at completion/timeout.

A current page can exceed these intentional budgets, require unsupported features,
or reference assets outside the capture scope. Unreplayed resources are listed;
a readable result does not imply every image/style was captured. A capture/network
failure is **inconclusive**, not a passing browser test. Exit status: 0 all selected
checks passed, 1 replay/check failure, 2 capture unavailable (unless another case
also failed). Error pages, missing content, enabled scripts, missing navigation
and oversized request counts cannot pass simply by producing a PNG.

Harness guards were tested red/green, including unknown CONNECT destinations,
false-positive error pages, corrupt snapshots, output-path traversal and incomplete
capture batches. New browser failures should become small synthetic regressions
with failure demonstrated before the corresponding fix.
