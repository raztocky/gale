use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
fn main() {
    for path in ["Cargo.lock", "src"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let reference = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    for name in [Some("HEAD"), Some("index"), reference.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Ok(o) = Command::new("git")
            .args(["rev-parse", "--git-path", name])
            .output()
        {
            if o.status.success() {
                println!(
                    "cargo:rerun-if-changed={}",
                    String::from_utf8_lossy(&o.stdout).trim()
                );
            }
        }
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let time = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        });
    println!("cargo:rustc-env=BRICKLE_BUILD_TIME={time}");
    let rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or("unknown".into());
    let dirty = Command::new("git")
        .args(["--no-optional-locks", "status", "--porcelain"])
        .output()
        .ok()
        .is_none_or(|o| !o.status.success() || !o.stdout.is_empty());
    println!(
        "cargo:rustc-env=BRICKLE_REVISION={rev}{}",
        if dirty { "+modified" } else { "" }
    );
}
