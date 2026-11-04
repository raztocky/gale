#![no_main]
//! CSS gradients come from any page: parsing must accept anything, and
//! drawing whatever parsed into any box must stay within its size limit.
use brickle::gradient;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let (size, text) = data.split_at(2);
    let text = String::from_utf8_lossy(text).to_ascii_lowercase();
    let Some(g) = gradient::parse(&text, 16.0) else {
        return;
    };
    assert!(g.stops.len() >= 2 && g.stops.len() <= gradient::MAX_STOPS + 1);
    let _ = gradient::average(&g);
    // Small boxes keep each run fast; the cap is tested separately.
    let (w, h) = (size[0] as f32 / 2.0, size[1] as f32 / 2.0);
    if let Some(b) = gradient::render(&g, w, h) {
        assert!(b.w >= 1 && b.h >= 1 && b.w <= gradient::MAX_SIDE && b.h <= gradient::MAX_SIDE);
        assert_eq!(b.rgba.len(), (b.w * b.h * 4) as usize);
    }
});
