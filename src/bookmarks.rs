//! Bookmarks: pages you saved, in `~/.local/share/brickle/bookmarks.txt`,
//! one per line as `address<TAB>title`. Only on your computer; nothing is
//! ever fetched for them (no icons, no previews).
use std::path::{Path, PathBuf};

/// Most bookmarks kept.
pub const MAX_BOOKMARKS: usize = 1000;
/// Longest address kept.
const MAX_URL: usize = 8192;
/// Longest title kept, in characters.
const MAX_TITLE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct Bookmarks {
    pub list: Vec<Bookmark>,
    path: Option<PathBuf>,
}

/// Only addresses Brickle can open, and that don't carry a password.
fn usable(url: &str) -> bool {
    url.len() <= MAX_URL
        && url::Url::parse(url).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https" | "file")
                && u.username().is_empty()
                && u.password().is_none()
        })
}

/// One line of text, without control or invisible characters (so a page's
/// title can't break the file or hide what it says).
fn clean_title(title: &str) -> String {
    let t: String = crate::select::clean(title)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    t.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE)
        .collect()
}

impl Bookmarks {
    /// Your bookmarks, from `dir` (none if there's no file yet).
    pub fn load(dir: Option<&Path>) -> Bookmarks {
        let path = dir.map(|d| d.join("bookmarks.txt"));
        let text = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let mut b = Bookmarks { list: vec![], path };
        for line in text.lines() {
            let (url, title) = line.split_once('\t').unwrap_or((line, ""));
            b.insert(url.trim(), title);
        }
        b
    }

    fn insert(&mut self, url: &str, title: &str) -> bool {
        if self.list.len() >= MAX_BOOKMARKS || !usable(url) || self.contains(url) {
            return false;
        }
        let title = clean_title(title);
        self.list.push(Bookmark {
            url: url.to_string(),
            title,
        });
        true
    }

    pub fn contains(&self, url: &str) -> bool {
        self.list.iter().any(|b| b.url == url)
    }

    /// Bookmark `url`, or take its bookmark away if it has one. Says what
    /// happened.
    pub fn toggle(&mut self, url: &str, title: &str) -> Result<String, String> {
        let msg = if let Some(i) = self.list.iter().position(|b| b.url == url) {
            let b = self.list.remove(i);
            format!("Removed bookmark “{}”", b.shown())
        } else if !usable(url) {
            return Err("Only web pages and local files can be bookmarked".into());
        } else if !self.insert(url, title) {
            return Err(format!("Brickle keeps at most {MAX_BOOKMARKS} bookmarks"));
        } else {
            format!(
                "Bookmarked “{}” (Ctrl+D again to remove it)",
                self.list.last().map(Bookmark::shown).unwrap_or_default()
            )
        };
        self.save()?;
        Ok(msg)
    }

    pub fn remove(&mut self, i: usize) -> Result<String, String> {
        if i >= self.list.len() {
            return Err("No such bookmark".into());
        }
        let b = self.list.remove(i);
        self.save()?;
        Ok(format!("Removed bookmark “{}”", b.shown()))
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        let text: String = self
            .list
            .iter()
            .map(|b| format!("{}\t{}\n", b.url, b.title))
            .collect();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("Couldn't save bookmarks: {e}"))?;
        }
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| format!("Couldn't save bookmarks: {e}"))
    }
}

impl Bookmark {
    /// Its title, or its address if it has none.
    pub fn shown(&self) -> String {
        if self.title.is_empty() {
            let (host, rest) = crate::net::display_parts(&self.url);
            let host = host
                .split_once("://")
                .map_or(host.clone(), |(_, h)| h.to_string());
            if rest == "/" {
                host
            } else {
                format!("{host}{rest}")
            }
        } else {
            self.title.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_and_reload() {
        let dir = std::env::temp_dir().join(format!("brickle-bm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut b = Bookmarks::load(Some(&dir));
        assert!(b.list.is_empty());
        assert!(b
            .toggle("https://a.example/", "A\tsite\nwith\u{200b} junk\u{202e}")
            .is_ok());
        assert!(b.toggle("https://b.example/x", "").is_ok());
        assert_eq!(b.list[0].title, "A site with junk");
        assert_eq!(b.list[1].shown(), "b.example/x");
        // Not bookmarkable.
        for bad in [
            "javascript:alert(1)",
            "about:home",
            "https://u:p@c.example/",
            "data:text/html,x",
        ] {
            assert!(b.toggle(bad, "x").is_err(), "{bad}");
        }
        let again = Bookmarks::load(Some(&dir));
        assert_eq!(again.list, b.list);
        // Toggling takes it away.
        assert!(b
            .toggle("https://a.example/", "A")
            .unwrap()
            .starts_with("Removed"));
        assert!(b.remove(0).is_ok());
        assert!(b.remove(0).is_err());
        assert!(Bookmarks::load(Some(&dir)).list.is_empty());
        // A hand-edited file: bad lines are skipped, duplicates dropped.
        std::fs::write(
            dir.join("bookmarks.txt"),
            "https://a.example/\tA\nnot a url\nhttps://a.example/\tdup\nfile:///tmp/x.html\n",
        )
        .unwrap();
        let edited = Bookmarks::load(Some(&dir));
        assert_eq!(edited.list.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
        // No folder (no HOME): works, saves nothing.
        let mut none = Bookmarks::load(None);
        assert!(none.toggle("https://a.example/", "A").is_ok());
    }
}
