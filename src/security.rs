//! Local inventory and age reporting. Never checks a server or downloads updates.
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn age(timestamp: Option<u64>, now: u64, max_days: u64) -> String {
    match timestamp.and_then(|t| now.checked_sub(t)) {
        None => "unknown (missing timestamp or clock mismatch)".into(),
        Some(seconds) => {
            let days = seconds / 86_400;
            format!(
                "{days} days old{}",
                if seconds > max_days * 86_400 {
                    "; stale"
                } else {
                    ""
                }
            )
        }
    }
}
pub fn lock_hash() -> String {
    ring::digest::digest(&ring::digest::SHA256, include_bytes!("../Cargo.lock"))
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn modified(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

pub fn audit_status(path: &Path, now: u64) -> String {
    let record = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let parsed = record.as_ref().and_then(|v| {
        Some((
            v.get("lock_sha256")?.as_str()?,
            v.get("checked_at")?.as_u64()?,
            v.get("advisory_db_updated_at")?.as_u64()?,
            v.get("vulnerabilities")?.as_u64()?,
        ))
    });
    match parsed {
        Some((hash, checked, db, count)) if hash == lock_hash() && checked <= now && db <= checked => {
            format!(
                "local audit record: {count} known vulnerabilities; checked {}; advisory database {}",
                age(Some(checked), now, 7),
                age(Some(db), now, 7)
            )
        }
        _ => "audit status: unknown (no valid local audit record for this dependency lock)".into(),
    }
}

pub fn report() -> Vec<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut out = vec![
        format!(
            "Browser build: {} revision {}; {}",
            env!("CARGO_PKG_VERSION"),
            env!("BRICKLE_REVISION"),
            age(env!("BRICKLE_BUILD_TIME").parse().ok(), now, 30)
        ),
        "Browser/dependency latest-version status: unknown; build age does not establish security freshness"
            .into(),
        format!("Dependency lock SHA-256: {}", lock_hash()),
    ];
    out.push(
        crate::network::data_dir()
            .map(|d| audit_status(&d.join("security-audit.json"), now))
            .unwrap_or("audit status: unknown (no data directory)".into()),
    );
    for list in crate::lists::LISTS {
        let path = crate::lists::filters_dir().map(|d| d.join(list.file));
        let state = path
            .as_ref()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0))
            .map(|p| age(modified(p), now, 7))
            .unwrap_or("missing or empty".into());
        out.push(format!("Filter {}: {state} (local file age)", list.name));
    }
    out.push(if crate::crlite::available() {
        crate::crlite::status()
    } else {
        "CRLite: unavailable (missing or invalid revocation data)".into()
    });
    out.push("Saved exceptions (persist after closing):".into());
    let n = crate::network::Network::from_disk();
    for origin in n.js_sites() {
        out.push(format!("  JavaScript: {origin}"));
    }
    for site in n.cookies.kept() {
        out.push(format!("  Keep cookies: {site}"));
    }
    out.push("No downloads or online checks performed. Update explicitly with `brickle filters update`; update the browser/dependencies through your normal build/release process.".into());
    out
}
pub fn brief() -> &'static str {
    static STATUS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    STATUS.get_or_init(|| {
        let report = report();
        let issues=report.iter().filter(|l|l.starts_with("Filter ") && (l.contains("stale") || l.contains("missing") || l.contains("unknown"))).count();
        let revocation=report.iter().find(|l|l.contains("CRLite")).map_or("CRLite: unknown",String::as_str);
        let audit=report.iter().find(|l|l.starts_with("audit status:") || l.starts_with("local audit record:")).map_or("audit status: unknown",String::as_str);
        format!("Security data (startup snapshot): {issues} filter lists stale/missing/unknown; {revocation}; {audit}. Run `brickle security status` for local ages.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps_never_make_missing_or_future_data_fresh() {
        assert!(age(None, 1, 7).contains("unknown"));
        assert!(age(Some(2), 1, 7).contains("unknown"));
        assert!(age(Some(1), 9 * 86_400, 7).contains("stale"));
        assert!(!age(Some(1), 2, 7).contains("stale"));
    }
}
