#![no_main]
//! Filter lists are downloaded from the internet. Parsing any list and
//! matching any URL against it must never panic or hang.
use brickle::filter::{Dest, Filters, Req};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    // Split: the list, then a URL to test (after the last newline).
    let (list, url) = text
        .rsplit_once('\n')
        .unwrap_or((&text, "https://a.example/x.css"));
    let f = Filters::parse(list);
    let url = format!("https://{}", url.trim_start_matches("https://"));
    for dest in [Dest::Document, Dest::Stylesheet] {
        let _ = f.check(&Req::new(&url, Some("https://page.example/"), dest));
        let _ = f.removeparam(&url, None, dest);
    }
    let css = f.cosmetic_css(&url);
    let _ = brickle::css::parse(&css);
    let _ = brickle::urlclean::strip_params(&url, &brickle::urlclean::is_tracking_param);
});
