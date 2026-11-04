#![no_main]
//! URL handling the browser's security decisions rest on (local-network and
//! file blocking, redirects, the address bar), on URLs from pages, servers
//! and you. Nothing may panic, and resolved URLs must be ones the shared
//! parser accepts.
use brickle::{net, urlclean};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (a, b) = text.split_once('\u{1}').unwrap_or((&text, "https://page.example/dir/x.html"));
    let _ = net::canonical(a);
    let _ = net::scheme(a);
    for (base, href) in [(b, a), (a, b), ("https://page.example/dir/", a)] {
        let r = net::resolve(base, href);
        assert!(r.is_empty() || url::Url::parse(&r).is_ok(), "{base:?} + {href:?} -> {r:?}");
        let _ = net::may_load(Some(base), &r);
        let _ = net::fetch_site(Some(base), &r);
    }
    let _ = net::is_local_host(a);
    let (host, rest) = net::display_parts(a);
    // The address bar shows hosts in ASCII, so lookalike letters can't hide.
    assert!(host.is_ascii(), "{host:?}");
    let _ = rest;
    let go = net::address_or_search(a);
    assert!(
        go.is_empty() || url::Url::parse(&go).is_ok() || go.starts_with("about:"),
        "{a:?} -> {go:?}"
    );
    let _ = net::normalize(a);
    let (clean, _) = urlclean::strip_params(a, &urlclean::is_tracking_param);
    if url::Url::parse(a).is_ok() {
        assert!(url::Url::parse(&clean).is_ok(), "{a:?} -> {clean:?}");
    }
});
