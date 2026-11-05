#![no_main]
//! Cookies come from servers (`Set-Cookie`) and scripts (`document.cookie`)
//! and are handled in the unsandboxed browser process. Any headers must be
//! stored or refused without panicking, within the jar's limits, and what
//! the jar sends back must be a single, clean header line.
use brickle::cookies::{self, Context, Jar};
use libfuzzer_sys::fuzz_target;

const SITES: [&str; 6] = [
    "https://a.example/",
    "https://www.a.example/path/x",
    "http://a.example/",
    "https://b.example/",
    "https://xn--pple-43d.com/",
    "https://192.0.2.1:8443/q?x=1",
];

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let (pick, rest) = data.split_at(3);
    let text = String::from_utf8_lossy(rest);
    let url = SITES[pick[0] as usize % SITES.len()];
    let top = SITES[pick[1] as usize % SITES.len()];
    let ctx = |nav: bool| Context {
        top,
        initiator: (pick[2] & 1 == 1).then_some(top),
        navigation: nav,
        safe_method: pick[2] & 8 == 8,
    };
    let mut jar = Jar::new();
    let headers: Vec<String> = text.split('\n').map(str::to_string).collect();
    let stored = jar.store(url, ctx(pick[2] & 2 == 2), &headers, pick[2] & 4 == 4);
    assert!(stored.set.len() + stored.refused.len() <= headers.len() * 2 + 1);
    jar.script_set(url, &text);
    for site in SITES {
        if let Some((header, _)) = jar.header_for(site, ctx(true)) {
            assert!(!header.contains(['\r', '\n', '\0']), "{header:?}");
        }
        let s = jar.script_string(site);
        assert!(!s.contains(['\r', '\n']));
        let _ = cookies::site(site);
    }
    for c in jar.sites() {
        assert!(jar.for_site(&c).len() <= 180);
    }
    let _ = cookies::parse_date(&text);
    let _ = cookies::site(&text);
});
