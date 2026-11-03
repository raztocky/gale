//! End-to-end tests that run the real binary, including the sandboxed
//! renderer process.

use std::process::Command;

fn brickle() -> Command {
    Command::new(env!("CARGO_BIN_EXE_brickle"))
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_blocks_files_network_and_exec() {
    let out = brickle().arg("sandbox-test").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout.contains("ALLOWED"), "{stdout}");
}

#[test]
fn sandboxed_render_matches_in_process_render() {
    let dir = std::env::temp_dir();
    let page = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pages/stress.html");
    let a = dir.join("brickle-sandboxed.png");
    let b = dir.join("brickle-inprocess.png");
    for (png, extra) in [(&a, None), (&b, Some("--no-sandbox"))] {
        let mut cmd = brickle();
        cmd.args(["render", page, "-o", png.to_str().unwrap(), "-w", "800"]);
        if let Some(x) = extra {
            cmd.arg(x);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
}

#[test]
fn hostile_page_does_not_crash_the_browser() {
    let path = std::env::temp_dir().join("brickle-hostile.html");
    let html = format!(
        "<style>{} p {{ font-size: 99999px }}</style>{}<p>x",
        "div ".repeat(31),
        "<div>".repeat(200_000)
    );
    std::fs::write(&path, html).unwrap();
    let png = std::env::temp_dir().join("brickle-hostile.png");
    let out = brickle()
        .args(["render", path.to_str().unwrap(), "-o", png.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
