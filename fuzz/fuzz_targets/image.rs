#![no_main]
//! The image decoders must reject or decode any input without panicking,
//! within their size limits; resizing must give exactly the size asked for.
use brickle::image;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(d) = image::decode(data) else { return };
    let b = &d.bitmap;
    assert!(b.w >= 1 && b.h >= 1 && b.w <= image::STORE_SIDE && b.h <= image::STORE_SIDE);
    assert_eq!(b.rgba.len(), b.w as usize * b.h as usize * 4);
    assert!(d.natural.0 <= image::MAX_SIDE && d.natural.1 <= image::MAX_SIDE);
    // A size derived from the input, shrinking and enlarging.
    let w = 1 + data.first().copied().unwrap_or(0) as u32 % 64;
    let h = 1 + data.last().copied().unwrap_or(0) as u32 % 64;
    let r = image::resize(b, w, h);
    assert_eq!((r.w, r.h), (w, h));
    assert_eq!(r.rgba.len(), (w * h * 4) as usize);
});
