//! End-to-end checks of the rules in PRIVACY.md, using the real binary, a
//! local HTTP server and a fake proxy that records every connection attempt.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("brickle-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The binary with an isolated home: its own config/data dirs, no proxy.
fn brickle(home: &PathBuf) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_brickle"));
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env("NO_PROXY", "");
    c
}

/// Records every request (request line + headers) and serves `pages`.
fn serve(pages: Vec<(&'static str, &'static str, String)>) -> (u16, Arc<Mutex<Vec<Vec<String>>>>) {
    serve_dynamic(|_| pages)
}

/// Like `serve`, but the pages can mention the server's own port.
fn serve_dynamic(
    pages: impl FnOnce(u16) -> Vec<(&'static str, &'static str, String)>,
) -> (u16, Arc<Mutex<Vec<Vec<String>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let pages = pages(port);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut lines = Vec::new();
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                    break;
                }
                lines.push(l.trim_end().to_string());
            }
            let path = lines
                .first()
                .and_then(|l| l.split(' ').nth(1))
                .unwrap_or("")
                .to_string();
            seen2.lock().unwrap().push(lines);
            let (ctype, body) = pages
                .iter()
                .find(|(p, _, _)| *p == path)
                .map(|(_, t, b)| (*t, b.clone()))
                .unwrap_or(("text/plain", "not found".into()));
            // A content type may start with a status code: "302 text/html".
            let (status, ctype) = match ctype.split_once(' ') {
                Some((code, rest)) if code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()) => {
                    (code, rest)
                }
                _ => ("200", ctype),
            };
            let mut s = stream;
            let _ = write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, seen)
}

/// A "proxy" that accepts connections and records that they happened.
fn trap() -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(Mutex::new(0));
    let hits2 = hits.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            *hits2.lock().unwrap() += 1;
            drop(s);
        }
    });
    (addr, hits)
}

#[test]
fn blocking_stripping_and_firefox_headers() {
    let home = temp_dir("block");
    std::fs::create_dir_all(home.join("data/brickle/filters")).unwrap();
    std::fs::write(home.join("data/brickle/filters/test.txt"), "/ads/*\n##.ad\n").unwrap();
    std::fs::create_dir_all(home.join("config/brickle")).unwrap();
    std::fs::write(home.join("config/brickle/rules.txt"), "/denied.css\n").unwrap();
    let page = "<link rel=stylesheet href=/ok.css><link rel=stylesheet href=/ads/banner.css>\
                <link rel=stylesheet href='/t.css?keep=1&utm_source=news&fbclid=abc'>\
                <link rel=stylesheet href=/denied.css><p class=ad>ad</p><p>hello</p>"
        .to_string();
    let (port, seen) = serve(vec![
        ("/", "text/html", page),
        ("/ok.css", "text/css", "p { color: red }".into()),
        ("/t.css?keep=1", "text/css", "".into()),
    ]);
    let out = brickle(&home)
        .args(["requests", &format!("http://127.0.0.1:{port}/?utm_campaign=x")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|r| r[0].split(' ').nth(1).unwrap()).collect();
    assert_eq!(paths, vec!["/", "/ok.css", "/t.css?keep=1"], "{stdout}");
    assert!(
        stdout.contains("BLOCKED") && stdout.contains("/ads/*"),
        "{stdout}"
    );
    assert!(
        stdout.contains("DENIED") && stdout.contains("denied.css"),
        "{stdout}"
    );
    assert!(stdout.contains("stripped utm_source, fbclid"), "{stdout}");

    let doc = &seen[0];
    let header = |name: &str| doc.iter().find(|l| l.starts_with(&format!("{name}: "))).cloned();
    assert_eq!(
        header("User-Agent").unwrap(),
        format!(
            "User-Agent: Mozilla/5.0 (X11; Linux x86_64; rv:{v}) Gecko/20100101 Firefox/{v}",
            v = brickle::net::FIREFOX_VERSION
        )
    );
    assert_eq!(
        header("Accept-Language").unwrap(),
        "Accept-Language: en-US,en;q=0.5"
    );
    assert_eq!(header("Sec-Fetch-Mode").unwrap(), "Sec-Fetch-Mode: navigate");
    assert!(header("Referer").is_none(), "no referrer is sent");
    assert!(header("Cookie").is_none());
    let css = &seen[1];
    assert!(css.iter().any(|l| l == "Sec-Fetch-Dest: style"));
    assert!(css.iter().any(|l| l == "Sec-Fetch-Site: same-origin"));
}

#[test]
fn cosmetic_filters_hide_elements() {
    let home = temp_dir("cosmetic");
    std::fs::create_dir_all(home.join("data/brickle/filters")).unwrap();
    std::fs::write(home.join("data/brickle/filters/test.txt"), "##.ad\n").unwrap();
    let page = home.join("page.html");
    std::fs::write(
        &page,
        "<p class=ad style='display:block !important'>AD</p><p>hello</p>",
    )
    .unwrap();
    // Local files have no host, so use dom + render through a server instead.
    let (port, _) = serve(vec![("/", "text/html", std::fs::read_to_string(&page).unwrap())]);
    let url = format!("http://127.0.0.1:{port}/");
    let png_on = home.join("on.png");
    let png_off = home.join("off.png");
    for (png, flag) in [(&png_on, None), (&png_off, Some("--no-filters"))] {
        let mut c = brickle(&home);
        c.args(["render", &url, "-o", png.to_str().unwrap()]);
        if let Some(f) = flag {
            c.arg(f);
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    // With the hiding rule the page is one paragraph shorter.
    let height = |p: &PathBuf| {
        let d = png::Decoder::new(std::fs::File::open(p).unwrap());
        d.read_info().unwrap().info().height
    };
    assert!(
        height(&png_on) < height(&png_off),
        "{} vs {}",
        height(&png_on),
        height(&png_off)
    );
}

#[test]
fn no_network_without_a_request_from_you() {
    // Point every proxy variable at a trap: any connection attempt is counted.
    let home = temp_dir("quiet");
    let (proxy, hits) = trap();
    let page = home.join("p.html");
    std::fs::write(&page, "<link rel=stylesheet href=s.css><p>local</p>").unwrap();
    std::fs::write(home.join("s.css"), "p { color: blue }").unwrap();
    for args in [
        vec![
            "render".to_string(),
            "about:home".into(),
            "-o".into(),
            home.join("a.png").to_string_lossy().into(),
        ],
        vec![
            "render".into(),
            page.to_string_lossy().into(),
            "-o".into(),
            home.join("b.png").to_string_lossy().into(),
        ],
        vec!["dom".into(), page.to_string_lossy().into()],
        vec!["filters".into(), "status".into()],
        vec!["--help".into()],
    ] {
        let out = brickle(&home)
            .args(&args)
            .env("HTTPS_PROXY", &proxy)
            .env("HTTP_PROXY", &proxy)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        *hits.lock().unwrap(),
        0,
        "Brickle made a network connection nobody asked for"
    );
}

#[test]
fn https_only_asks_before_using_plain_http() {
    let home = temp_dir("httpsonly");
    // The HTTPS attempt goes to a dead proxy and fails, like a site without HTTPS.
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let out = brickle(&home)
        .args(["requests", "http://no-https.example/"])
        .env("HTTPS_PROXY", &proxy)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no HTTPS") && stdout.contains("upgraded to https"),
        "{stdout}"
    );
    // Only the https:// attempt was made; nothing went out over plain HTTP.
    assert!(stdout.contains("https://no-https.example/"), "{stdout}");
    assert!(!stdout.contains("INSECURE"), "{stdout}");
}

#[test]
fn only_net_rs_talks_to_the_network() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        if name != "net.rs" && name != "sandbox.rs" {
            for needle in ["ureq::", "TcpStream", "UdpSocket", "std::net::Tcp"] {
                assert!(
                    !text.contains(needle),
                    "{name} uses {needle}; all networking must go through net.rs"
                );
            }
        }
    }
}

#[test]
fn local_proxy_still_works_for_public_pages() {
    // A proxy on 127.0.0.1 (like Tor or Privoxy) must not trip the
    // local-network protection for a public page's subresources.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let connects = Arc::new(Mutex::new(Vec::new()));
    let c2 = connects.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            c2.lock().unwrap().push(first.trim().to_string());
            let mut l = String::new();
            while reader.read_line(&mut l).unwrap_or(0) > 0 && l != "\r\n" {
                l.clear();
            }
            let mut s = stream;
            if first.starts_with("CONNECT") {
                let _ = s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                // Then act as the origin server inside the tunnel.
                loop {
                    l.clear();
                    if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                        break;
                    }
                }
            }
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    std::env::set_var("HTTPS_PROXY", &proxy);
    std::env::set_var("NO_PROXY", "");
    let r = brickle::net::http_get(
        "http://public.example/x.css",
        brickle::net::Dest::Stylesheet,
        Some("https://page.example/"),
        true,
        None,
    );
    std::env::remove_var("HTTPS_PROXY");
    assert_eq!(r.map(|r| r.body).unwrap(), "ok");
    assert!(
        connects.lock().unwrap()[0].contains("public.example"),
        "{:?}",
        connects.lock().unwrap()
    );

    // But a literal local address is still refused, proxy or not.
    let r = brickle::net::http_get(
        "http://192.168.0.1/x.css",
        brickle::net::Dest::Stylesheet,
        Some("https://page.example/"),
        true,
        None,
    );
    assert_eq!(r.err(), Some(brickle::net::NetError::LocalNetwork));
}

/// PRIVACY.md rule 8: first-party cookies work, third parties get nothing,
/// and nothing survives a restart unless you keep the site.
#[test]
fn cookies_first_party_only_and_session_only() {
    let home = temp_dir("cookies");
    // `Content-Type` is written raw, so a Set-Cookie header can ride along.
    let set = |c: &str| -> &'static str { format!("text/html\r\nSet-Cookie: {c}").leak() };
    let (port, seen) = serve_dynamic(|port| {
        vec![
            (
                "/",
                set("sid=1; Max-Age=3600; HttpOnly"),
                format!(
                    "<link rel=stylesheet href=/same.css>\
                     <link rel=stylesheet href=http://localhost:{port}/third.css><p>hi"
                ),
            ),
            ("/same.css", "text/css", String::new()),
            ("/third.css", set("track=1; Max-Age=3600"), String::new()),
        ]
    });
    let url = format!("http://127.0.0.1:{port}/");
    let run = || {
        let out = brickle(&home).args(["requests", &url]).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{stdout}");
        stdout
    };
    let cookie_of = |req: &Vec<String>| req.iter().find(|l| l.starts_with("Cookie: ")).cloned();

    let out = run();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "{out}");
        assert_eq!(cookie_of(&seen[0]), None, "fresh start has no cookies");
        assert_eq!(
            cookie_of(&seen[1]).as_deref(),
            Some("Cookie: sid=1"),
            "{:?}",
            seen[1]
        );
        assert_eq!(cookie_of(&seen[2]), None, "third party gets nothing");
    }
    assert!(out.contains("set cookies sid"), "{out}");
    assert!(out.contains("refused cookies track (third-party)"), "{out}");

    // A new session forgets everything...
    run();
    assert_eq!(cookie_of(&seen.lock().unwrap()[3]), None);

    // ...unless you keep the site.
    let keep = brickle(&home)
        .args(["cookies", "keep", "127.0.0.1"])
        .output()
        .unwrap();
    assert!(keep.status.success());
    run();
    run();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 12);
    assert_eq!(
        cookie_of(&seen[9]).as_deref(),
        Some("Cookie: sid=1"),
        "{:?}",
        seen[9]
    );
    assert!(!seen
        .iter()
        .any(|r| cookie_of(r).is_some_and(|c| c.contains("track"))));
    let saved = std::fs::read_to_string(home.join("data/brickle/cookies.txt")).unwrap();
    assert!(saved.contains("sid") && !saved.contains("track"));
}

/// PRIVACY.md rule 5: a certificate that fails verification is an error,
/// never a reason to offer plain HTTP.
#[test]
fn bad_certificates_are_refused_not_downgraded() {
    use std::sync::Arc as A;
    let cert = rustls::pki_types::CertificateDer::from(include_bytes!("data/selfsigned.crt.der").to_vec());
    let key =
        rustls::pki_types::PrivateKeyDer::Pkcs8(include_bytes!("data/selfsigned.key.der").to_vec().into());
    let config = A::new(
        rustls::ServerConfig::builder_with_provider(A::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = Arc::new(Mutex::new(0));
    let served2 = served.clone();
    std::thread::spawn(move || {
        for mut tcp in listener.incoming().flatten() {
            let mut conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
            let mut buf = [0u8; 1024];
            if let Ok(n) = std::io::Read::read(&mut tls, &mut buf) {
                if n > 0 {
                    *served2.lock().unwrap() += 1; // the request got through
                }
            }
        }
    });
    let home = temp_dir("cert");
    let out = brickle(&home)
        .args(["requests", &format!("https://localhost:{port}/")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("BLOCKED (bad certificate)"), "{stdout}");
    assert!(!stdout.contains("no HTTPS"), "{stdout}");
    assert_eq!(
        *served.lock().unwrap(),
        0,
        "no request was sent over the bad connection"
    );
    let page = brickle(&home)
        .args(["dom", &format!("https://localhost:{port}/")])
        .output()
        .unwrap();
    let dom = String::from_utf8_lossy(&page.stdout);
    assert!(dom.contains("couldn't verify this site's identity"), "{dom}");
    assert!(!dom.contains("brickle:proceed"), "no way around it: {dom}");
}

/// Cookies are worked out again at every redirect hop, and their values
/// never appear in the request log.
#[test]
fn cookies_follow_redirect_hops_and_values_stay_out_of_logs() {
    let home = temp_dir("cookie-redirect");
    let (port, seen) = serve_dynamic(|port| {
        vec![
            (
                "/login",
                "302 text/html\r\nLocation: /home\r\nSet-Cookie: login=s3cretvalue; Path=/; HttpOnly",
                String::new(),
            ),
            (
                "/home",
                // On to another site, which must not get 127.0.0.1's cookie.
                Box::leak(
                    format!("302 text/html\r\nLocation: http://localhost:{port}/other").into_boxed_str(),
                ),
                String::new(),
            ),
            ("/other", "text/html", "<p>done".into()),
        ]
    });
    let out = brickle(&home)
        .args(["requests", &format!("http://127.0.0.1:{port}/login")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let seen = seen.lock().unwrap();
    let cookie_of = |i: usize| seen[i].iter().find(|l| l.starts_with("Cookie: ")).cloned();
    assert_eq!(seen.len(), 3, "{stdout}");
    assert_eq!(cookie_of(0), None);
    assert_eq!(
        cookie_of(1).as_deref(),
        Some("Cookie: login=s3cretvalue"),
        "{stdout}"
    );
    assert_eq!(cookie_of(2), None, "another site: {stdout}");
    assert!(
        stdout.contains("set cookies login") && stdout.contains("sent cookies login"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("s3cretvalue"),
        "values are never logged: {stdout}"
    );
}
