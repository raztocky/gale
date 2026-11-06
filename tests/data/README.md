`selfsigned.*.der`: a throwaway self-signed certificate for `localhost`, used only by
`tests/privacy.rs` to check that certificate errors are refused. The private key is public on
purpose; it protects nothing.

`tiny*`: small images in each format Brickle decodes (`tiny.svg` uses gradients, clipping, a
filter, `<use>` and CSS), for `src/image.rs`'s tests and as seeds
for the `image` fuzz target.

`example-com*.der`: example.com's public TLS certificate and its issuer (November 2026), for the
certificate parsing in `src/crlite.rs`.

`slow-svg.svg` is the 3,941-byte synthetic SVG input previously found by this
repository's SVG fuzz job (SHA-1 f89158f0436cf06b3606d10c02dbf1e1cee5885c).
It is retained solely as an offline decoder-budget regression case. It is
not a downloaded malware sample. Tests percent-encode it into a local page.
