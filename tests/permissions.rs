use brickle::network::Network;
#[test]
fn saved_javascript_permission_is_limited_to_origin() {
    let mut n = Network::empty();
    n.set_scripts_allowed("https://a.example:8443/one", true).unwrap();
    assert!(n.scripts_allowed("https://a.example:8443/two"));
    for other in [
        "https://sub.a.example:8443/",
        "https://a.example/",
        "http://a.example:8443/",
    ] {
        assert!(!n.scripts_allowed(other), "permission leaked to {other}");
    }
}

#[test]
fn temporary_grants_and_denials_never_change_saved_permissions() {
    let mut n = Network::empty();
    n.set_scripts_allowed("https://saved.example/", true).unwrap();
    n.set_scripts_session("https://temporary.example:8443/", true)
        .unwrap();
    assert!(n.scripts_allowed("https://temporary.example:8443/a"));
    assert!(!n.scripts_allowed("https://temporary.example/"));
    assert_eq!(n.js_sites(), vec!["https://saved.example"]);
    n.set_scripts_session("https://saved.example/", false).unwrap();
    assert!(!n.scripts_allowed("https://saved.example/"));
    assert!(n.scripts_persistent("https://saved.example/"));
    assert_eq!(n.script_permission("https://saved.example/"), "Off this session");
    assert!(!Network::empty().scripts_allowed("https://temporary.example:8443/"));
}
