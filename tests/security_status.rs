use std::process::Command;
#[test]
fn security_status_is_offline_and_reports_unknowns() {
    let dir = std::env::temp_dir().join(format!("brickle-status-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_brickle"))
        .args(["--no-sandbox", "security", "status"])
        .env("XDG_CONFIG_HOME", &dir)
        .env("XDG_DATA_HOME", &dir)
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for label in [
        "Browser build",
        "Dependency lock SHA-256",
        "audit status: unknown",
        "missing",
        "CRLite",
        "No downloads",
    ] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "status must not create profile/update files"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dependency_audit_record_must_match_the_built_lock_and_clock() {
    let file = std::env::temp_dir().join(format!("brickle-audit-{}.json", std::process::id()));
    let record = |hash: &str, checked: u64, db: u64| serde_json::json!({"lock_sha256":hash,"checked_at":checked,"advisory_db_updated_at":db,"vulnerabilities":0});
    std::fs::write(&file, record("wrong-lock", 100, 90).to_string()).unwrap();
    assert!(brickle::security::audit_status(&file, 110).contains("unknown"));
    let hash = brickle::security::lock_hash();
    std::fs::write(&file, record(&hash, 120, 90).to_string()).unwrap();
    assert!(brickle::security::audit_status(&file, 110).contains("unknown"));
    std::fs::write(&file, record(&hash, 100, 90).to_string()).unwrap();
    assert!(brickle::security::audit_status(&file, 110).contains("0 known vulnerabilities"));
    assert!(brickle::security::audit_status(&file, 9 * 86_400).contains("stale"));
    std::fs::remove_file(file).unwrap();
}
