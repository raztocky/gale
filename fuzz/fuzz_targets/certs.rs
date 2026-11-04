#![no_main]
//! Certificates a server sends, read in the browser process for revocation
//! checking (`crlite.rs`). Nothing may panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(c) = brickle::crlite::parse_cert(data) {
        assert!(c.scts.len() <= 16);
        assert!(c.spki.len() <= data.len() && c.issuer.len() <= data.len());
    }
});
