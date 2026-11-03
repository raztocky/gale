//! Filter-list downloads (`brickle filters update`).
//!
//! Lists are never fetched automatically: only when you run the command,
//! and every request it makes is printed.

use crate::filter;
use crate::net::{self, Dest};
use crate::network::data_dir;
use std::path::PathBuf;

pub struct List {
    pub file: &'static str,
    pub name: &'static str,
    /// Official location first, then uBlock Origin's GitHub mirror.
    pub urls: &'static [&'static str],
}

pub const LISTS: &[List] = &[
    List {
        file: "easylist.txt",
        name: "EasyList (ads)",
        urls: &[
            "https://easylist.to/easylist/easylist.txt",
            "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/thirdparties/easylist/easylist.txt",
        ],
    },
    List {
        file: "easyprivacy.txt",
        name: "EasyPrivacy (trackers)",
        urls: &[
            "https://easylist.to/easylist/easyprivacy.txt",
            "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/thirdparties/easylist/easyprivacy.txt",
        ],
    },
    List {
        file: "ublock-filters.txt",
        name: "uBlock filters",
        urls: &[
            "https://ublockorigin.github.io/uAssets/filters/filters.txt",
            "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters.txt",
        ],
    },
    List {
        file: "ublock-privacy.txt",
        name: "uBlock filters – Privacy",
        urls: &[
            "https://ublockorigin.github.io/uAssets/filters/privacy.txt",
            "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/privacy.txt",
        ],
    },
    List {
        file: "ublock-unbreak.txt",
        name: "uBlock filters – Unbreak",
        urls: &[
            "https://ublockorigin.github.io/uAssets/filters/unbreak.txt",
            "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/unbreak.txt",
        ],
    },
];

pub fn filters_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("filters"))
}

/// GET with redirects (https only), printing each request.
fn download(url: &str, log: &mut dyn FnMut(&str)) -> Result<String, String> {
    let mut url = url.to_string();
    for _ in 0..5 {
        if net::scheme(&url) != "https" {
            log(&format!("  refused  {url} (not https)"));
            return Err("refusing non-https download".into());
        }
        let r = net::http_get(&url, Dest::Download, None, true);
        match r {
            Ok(resp) if (300..400).contains(&resp.status) => {
                let next = net::resolve(&url, resp.location.as_deref().unwrap_or(""));
                log(&format!("  GET {} -> {} redirect to {next}", url, resp.status));
                url = next;
            }
            Ok(resp) if resp.status == 200 => {
                log(&format!("  GET {url} -> 200, {} KB", resp.body.len() / 1024));
                return Ok(resp.body);
            }
            Ok(resp) => {
                log(&format!("  GET {url} -> {}", resp.status));
                return Err(format!("HTTP {}", resp.status));
            }
            Err(e) => {
                log(&format!("  GET {url} -> failed: {e}"));
                return Err(e);
            }
        }
    }
    Err("too many redirects".into())
}

/// Download one list (trying each mirror) and inline its active `!#include`s.
fn fetch_list(list: &List, log: &mut dyn FnMut(&str)) -> Result<String, String> {
    let mut last_err = String::new();
    for base in list.urls {
        let text = match download(base, log) {
            Ok(t) => t,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        if !text.contains('\n') || text.trim_start().starts_with('<') {
            last_err = "not a filter list".into();
            continue;
        }
        let mut out = String::with_capacity(text.len());
        let mut cond: Vec<bool> = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            if let Some(e) = t.strip_prefix("!#if ") {
                cond.push(filter::eval_condition(e.trim()));
            } else if t.starts_with("!#else") {
                if let Some(c) = cond.last_mut() {
                    *c = !*c;
                }
            } else if t.starts_with("!#endif") {
                cond.pop();
            }
            match t.strip_prefix("!#include ") {
                // Includes must stay next to the list (no other hosts).
                Some(inc) if cond.iter().all(|c| *c) && !inc.contains("://") && !inc.contains("..") => {
                    let inc_url = net::resolve(base, inc.trim());
                    match download(&inc_url, log) {
                        Ok(body) => {
                            out.push_str(&format!("! ---- included from {inc_url}\n"));
                            out.push_str(&body);
                            out.push('\n');
                        }
                        Err(e) => log(&format!("  (skipped include {inc}: {e})")),
                    }
                }
                _ => {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        return Ok(out);
    }
    Err(last_err)
}

/// `brickle filters update`.
pub fn update(log: &mut dyn FnMut(&str)) -> Result<(), String> {
    let dir = filters_dir().ok_or("can't find a data directory (is $HOME set?)")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut failed = 0;
    for list in LISTS {
        log(&format!("{}:", list.name));
        match fetch_list(list, log) {
            Ok(text) => {
                let tmp = dir.join(format!("{}.tmp", list.file));
                let dst = dir.join(list.file);
                std::fs::write(&tmp, &text)
                    .and_then(|_| std::fs::rename(&tmp, &dst))
                    .map_err(|e| format!("{}: {e}", dst.display()))?;
                let f = filter::Filters::parse(&text);
                log(&format!(
                    "  saved {} ({} network rules, {} hiding rules, {} unsupported skipped)",
                    dst.display(),
                    f.stats.network,
                    f.stats.cosmetic,
                    f.stats.skipped
                ));
            }
            Err(e) => {
                failed += 1;
                log(&format!("  FAILED: {e} (keeping the previous copy, if any)"));
            }
        }
    }
    if failed > 0 {
        Err(format!("{failed} list(s) failed to update"))
    } else {
        Ok(())
    }
}

/// `brickle filters status`.
pub fn status() -> Vec<String> {
    let Some(dir) = filters_dir() else {
        return vec!["no data directory".into()];
    };
    let mut out = vec![format!("filter lists in {}:", dir.display())];
    for list in LISTS {
        let path = dir.join(list.file);
        match std::fs::metadata(&path) {
            Ok(m) => {
                let age = m
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(|d| format!("{} days old", d.as_secs() / 86_400))
                    .unwrap_or_default();
                out.push(format!("  {:<28} {:>6} KB  {age}", list.name, m.len() / 1024));
            }
            Err(_) => out.push(format!("  {:<28} missing", list.name)),
        }
    }
    out
}
