#![no_main]
//! The whole pipeline (parse, cascade, layout, display list, paint) on
//! arbitrary HTML at an arbitrary viewport width.
use brickle::{page::Page, paint};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let width = 50.0 + (u16::from_le_bytes([data[0], data[1]]) % 2000) as f32;
    let src = String::from_utf8_lossy(&data[2..]);
    let page = Page::trusted("https://fuzz.example/", &src);
    let r = page.render(width, 600.0);
    assert!(r.height.is_finite());
    let mut canvas = paint::Canvas::new(64, 64, r.background);
    canvas.paint(&r.list, &r.atlas, 0.0, 0.0);
});
