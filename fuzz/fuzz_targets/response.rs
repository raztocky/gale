#![no_main]
//! Whatever a server sends back is read in the browser process: status
//! line, headers, redirects, content types, compression. This serves the
//! input as a raw HTTP response on a local socket and fetches it, so the
//! whole path runs, including the decompression limits.
use brickle::net::{self, Dest};
use libfuzzer_sys::fuzz_target;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::OnceLock;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;

/// One server for the whole run; each request answers with the next input.
fn server() -> &'static (String, Mutex<Sender<Vec<u8>>>) {
    static S: OnceLock<(String, Mutex<Sender<Vec<u8>>>)> = OnceLock::new();
    S.get_or_init(|| {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/", l.local_addr().unwrap().port());
        let (tx, rx) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let body = rx.recv().unwrap_or_default();
                let mut buf = [0u8; 4096];
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                let _ = s.read(&mut buf);
                let _ = s.write_all(&body);
            }
        });
        (url, Mutex::new(tx))
    })
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let (url, tx) = server();
    let dest = match data[0] % 3 {
        0 => Dest::Document,
        1 => Dest::Stylesheet,
        _ => Dest::Image,
    };
    tx.lock().unwrap().send(data[1..].to_vec()).unwrap();
    // Local addresses are only reachable with public_only off, as for a
    // page you opened from your own network.
    if let Ok(r) = net::http_get(url, dest, None, false, None) {
        assert!(r.body.len() as u64 <= net::MAX_BYTES && r.bytes.len() as u64 <= net::MAX_BYTES);
        if let Some(loc) = &r.location {
            let next = net::resolve(url, loc);
            assert!(next.is_empty() || url::Url::parse(&next).is_ok());
        }
    }
});
