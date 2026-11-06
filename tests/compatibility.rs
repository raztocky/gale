//! Default CI uses only original synthetic pages and a local TLS replay proxy.
#![cfg(target_os = "linux")]
use std::{path::PathBuf, process::Command};
#[test]
fn compatibility_harness_rejects_false_positives() {
    let out = Command::new("python3")
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "tests/compatibility",
            "-p",
            "test_runner.py",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn link_board_forum_and_news_replay_without_external_network() {
    let out = Command::new("python3")
        .args([
            "tests/compatibility/runner.py",
            "--binary",
            env!("CARGO_BIN_EXE_brickle"),
            "--output",
        ])
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/compat-ci"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 and openssl are required");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for case in ["forum: PASS", "links: PASS", "news: PASS"] {
        assert!(stdout.contains(case), "missing case {case}: {stdout}");
    }
}
