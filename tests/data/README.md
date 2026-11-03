`selfsigned.*.der`: a throwaway self-signed certificate for `localhost`, used only by
`tests/privacy.rs` to check that certificate errors are refused. The private key is public on
purpose; it protects nothing.

`tiny*`: small images in each format Brickle decodes, for `src/image.rs`'s tests and as seeds
for the `image` fuzz target.
