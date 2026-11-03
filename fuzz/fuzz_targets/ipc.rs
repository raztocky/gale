#![no_main]
//! The browser decodes messages from a possibly compromised renderer. Any byte
//! string must decode to an error or a valid message, never panic, and valid
//! messages must survive a re-encode.
use brickle::ipc::{FromRenderer, ToRenderer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(msg) = FromRenderer::decode(data) {
        assert_eq!(FromRenderer::decode(&msg.encode()).unwrap(), msg);
    }
    if let Ok(msg) = ToRenderer::decode(data) {
        assert_eq!(ToRenderer::decode(&msg.encode()).unwrap(), msg);
    }
    let mut r = data;
    let _ = brickle::ipc::read_frame(&mut r);
});
