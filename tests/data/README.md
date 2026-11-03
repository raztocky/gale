`selfsigned.*.der`: a throwaway self-signed certificate for `localhost`, used only by
`tests/privacy.rs` to check that certificate errors are refused. The private key is public on
purpose; it protects nothing.
