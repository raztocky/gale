#![no_main]
//! The browser decodes messages from a possibly compromised renderer. Any byte
//! string must decode to an error or a valid message, never panic, and valid
//! messages must survive a re-encode and be paintable.
use brickle::ipc::{FromRenderer, ToRenderer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(msg) = FromRenderer::decode(data) {
        assert_eq!(FromRenderer::decode(&msg.encode()).unwrap(), msg);
        // The browser paints whatever a (possibly hostile) renderer sends.
        if let FromRenderer::Rendered(r) = &msg {
            let mut canvas = brickle::paint::Canvas::new(64, 48, r.background);
            canvas.paint(&r.list, &r.atlas, 0.0, 10.0);
            canvas.paint(&r.list, &r.atlas, 1e6, 10.0);
            canvas.paint(&r.list, &r.atlas, -1e6, 10.0);
            let _ = r.link_at(10.0, 10.0);
        }
    }
    if let Ok(msg) = ToRenderer::decode(data) {
        assert_eq!(ToRenderer::decode(&msg.encode()).unwrap(), msg);
    }
    let mut r = data;
    let _ = brickle::ipc::read_frame(&mut r);
});
