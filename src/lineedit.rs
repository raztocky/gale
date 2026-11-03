//! A one-line text field's contents, cursor and selection, for the address
//! bar. Positions count characters, not bytes.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineEdit {
    pub text: String,
    /// Where the cursor is, from 0 to the number of characters.
    pub cursor: usize,
    /// The other end of the selection, if any.
    pub anchor: Option<usize>,
    /// Longest the text may get, in bytes.
    pub max_len: usize,
}

impl LineEdit {
    pub fn new(max_len: usize) -> LineEdit {
        LineEdit {
            max_len,
            ..LineEdit::default()
        }
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte(&self, i: usize) -> usize {
        self.text
            .char_indices()
            .nth(i)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Replace the text, with the cursor at the end and nothing selected.
    pub fn set(&mut self, text: &str) {
        self.text = text.chars().take(self.max_len).collect();
        while self.text.len() > self.max_len {
            self.text.pop();
        }
        self.cursor = self.len();
        self.anchor = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.len();
    }

    /// The selection as (start, end), if it isn't empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected(&self) -> Option<&str> {
        let (a, b) = self.selection()?;
        Some(&self.text[self.byte(a)..self.byte(b)])
    }

    /// Delete the selection; true if there was one.
    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let (ba, bb) = (self.byte(a), self.byte(b));
        self.text.replace_range(ba..bb, "");
        self.cursor = a;
        self.anchor = None;
        true
    }

    /// Type `s` at the cursor, replacing the selection. Stops at `max_len`.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let mut at = self.byte(self.cursor);
        for c in s.chars() {
            if self.text.len() + c.len_utf8() > self.max_len {
                break;
            }
            self.text.insert(at, c);
            at += c.len_utf8();
            self.cursor += 1;
        }
    }

    /// Cut the selection out, returning it.
    pub fn cut(&mut self) -> Option<String> {
        let s = self.selected()?.to_string();
        self.delete_selection();
        Some(s)
    }

    pub fn backspace(&mut self, word: bool) {
        if !self.delete_selection() && self.cursor > 0 {
            let from = if word {
                self.word_start(self.cursor)
            } else {
                self.cursor - 1
            };
            self.anchor = Some(from);
            self.delete_selection();
        }
    }

    pub fn delete(&mut self, word: bool) {
        if !self.delete_selection() && self.cursor < self.len() {
            let to = if word {
                self.word_end(self.cursor)
            } else {
                self.cursor + 1
            };
            self.anchor = Some(to);
            self.delete_selection();
        }
    }

    /// Move the cursor to `to`; with `extend`, the selection follows it.
    pub fn move_to(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = to.min(self.len());
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        // Without shift, a selection collapses to its start.
        let to = match (self.selection(), extend) {
            (Some((a, _)), false) if !word => a,
            _ if word => self.word_start(self.cursor),
            _ => self.cursor.saturating_sub(1),
        };
        self.move_to(to, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        let to = match (self.selection(), extend) {
            (Some((_, b)), false) if !word => b,
            _ if word => self.word_end(self.cursor),
            _ => self.cursor + 1,
        };
        self.move_to(to, extend);
    }

    /// Word boundaries: runs of letters and digits.
    fn is_word(c: char) -> bool {
        c.is_alphanumeric()
    }

    fn word_start(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from.min(chars.len());
        while i > 0 && !Self::is_word(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && Self::is_word(chars[i - 1]) {
            i -= 1;
        }
        i
    }

    fn word_end(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from.min(chars.len());
        while i < chars.len() && !Self::is_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && Self::is_word(chars[i]) {
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str) -> LineEdit {
        let mut e = LineEdit::new(64);
        e.set(text);
        e
    }

    #[test]
    fn typing_moving_and_deleting() {
        let mut e = edit("exmple.com");
        e.move_to(2, false);
        e.insert("a");
        assert_eq!((e.text.as_str(), e.cursor), ("example.com", 3));
        e.backspace(false);
        e.delete(false);
        assert_eq!(e.text, "exple.com");
        e.left(false, false);
        e.left(false, false);
        e.left(false, false);
        assert_eq!(e.cursor, 0);
        e.right(true, false);
        assert_eq!(e.cursor, 5, "a word to the right");
    }

    #[test]
    fn selecting_replaces_and_cuts() {
        let mut e = edit("https://example.com/path");
        e.select_all();
        assert_eq!(e.selected(), Some("https://example.com/path"));
        e.insert("cats");
        assert_eq!((e.text.as_str(), e.selection()), ("cats", None));
        let mut e = edit("hello world");
        e.move_to(0, false);
        e.right(true, true);
        assert_eq!(e.selected(), Some("hello"));
        assert_eq!(e.cut().as_deref(), Some("hello"));
        assert_eq!(e.text, " world");
        // Left without shift collapses a selection to its start.
        e.select_all();
        e.left(false, false);
        assert_eq!((e.cursor, e.selection()), (0, None));
    }

    #[test]
    fn word_deletes() {
        let mut e = edit("example.com/some/path");
        e.backspace(true);
        assert_eq!(e.text, "example.com/some/");
        e.move_to(0, false);
        e.delete(true);
        assert_eq!(e.text, ".com/some/");
    }

    #[test]
    fn unicode_and_the_length_cap() {
        let mut e = edit("héllo");
        e.move_to(2, false);
        e.backspace(false);
        assert_eq!(e.text, "hllo");
        let mut e = LineEdit::new(8);
        e.insert("ééééé");
        assert!(e.text.len() <= 8 && e.text.chars().all(|c| c == 'é'));
        e.set("0123456789");
        assert_eq!(e.text, "01234567");
    }
}
