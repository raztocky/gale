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
            // Keep a request body as a "BODY: ..." line.
            let len = lines
                .iter()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|n| n.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if len > 0 {
                let mut body = vec![0u8; len.min(1 << 20)];
                if std::io::Read::read_exact(&mut reader, &mut body).is_ok() {
                    lines.push(format!("BODY: {}", String::from_utf8_lossy(&body)));
                }
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
            // A body may be binary, written as "hex:89504e47...".
            let body: Vec<u8> = match body.strip_prefix("hex:") {
                Some(h) => (0..h.len() / 2)
                    .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
                    .collect(),
                None => body.into_bytes(),
            };
            let mut s = stream;
            let _ = write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
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
            "User-Agent: Mozilla/5.0 (X11; Linux x86_64; rv:{v}.0) Gecko/20100101 Firefox/{v}.0",
            v = brickle::net::firefox_version().0
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
        let d = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(p).unwrap()));
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
    // The HTTPS attempt goes to a proxy that hangs up at once, and fails like
    // a site without HTTPS. (The port stays ours: a freed one could be
    // picked up by another test's listener running alongside.)
    let (proxy, _hangups) = trap();
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
    // The check sets HTTPS_PROXY, which is process-wide: run it in a
    // process of its own so it can't send other tests' requests through
    // the fake proxy.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "local_proxy_check", "--ignored", "--test-threads=1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
}

/// The body of `local_proxy_still_works_for_public_pages`, run in its own
/// process.
#[test]
#[ignore]
fn local_proxy_check() {
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

/// JavaScript is off unless you allow a site. When on, scripts and their
/// requests go through the same policy as everything else, they can't send
/// to other sites unless you allow it, CORS keeps other sites' responses
/// from the page, and HttpOnly cookies stay hidden.
#[test]
fn javascript_is_off_by_default_and_its_requests_follow_the_rules() {
    let home = temp_dir("js");
    std::fs::create_dir_all(home.join("data/brickle/filters")).unwrap();
    std::fs::write(
        home.join("data/brickle/filters/test.txt"),
        "/tracker.js$script\n/collect$xmlhttprequest\n",
    )
    .unwrap();
    let (port, seen) = serve_dynamic(|port| {
        let page = format!(
            "<p id=out>static</p>\
             <script>document.getElementById('out').textContent = 'inline ran';\
             console.log('cookie', document.cookie);\
             document.cookie = 'lang=en; Path=/';\
             fetch('/api').then(r => r.text()).then(t => console.log('same-origin', t));\
             fetch('http://localhost:{port}/open').then(r => r.text()).then(t => console.log('cors ok', t));\
             fetch('http://localhost:{port}/closed').then(r => r.text()).then(t => console.log('LEAK', t),\
               e => console.log('cors blocked'));\
             fetch('http://localhost:{port}/custom', {{headers: {{'X-Custom': '1'}}}}).catch(e => console.log('preflight refused'));\
             fetch('http://localhost:{port}/put', {{method: 'PUT', headers: {{'X-Custom': '1'}}}})\
               .then(r => r.text()).then(t => console.log('preflight ok', t));\
             fetch('/collect').catch(e => console.log('filtered'));\
             navigator.sendBeacon('/beacon');\
             for (const src of ['http://localhost:{port}/pixel.png?id=42', '/own.png']) {{\
               const i = document.createElement('img'); i.src = src; document.body.appendChild(i);\
             }}\
             const s = document.createElement('script'); s.src = 'http://localhost:{port}/added.js';\
             document.body.appendChild(s);\
             const l = document.createElement('link'); l.rel = 'stylesheet';\
             l.href = 'http://localhost:{port}/added.css'; document.body.appendChild(l);\
             const o = document.createElement('script'); o.src = '/own.js'; document.body.appendChild(o);</script>\
             <img src=http://localhost:{port}/listed.png>\
             <script src=/app.js></script><script src=/tracker.js></script>"
        );
        vec![
            (
                "/",
                "text/html\r\nSet-Cookie: sid=secret; HttpOnly\r\nSet-Cookie: theme=dark",
                page,
            ),
            (
                "/app.js",
                "application/javascript",
                "document.title = 'app ran';".into(),
            ),
            ("/tracker.js", "application/javascript", "".into()),
            ("/api", "text/plain", "from api".into()),
            (
                "/open",
                "text/plain\r\nAccess-Control-Allow-Origin: *",
                "public data".into(),
            ),
            ("/closed", "text/plain", "private data".into()),
            (
                "/put",
                "text/plain\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: PUT\r\n\
                 Access-Control-Allow-Headers: X-Custom",
                "put done".into(),
            ),
        ]
    });
    let url = format!("http://127.0.0.1:{port}/");
    let paths = |seen: &Arc<Mutex<Vec<Vec<String>>>>| -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|r| r[0].split(' ').nth(1).unwrap().to_string())
            .collect()
    };

    // Off by default: no script is fetched or run.
    let out = brickle(&home).args(["dom", &url]).output().unwrap();
    let dom = String::from_utf8_lossy(&out.stdout);
    assert!(dom.contains("\"static\""), "{dom}");
    assert_eq!(paths(&seen), vec!["/", "/listed.png"]);
    seen.lock().unwrap().clear();

    // Allowed for the site.
    let allow = brickle(&home)
        .args(["js", "allow", "127.0.0.1"])
        .output()
        .unwrap();
    assert!(allow.status.success());
    // Scripts can't send to other sites: not their requests, nor images
    // they put on the page. Images in the page as sent load as usual.
    let out = brickle(&home).args(["requests", &url]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    for want in [
        "[log] same-origin from api",
        "BLOCKED (script sending to another site) xhr   http://localhost",
        "BLOCKED (script sending to another site) img   http://localhost",
    ] {
        assert!(stdout.contains(want), "missing {want:?}:\n{stdout}");
    }
    let got = paths(&seen);
    for p in ["/", "/app.js", "/api", "/own.png", "/own.js", "/listed.png"] {
        assert!(got.contains(&p.to_string()), "{p} not requested: {got:?}");
    }
    for p in [
        "/open",
        "/closed",
        "/custom",
        "/put",
        "/pixel.png?id=42",
        "/added.js",
        "/added.css",
    ] {
        assert!(!got.contains(&p.to_string()), "{p} must not be sent: {got:?}");
    }
    seen.lock().unwrap().clear();

    // Allowed for this site (the rule clicking the request adds): then CORS
    // decides what the page may read.
    std::fs::create_dir_all(home.join("config/brickle")).unwrap();
    std::fs::write(
        home.join("config/brickle/rules.txt"),
        "@@||localhost^$xhr,domain=127.0.0.1\n",
    )
    .unwrap();
    let out = brickle(&home).args(["requests", &url]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    for want in [
        "[log] cookie theme=dark",
        "[log] same-origin from api",
        "[log] cors ok public data",
        "[log] cors blocked",
        "[log] preflight refused",
        "[log] preflight ok put done",
        "[log] filtered",
        "BLOCKED  js",
        "BLOCKED  xhr",
        "response hidden from the page (CORS)",
    ] {
        assert!(stdout.contains(want), "missing {want:?}:\n{stdout}");
    }
    assert!(!stdout.contains("LEAK"), "{stdout}");
    let got = paths(&seen);
    for p in ["/", "/app.js", "/api", "/open", "/closed"] {
        assert!(got.contains(&p.to_string()), "{p} not requested: {got:?}");
    }
    // A preflight (no cookies) for each non-simple request; only the one the
    // server agreed to was sent.
    let lines: Vec<String> = seen.lock().unwrap().iter().map(|r| r[0].clone()).collect();
    for want in ["OPTIONS /custom ", "OPTIONS /put ", "PUT /put "] {
        assert!(
            lines.iter().any(|l| l.starts_with(want)),
            "no {want:?}: {lines:?}"
        );
    }
    assert!(!lines.iter().any(|l| l.starts_with("GET /custom")), "{lines:?}");
    {
        let seen = seen.lock().unwrap();
        let ask = seen.iter().find(|r| r[0].starts_with("OPTIONS /put ")).unwrap();
        let has = |h: &str| ask.iter().any(|l| l.eq_ignore_ascii_case(h));
        assert!(has(&format!("Origin: http://127.0.0.1:{port}")), "{ask:?}");
        assert!(has("Access-Control-Request-Method: PUT"), "{ask:?}");
        assert!(has("Access-Control-Request-Headers: x-custom"), "{ask:?}");
        assert!(
            !ask.iter().any(|l| l.to_ascii_lowercase().starts_with("cookie:")),
            "{ask:?}"
        );
    }
    for p in [
        "/tracker.js",
        "/collect",
        "/beacon",
        "/pixel.png?id=42",
        "/added.js",
        "/added.css",
    ] {
        assert!(!got.contains(&p.to_string()), "{p} must not be sent: {got:?}");
    }
    let seen = seen.lock().unwrap();
    let req = |path: &str| {
        seen.iter()
            .find(|r| r[0].split(' ').nth(1) == Some(path))
            .unwrap()
            .clone()
    };
    let header =
        |r: &Vec<String>, name: &str| r.iter().find(|l| l.starts_with(&format!("{name}: "))).cloned();
    // Same-origin: cookies (including HttpOnly, and the one the script set).
    assert_eq!(
        header(&req("/api"), "Cookie").as_deref(),
        Some("Cookie: sid=secret; theme=dark; lang=en")
    );
    assert_eq!(
        header(&req("/api"), "Sec-Fetch-Mode").as_deref(),
        Some("Sec-Fetch-Mode: same-origin")
    );
    // Cross-site: an Origin header and no cookies.
    let open = req("/open");
    assert_eq!(
        header(&open, "Origin"),
        Some(format!("Origin: http://127.0.0.1:{port}"))
    );
    assert_eq!(header(&open, "Cookie"), None);
    assert_eq!(
        header(&open, "Sec-Fetch-Mode").as_deref(),
        Some("Sec-Fetch-Mode: cors")
    );
    assert_eq!(
        header(&req("/app.js"), "Sec-Fetch-Dest").as_deref(),
        Some("Sec-Fetch-Dest: script")
    );
    drop(seen);

    // --no-sandbox never runs scripts.
    let out = brickle(&home)
        .args(["--no-sandbox", "dom", &url])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"static\""));
}

/// A script request that's redirected gets every check again at each hop,
/// before anything is sent there: the other-site rule (also for files
/// scripts put on the page), `mode`, `redirect`, CORS on the redirect, and a
/// preflight; and a script's `Authorization` never follows it to another
/// origin.
#[test]
fn script_requests_are_checked_at_every_redirect() {
    let home = temp_dir("js-redirects");
    let (port, seen) = serve_dynamic(|port| {
        let other = format!("http://localhost:{port}");
        let page = format!(
            "<script>\
             const log = (m) => console.log(m);\
             if (location.search !== '?b') {{\
             fetch('/bounce').then(() => log('LEAK bounce'), () => log('bounce refused'));\
             const i = document.createElement('img'); i.src = '/img-bounce'; document.body.appendChild(i);\
             fetch('/same-redirect').then(r => r.text()).then(t => log('followed ' + t));\
             fetch('/r-error', {{redirect: 'error'}}).then(() => log('LEAK error'), () => log('redirect error'));\
             fetch('/r-manual', {{redirect: 'manual'}}).then(r => log('manual ' + r.type + ' ' + r.status));\
             fetch('{other}/open', {{mode: 'same-origin'}}).then(() => log('LEAK mode'), () => log('mode refused'));\
             }} else {{\
               fetch('/to-put', {{method: 'PUT', headers: {{'X-Custom': '1', 'Authorization': 'Bearer s3cret'}}}})\
                 .then(r => r.text()).then(t => log('put ' + t), e => log('put failed ' + e));\
               fetch('/to-closed', {{method: 'PUT', headers: {{'X-Custom': '1'}}}})\
                 .then(() => log('LEAK closed'), () => log('closed preflight refused'));\
               fetch('{other}/xbounce').then(() => log('LEAK xbounce'), () => log('xbounce refused'));\
               fetch('/bounce2', {{mode: 'same-origin'}}).then(() => log('LEAK bounce2'), () => log('bounce2 refused'));\
             }}</script>"
        );
        let to = |path: &str| format!("302 text/plain\r\nLocation: {other}{path}");
        let leak = |p: &'static str| {
            (
                p,
                "text/plain\r\nAccess-Control-Allow-Origin: *",
                "LEAK".to_string(),
            )
        };
        vec![
            ("/", "text/html", page.clone()),
            ("/?b", "text/html", page),
            (
                "/bounce",
                Box::leak(to("/stolen?c=1").into_boxed_str()),
                String::new(),
            ),
            (
                "/img-bounce",
                Box::leak(to("/pixel2.png").into_boxed_str()),
                String::new(),
            ),
            (
                "/same-redirect",
                "302 text/plain\r\nLocation: /api4",
                String::new(),
            ),
            ("/api4", "text/plain", "ok".into()),
            ("/r-error", "302 text/plain\r\nLocation: /api2", String::new()),
            ("/r-manual", "302 text/plain\r\nLocation: /api3", String::new()),
            (
                "/to-put",
                Box::leak(format!("307 text/plain\r\nLocation: {other}/put2").into_boxed_str()),
                String::new(),
            ),
            (
                "/put2",
                "text/plain\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: PUT\r\n\
                 Access-Control-Allow-Headers: X-Custom",
                "done".into(),
            ),
            (
                "/to-closed",
                Box::leak(format!("307 text/plain\r\nLocation: {other}/closed2").into_boxed_str()),
                String::new(),
            ),
            ("/closed2", "text/plain", "LEAK".into()),
            // A cross-origin redirect that doesn't allow the page.
            (
                "/xbounce",
                Box::leak(to("/open2").into_boxed_str()),
                String::new(),
            ),
            (
                "/bounce2",
                Box::leak(to("/stolen2").into_boxed_str()),
                String::new(),
            ),
            leak("/stolen?c=1"),
            leak("/stolen2"),
            leak("/pixel2.png"),
            leak("/api2"),
            leak("/api3"),
            leak("/open"),
            leak("/open2"),
        ]
    });
    let url = format!("http://127.0.0.1:{port}/");
    assert!(brickle(&home)
        .args(["js", "allow", "127.0.0.1"])
        .output()
        .unwrap()
        .status
        .success());
    let lines = |seen: &Arc<Mutex<Vec<Vec<String>>>>| -> Vec<String> {
        seen.lock().unwrap().iter().map(|r| r[0].clone()).collect()
    };
    let run = |url: &str| {
        let out = brickle(&home).args(["requests", url]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // Without a rule allowing the other site.
    let stdout = run(&url);
    for want in [
        "[log] bounce refused",
        "[log] followed ok",
        "[log] redirect error",
        "[log] manual opaqueredirect 0",
        "[log] mode refused",
        "BLOCKED (script sending to another site) xhr   http://localhost",
        "BLOCKED (script sending to another site) img   http://localhost",
    ] {
        assert!(stdout.contains(want), "missing {want:?}:\n{stdout}");
    }
    assert!(!stdout.contains("LEAK"), "{stdout}");
    let got = lines(&seen);
    for p in ["/stolen", "/pixel2.png", "/api2", "/api3", "/open "] {
        assert!(
            !got.iter().any(|l| l.contains(p)),
            "{p} must not be sent: {got:?}"
        );
    }
    assert!(got.iter().any(|l| l.starts_with("GET /api4 ")), "{got:?}");
    seen.lock().unwrap().clear();

    // With one: CORS and preflights decide, at each hop.
    std::fs::create_dir_all(home.join("config/brickle")).unwrap();
    std::fs::write(
        home.join("config/brickle/rules.txt"),
        "@@||localhost^$xhr,domain=127.0.0.1\n",
    )
    .unwrap();
    let stdout = run(&format!("{url}?b"));
    for want in [
        "[log] put done",
        "[log] closed preflight refused",
        "[log] xbounce refused",
        "[log] bounce2 refused",
    ] {
        assert!(stdout.contains(want), "missing {want:?}:\n{stdout}");
    }
    assert!(!stdout.contains("LEAK"), "{stdout}");
    let got = lines(&seen);
    for p in ["PUT /closed2", "GET /open2", "GET /stolen2"] {
        assert!(
            !got.iter().any(|l| l.starts_with(p)),
            "{p} must not be sent: {got:?}"
        );
    }
    // The preflight for the second hop came before the PUT there.
    let at = |want: &str| got.iter().position(|l| l.starts_with(want));
    assert!(
        at("OPTIONS /put2 ").unwrap() < at("PUT /put2 ").unwrap(),
        "{got:?}"
    );
    assert!(at("OPTIONS /closed2 ").is_some(), "{got:?}");
    // Authorization went to the page's own origin, not across the redirect.
    let seen = seen.lock().unwrap();
    let req = |start: &str| seen.iter().find(|r| r[0].starts_with(start)).unwrap().clone();
    let auth = |r: &Vec<String>| {
        r.iter()
            .any(|l| l.to_ascii_lowercase().starts_with("authorization:"))
    };
    assert!(auth(&req("PUT /to-put ")));
    assert!(!auth(&req("PUT /put2 ")), "{:?}", req("PUT /put2 "));
    assert!(!auth(&req("OPTIONS /put2 ")));
}

/// Forms: what you type is submitted only when you submit, through the
/// browser's checks, with Firefox's headers and the page's own cookies.
#[test]
fn forms_submit_through_the_browser() {
    use brickle::page::Key;
    use brickle::tab::{Input, Tab};
    let (port, seen) = serve(vec![
        (
            "/",
            "text/html\r\nSet-Cookie: sid=1",
            "<form method=post action=/login><input name=user><input type=password name=pw>\
             <input type=submit value=Go></form>\
             <form action=/search><input name=q></form>"
                .into(),
        ),
        ("/login", "text/html", "<p>welcome".into()),
        ("/search", "text/html", "<p>results".into()),
    ]);
    let url = format!("http://127.0.0.1:{port}/");
    let mut net = brickle::network::Network::empty();
    // In-process here (the test binary can't be a sandboxed renderer); the
    // form code is the same.
    let mut tab = Tab::open(&mut net, &url, None, false);
    let _ = tab.render(800.0, 600.0);
    for input in [
        Input::Key(Key::Tab),
        Input::Type("raz".into()),
        Input::Key(Key::Tab),
        Input::Type("p&ss w=rd".into()),
    ] {
        let r = tab.interact(&mut net, input);
        assert!(r.submit.is_none() && r.focus);
    }
    assert_eq!(seen.lock().unwrap().len(), 1, "typing sends nothing");
    let r = tab.interact(&mut net, Input::Key(Key::Enter));
    let sub = r.submit.expect("Enter submits");
    let _ = Tab::submit(&mut net, &sub, Some(&url), false, (800.0, 600.0));
    let log = seen.clone();
    let seen = seen.lock().unwrap();
    let post = &seen[1];
    assert_eq!(post[0], "POST /login HTTP/1.1");
    let header = |name: &str| post.iter().find(|l| l.starts_with(&format!("{name}: "))).cloned();
    assert_eq!(
        header("Content-Type").as_deref(),
        Some("Content-Type: application/x-www-form-urlencoded")
    );
    assert_eq!(header("Origin"), Some(format!("Origin: http://127.0.0.1:{port}")));
    assert_eq!(header("Cookie").as_deref(), Some("Cookie: sid=1"));
    assert_eq!(
        header("Sec-Fetch-Mode").as_deref(),
        Some("Sec-Fetch-Mode: navigate")
    );
    assert_eq!(header("BODY").as_deref(), Some("BODY: user=raz&pw=p%26ss+w%3Drd"));
    drop(seen);

    // The same POST from a page on another site (localhost isn't
    // 127.0.0.1's site): the cookie (Lax, by default) stays home, so a
    // hostile form can't ride on your login. A link from there still
    // carries it, as Lax allows.
    let evil = format!("http://localhost:{port}/");
    let _ = Tab::submit(&mut net, &sub, Some(&evil), false, (800.0, 600.0));
    let _ = Tab::open(
        &mut net,
        &format!("http://127.0.0.1:{port}/search"),
        Some(&evil),
        false,
    );
    let seen = log.lock().unwrap();
    let n = seen.len();
    let cookie = |req: &Vec<String>| req.iter().any(|l| l.starts_with("Cookie: "));
    assert_eq!(seen[n - 2][0], "POST /login HTTP/1.1");
    assert!(!cookie(&seen[n - 2]), "cross-site POST: {:?}", seen[n - 2]);
    assert!(cookie(&seen[n - 1]), "cross-site link: {:?}", seen[n - 1]);
    drop(seen);

    // A GET form goes to its URL with the data as the query.
    let mut tab = Tab::open(&mut net, &url, None, false);
    let _ = tab.render(800.0, 600.0);
    for k in [Key::Tab, Key::Tab, Key::Tab] {
        tab.interact(&mut net, Input::Key(k));
    }
    tab.interact(&mut net, Input::Type("cats".into()));
    let sub = tab.interact(&mut net, Input::Key(Key::Enter)).submit.unwrap();
    assert_eq!(sub.url, format!("http://127.0.0.1:{port}/search?q=cats"));
    assert!(!sub.post);
}

/// A page that answers a POST remembers its form, so reloading it asks
/// before sending the form again; one reached by a redirect after the POST
/// (the usual pattern) is a plain GET and reloads freely.
#[test]
fn pages_answering_a_post_remember_their_form() {
    use brickle::forms::Submission;
    use brickle::tab::Tab;
    let (port, _) = serve(vec![
        ("/buy", "text/html", "<p>thanks".into()),
        ("/send", "303 text/html\r\nLocation: /done", String::new()),
        ("/done", "text/html", "<p>sent".into()),
    ]);
    let from = format!("http://127.0.0.1:{port}/");
    let mut net = brickle::network::Network::empty();
    let post = |path: &str| Submission {
        url: format!("http://127.0.0.1:{port}{path}"),
        post: true,
        body: Some("item=1".into()),
        content_type: Some("application/x-www-form-urlencoded".into()),
    };
    let tab = Tab::submit(&mut net, &post("/buy"), Some(&from), false, (800.0, 600.0));
    assert_eq!(tab.resubmit, Some(post("/buy")));
    let tab = Tab::submit(&mut net, &post("/send"), Some(&from), false, (800.0, 600.0));
    assert!(tab.url.ends_with("/done"), "{}", tab.url);
    assert_eq!(tab.resubmit, None);
    let get = Submission {
        post: false,
        body: None,
        ..post("/buy")
    };
    let tab = Tab::submit(&mut net, &get, Some(&from), false, (800.0, 600.0));
    assert_eq!(tab.resubmit, None);
    let tab = Tab::open(&mut net, &from, None, false);
    assert_eq!(tab.resubmit, None);
}

/// Images go through the same rules as everything else: your rules and
/// `$image` filters, first-party-only cookies, Firefox's headers. Only real
/// images are accepted (no HTML posing as an image; an SVG is drawn as a
/// picture and its scripts never run), and they are decoded in the
/// sandboxed renderer.
#[test]
fn images_follow_the_rules() {
    let home = temp_dir("images");
    std::fs::create_dir_all(home.join("data/brickle/filters")).unwrap();
    std::fs::write(home.join("data/brickle/filters/test.txt"), "/track/*$image\n").unwrap();
    let png = {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, 4, 2);
        e.set_color(png::ColorType::Rgb);
        e.write_header().unwrap().write_image_data(&[90; 24]).unwrap();
        out
    };
    let hex: String = png.iter().map(|b| format!("{b:02x}")).collect();
    let hex: &'static str = format!("hex:{hex}").leak();
    let (port, seen) = serve_dynamic(|port| {
        vec![
            (
                "/",
                "text/html\r\nSet-Cookie: sid=1",
                format!(
                    "<img src=/a.png alt=A><img src=/track/pixel.gif?utm_source=mail>\
                     <link rel=stylesheet href=/track/style.css>\
                     <img src=/vector.svg><img src=/page.html>\
                     <img src=http://localhost:{port}/third.png><img src=/a.png>"
                ),
            ),
            ("/a.png", "image/png", hex.into()),
            ("/third.png", "image/png\r\nSet-Cookie: t=1", hex.into()),
            ("/track/style.css", "text/css", String::new()),
            ("/vector.svg", "image/svg+xml", "<svg onload=alert(1)/>".into()),
            ("/page.html", "text/html", "<p>not an image".into()),
        ]
    });
    let url = format!("http://127.0.0.1:{port}/");
    let out = brickle(&home).args(["requests", &url]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(
        paths,
        [
            "GET / HTTP/1.1",
            "GET /track/style.css HTTP/1.1",
            "GET /a.png HTTP/1.1",
            "GET /vector.svg HTTP/1.1",
            "GET /page.html HTTP/1.1",
            "GET /third.png HTTP/1.1",
        ],
        "the $image rule blocks only the image; each image is fetched once\n{stdout}"
    );
    let header = |i: usize, name: &str| {
        seen[i]
            .iter()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
            .map(str::to_string)
    };
    assert_eq!(header(2, "Sec-Fetch-Dest").as_deref(), Some("image"));
    assert_eq!(header(2, "Sec-Fetch-Mode").as_deref(), Some("no-cors"));
    assert_eq!(
        header(2, "Accept").as_deref(),
        Some("image/avif,image/webp,image/png,image/svg+xml,image/*;q=0.8,*/*;q=0.5")
    );
    assert_eq!(header(2, "Cookie").as_deref(), Some("sid=1"), "first-party image");
    assert_eq!(header(5, "Cookie"), None, "third-party image gets no cookies");
    assert!(stdout.contains("refused cookies t (third-party)"), "{stdout}");
    assert!(
        stdout.contains("pixel.gif"),
        "the blocked image is listed: {stdout}"
    );
    assert!(
        !stdout.contains("?utm_source") && stdout.contains("stripped utm_source"),
        "tracking parameters are stripped from images too: {stdout}"
    );

    drop(seen); // the server records into it

    // The page renders with the picture in it, decoded by the renderer.
    let png_out = home.join("page.png");
    let out = brickle(&home)
        .args(["render", &url, "-o", png_out.to_str().unwrap(), "-w", "200"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let d = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&png_out).unwrap()));
    let mut r = d.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    let px = info.color_type.samples();
    let grey = buf
        .chunks(px)
        .filter(|p| p[0] == 90 && p[1] == 90 && p[2] == 90)
        .count();
    assert!(grey >= 8, "the 4×2 image is painted ({grey} pixels)");
}
