//! Bookmarks: pages you saved, in `~/.local/share/brickle/bookmarks.txt`,
//! one per line as `address<TAB>title<TAB>folder` (folder left out for the
//! toolbar itself), in the order shown. Only on your computer; nothing is
//! ever fetched for them (no icons, no previews). Folders are one level
//! deep, and exist while they have bookmarks in them.
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
    /// The folder it's in; empty for the toolbar itself.
    pub folder: String,
}

/// One place on the bookmarks toolbar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// A bookmark, by index in the list.
    Mark(usize),
    /// A folder, by name (where its first bookmark is).
    Folder(String),
}

/// Longest folder name kept, in characters.
const MAX_FOLDER: usize = 60;

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

/// A folder name: one line, no tabs, like a title, and shorter.
fn clean_folder(name: &str) -> String {
    clean_title(name).chars().take(MAX_FOLDER).collect()
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
            let mut parts = line.splitn(3, '\t');
            let url = parts.next().unwrap_or_default();
            let title = parts.next().unwrap_or_default();
            let folder = parts.next().unwrap_or_default();
            if b.insert(url.trim(), title) {
                if let Some(last) = b.list.last_mut() {
                    last.folder = clean_folder(folder);
                }
            }
        }
        b.group();
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
            folder: String::new(),
        });
        true
    }

    /// Each folder's bookmarks together, where its first one is.
    fn group(&mut self) {
        let slots = self.toolbar();
        self.list = self.in_order(&slots);
    }

    /// The list, slot by slot (a folder's bookmarks in their order).
    fn in_order(&self, slots: &[Slot]) -> Vec<Bookmark> {
        let mut out = Vec::with_capacity(self.list.len());
        for slot in slots {
            match slot {
                Slot::Mark(i) => out.push(self.list[*i].clone()),
                Slot::Folder(f) => out.extend(self.list.iter().filter(|b| &b.folder == f).cloned()),
            }
        }
        out
    }

    /// What the toolbar shows, left to right: bookmarks outside folders, and
    /// each folder where its first bookmark is.
    pub fn toolbar(&self) -> Vec<Slot> {
        let mut out = Vec::new();
        for (i, b) in self.list.iter().enumerate() {
            if b.folder.is_empty() {
                out.push(Slot::Mark(i));
            } else if !out.iter().any(|s| matches!(s, Slot::Folder(f) if *f == b.folder)) {
                out.push(Slot::Folder(b.folder.clone()));
            }
        }
        out
    }

    /// The bookmarks in `folder`, by index, in order.
    pub fn in_folder(&self, folder: &str) -> Vec<usize> {
        (0..self.list.len())
            .filter(|&i| self.list[i].folder == folder)
            .collect()
    }

    /// Every folder, in toolbar order.
    pub fn folders(&self) -> Vec<String> {
        self.toolbar()
            .into_iter()
            .filter_map(|s| match s {
                Slot::Folder(f) => Some(f),
                Slot::Mark(_) => None,
            })
            .collect()
    }

    /// Give bookmark `i` a new name (empty: show its address).
    pub fn rename(&mut self, i: usize, title: &str) -> Result<String, String> {
        let b = self.list.get_mut(i).ok_or("No such bookmark")?;
        b.title = clean_title(title);
        let shown = b.shown();
        self.save()?;
        Ok(format!("Renamed the bookmark to “{shown}”"))
    }

    /// Put bookmark `i` in `folder` (empty: on the toolbar itself), after
    /// what's there already.
    pub fn set_folder(&mut self, i: usize, folder: &str) -> Result<String, String> {
        if i >= self.list.len() {
            return Err("No such bookmark".into());
        }
        let folder = clean_folder(folder);
        let mut b = self.list.remove(i);
        b.folder = folder.clone();
        let at = self
            .list
            .iter()
            .rposition(|o| !folder.is_empty() && o.folder == folder)
            .map_or(self.list.len(), |p| p + 1);
        self.list.insert(at, b);
        self.group();
        self.save()?;
        Ok(if folder.is_empty() {
            "Moved the bookmark to the toolbar".into()
        } else {
            format!("Moved the bookmark to “{folder}”")
        })
    }

    /// Rename a folder (to one that exists: the two become one).
    pub fn rename_folder(&mut self, old: &str, new: &str) -> Result<String, String> {
        let new = clean_folder(new);
        if new.is_empty() {
            return Err("A folder needs a name".into());
        }
        for b in self.list.iter_mut().filter(|b| b.folder == old) {
            b.folder = new.clone();
        }
        self.group();
        self.save()?;
        Ok(format!("Renamed the folder to “{new}”"))
    }

    /// Take a folder away, leaving its bookmarks on the toolbar where it was.
    pub fn remove_folder(&mut self, folder: &str) -> Result<String, String> {
        for b in self.list.iter_mut().filter(|b| b.folder == folder) {
            b.folder.clear();
        }
        self.save()?;
        Ok(format!(
            "Removed the folder “{folder}” (its bookmarks are on the toolbar)"
        ))
    }

    /// Move toolbar slot `from` to where slot `to` is, a folder with all
    /// its bookmarks.
    pub fn move_slot(&mut self, from: usize, to: usize) -> Result<(), String> {
        let mut slots = self.toolbar();
        if from >= slots.len() || to >= slots.len() || from == to {
            return Ok(());
        }
        let s = slots.remove(from);
        slots.insert(to, s);
        self.list = self.in_order(&slots);
        self.save()
    }

    /// Move bookmark `i` within its folder, to where bookmark `j` is.
    pub fn move_within(&mut self, i: usize, j: usize) -> Result<(), String> {
        if i >= self.list.len()
            || j >= self.list.len()
            || i == j
            || self.list[i].folder != self.list[j].folder
        {
            return Ok(());
        }
        let b = self.list.remove(i);
        self.list.insert(j, b);
        self.save()
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
            .map(|b| {
                if b.folder.is_empty() {
                    format!("{}\t{}\n", b.url, b.title)
                } else {
                    format!("{}\t{}\t{}\n", b.url, b.title, b.folder)
                }
            })
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
        // Folders: kept together, saved, renamed, taken away.
        let mut f = Bookmarks::load(Some(&dir));
        for u in [
            "https://a.example/",
            "https://b.example/",
            "https://c.example/",
            "https://d.example/",
        ] {
            f.toggle(u, "").unwrap();
        }
        f.set_folder(3, "News\tstuff").unwrap();
        f.set_folder(0, "News stuff").unwrap();
        let shown = |b: &Bookmarks| -> Vec<String> {
            b.toolbar()
                .iter()
                .map(|s| match s {
                    Slot::Mark(i) => b.list[*i].shown(),
                    Slot::Folder(f) => format!(
                        "[{f}: {}]",
                        b.in_folder(f)
                            .iter()
                            .map(|&i| b.list[i].shown())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                })
                .collect()
        };
        assert_eq!(
            shown(&f),
            ["b.example", "c.example", "[News stuff: d.example, a.example]"]
        );
        assert_eq!(shown(&Bookmarks::load(Some(&dir))), shown(&f), "saved");
        f.move_slot(2, 0).unwrap();
        assert_eq!(
            shown(&f),
            ["[News stuff: d.example, a.example]", "b.example", "c.example"]
        );
        let a = f.list.iter().position(|b| b.url == "https://a.example/").unwrap();
        f.move_within(a, 0).unwrap();
        f.rename(0, "  Alpha\n").unwrap();
        assert_eq!(
            shown(&f),
            ["[News stuff: Alpha, d.example]", "b.example", "c.example"]
        );
        f.rename_folder("News stuff", "Daily").unwrap();
        assert_eq!(f.folders(), ["Daily"]);
        f.remove_folder("Daily").unwrap();
        assert_eq!(shown(&f), ["Alpha", "d.example", "b.example", "c.example"]);
        assert_eq!(Bookmarks::load(Some(&dir)).list, f.list);
        let _ = std::fs::remove_dir_all(&dir);
        // No folder (no HOME): works, saves nothing.
        let mut none = Bookmarks::load(None);
        assert!(none.toggle("https://a.example/", "A").is_ok());
    }
}
