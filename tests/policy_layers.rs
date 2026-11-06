use brickle::{net::Dest, page::Page};
#[test]
fn meta_image_policy_applies_without_javascript() {
    let mut p = Page::parse(
        "https://a.example/",
        "<meta http-equiv='Content-Security-Policy' content=\"img-src 'none'\"><img src='/x.png'>",
        "",
        &mut |_, _| Err("off".into()),
    );
    let mut calls = 0;
    p.load_images(&mut |_, _| {
        calls += 1;
        Err("off".into())
    });
    assert_eq!(calls, 0, "CSP-blocked image must never be fetched");
}
#[test]
fn meta_style_policy_blocks_link_and_inline() {
    let mut calls = 0;
    let p=Page::parse("https://a.example/", "<meta http-equiv='Content-Security-Policy' content=\"style-src 'none'\"><link rel=stylesheet href='/a.css'><style>body {color:red}</style><p style='color:blue'>x", "", &mut |_,d|{assert_eq!(d,Dest::Stylesheet); calls+=1;Ok("p{color:red}".into())});
    assert_eq!(calls, 0, "CSP-blocked stylesheet must never be fetched");
    assert!(p.sheets.is_empty());
    let mut dom = String::new();
    p.dom.dump(0, &mut dom);
    assert!(!dom.contains("color:blue"));
}
#[test]
fn stylesheet_integrity_is_checked_before_application_and_cache_reuse() {
    let p = Page::parse(
        "https://a.example/",
        "<link rel=stylesheet href='/a.css'><link rel=stylesheet href='/a.css' integrity='sha256-AAAA'>",
        "",
        &mut |_, _| Ok("p{color:red}".into()),
    );
    assert_eq!(
        p.sheets.len(),
        1,
        "cached stylesheet cannot bypass integrity on a second link"
    );
}

#[test]
fn source_paths_keep_their_case() {
    let mut c = brickle::csp::Csp::new("https://a.example/");
    c.add("style-src https://a.example/Case/");
    assert!(c.resource(Dest::Stylesheet, "https://a.example/Case/a.css"));
    assert!(!c.resource(Dest::Stylesheet, "https://a.example/case/a.css"));
}

#[test]
fn nonces_data_images_and_separate_style_directives_have_controls() {
    let mut c = brickle::csp::Csp::new("https://a.example/");
    c.add("default-src 'none'; img-src data:; style-src 'nonce-abc'; style-src-attr 'unsafe-inline'");
    assert!(c.resource(Dest::Image, "data:image/svg+xml,x"));
    assert!(!c.resource(Dest::Image, "https://a.example/x.png"));
    assert!(c.style_inline(Some("abc"), "p{color:red}", false));
    assert!(!c.style_inline(Some("wrong"), "p{color:red}", false));
    assert!(c.style_inline(None, "color:red", true));
    assert!(c.style_link(Some("abc"), "https://cdn.example/a.css"));
    assert!(c.style_sources(Some("abc")).is_empty());
    c.add("img-src 'none'; style-src 'none'");
    assert!(!c.resource(Dest::Image, "data:image/svg+xml,x"));
    assert!(!c.style_inline(Some("abc"), "p{color:red}", false));
}

#[test]
fn stylesheet_metadata_round_trips_across_ipc() {
    use brickle::ipc::FromRenderer;
    let request = FromRenderer::Stylesheet {
        url: "https://a.example/a.css".into(),
        integrity: "sha256-AAAA".into(),
        csp: vec![vec!["'self'".into()]],
        added: true,
        nonce: Some("random".into()),
    };
    assert_eq!(FromRenderer::decode(&request.encode()).unwrap(), request);
}

#[test]
fn too_many_csp_policies_fail_closed() {
    let mut c = brickle::csp::Csp::new("https://a.example/");
    c.add(&format!("{}img-src 'none'", "img-src *,".repeat(16)));
    assert!(!c.resource(Dest::Image, "https://a.example/image.png"));
}
#[test]
fn oversized_csp_sources_cannot_reactivate_unsafe_inline() {
    let mut c = brickle::csp::Csp::new("https://a.example/");
    c.add(&format!("style-src 'unsafe-inline' 'nonce-{}'", "x".repeat(3000)));
    assert!(!c.style_inline(None, "body{color:red}", false));
}
#[test]
fn too_many_csp_directives_fail_closed() {
    let mut c = brickle::csp::Csp::new("https://a.example/");
    c.add(&format!("{}img-src 'none'", "unused;".repeat(64)));
    assert!(!c.resource(Dest::Image, "https://a.example/image.png"));
}
