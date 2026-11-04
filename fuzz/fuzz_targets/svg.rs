#![no_main]
//! SVG images, drawn in the sandboxed renderer: any input must be drawn or
//! refused without panicking, within the image size limits. Input that
//! isn't a whole SVG document is put inside one.
use brickle::image;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let wrapped;
    let svg = if image::sniff(data) == Some(image::Format::Svg) {
        data
    } else {
        wrapped = format!(
            "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink' \
             width='64' height='64'>{}</svg>",
            String::from_utf8_lossy(data)
        );
        wrapped.as_bytes()
    };
    let Ok(d) = image::decode(svg) else { return };
    let b = &d.bitmap;
    assert!(b.w >= 1 && b.h >= 1 && b.w <= image::STORE_SIDE && b.h <= image::STORE_SIDE);
    assert_eq!(b.rgba.len(), b.w as usize * b.h as usize * 4);
    assert!(d.natural.0 <= image::MAX_SIDE && d.natural.1 <= image::MAX_SIDE);
});
