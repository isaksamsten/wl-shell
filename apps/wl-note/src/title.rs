//! The open note's title, edited in place: one line of text and a caret.
//! Pure, unit tested.
//!
//! The usual keys: characters insert, Backspace and Delete remove (a word
//! with Ctrl), the arrows move (by words with Ctrl), Home and End go to
//! the ends. The column maps keys to these; Enter keeps the edit (if it
//! `changed`), Escape drops it.

/// A title being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title {
    pub text: String,
    /// A byte index of `text`, on a char boundary.
    pub caret: usize,
    anchor: usize,
    /// As it was.
    original: String,
}

impl Title {
    /// Edit `text`, the caret at its end.
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            caret: text.len(),
            anchor: text.len(),
            original: text.to_string(),
        }
    }

    /// Whether it differs from what it was.
    pub fn changed(&self) -> bool {
        self.text.trim() != self.original.trim()
    }

    /// Put the caret at byte `at` (or the char boundary before it).
    pub fn set_caret(&mut self, at: usize) {
        self.caret = at.min(self.text.len());
        self.snap();
        self.anchor = self.caret;
    }

    fn snap(&mut self) {
        while !self.text.is_char_boundary(self.caret) {
            self.caret -= 1;
        }
    }

    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        (self.anchor != self.caret)
            .then(|| self.anchor.min(self.caret)..self.anchor.max(self.caret))
    }

    pub fn selected(&self) -> Option<&str> {
        self.selection().map(|r| &self.text[r])
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    pub fn cut(&mut self) -> Option<String> {
        let range = self.selection()?;
        let text = self.text[range.clone()].to_owned();
        self.text.replace_range(range.clone(), "");
        self.set_caret(range.start);
        Some(text)
    }

    pub fn extend_from(&mut self, anchor: usize) {
        self.anchor = anchor;
    }

    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// Type `s` at the caret; line breaks and other control characters
    /// are left out.
    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        if s.is_empty() {
            return;
        }
        self.cut();
        self.text.insert_str(self.caret, &s);
        self.caret += s.len();
        self.anchor = self.caret;
    }

    /// Where the char before `at` starts.
    fn prev(&self, at: usize) -> usize {
        self.text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    /// Where the char at `at` ends.
    fn next(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8())
    }

    /// The start of the word before the caret.
    fn word_back(&self) -> usize {
        let before = &self.text[..self.caret];
        let trimmed = before.trim_end();
        match trimmed.rfind(char::is_whitespace) {
            Some(i) => i + trimmed[i..].chars().next().map_or(1, char::len_utf8),
            None => 0,
        }
    }

    /// The end of the word after the caret.
    fn word_forward(&self) -> usize {
        let after = &self.text[self.caret..];
        let skipped = after.len() - after.trim_start().len();
        let rest = &after[skipped..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.caret + skipped + word
    }

    pub fn left(&mut self, word: bool) {
        self.caret = match word {
            true => self.word_back(),
            false => self.prev(self.caret),
        };
        self.anchor = self.caret;
    }

    pub fn right(&mut self, word: bool) {
        self.caret = match word {
            true => self.word_forward(),
            false => self.next(self.caret),
        };
        self.anchor = self.caret;
    }

    pub fn home(&mut self) {
        self.caret = 0;
        self.anchor = self.caret;
    }

    pub fn end(&mut self) {
        self.caret = self.text.len();
        self.anchor = self.caret;
    }

    /// Remove the char (or word) before the caret.
    pub fn backspace(&mut self, word: bool) {
        if self.cut().is_some() {
            return;
        }
        let from = match word {
            true => self.word_back(),
            false => self.prev(self.caret),
        };
        self.text.replace_range(from..self.caret, "");
        self.caret = from;
        self.anchor = self.caret;
    }

    /// Remove the char (or word) after the caret.
    pub fn delete(&mut self, word: bool) {
        if self.cut().is_some() {
            return;
        }
        let to = match word {
            true => self.word_forward(),
            false => self.next(self.caret),
        };
        self.text.replace_range(self.caret..to, "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_moving() {
        let mut t = Title::new("Groceries");
        assert_eq!(t.caret, 9);
        assert!(!t.changed());
        t.insert(" list\n");
        assert_eq!(t.text, "Groceries list");
        t.home();
        t.insert("My ");
        assert_eq!((t.text.as_str(), t.caret), ("My Groceries list", 3));
        t.right(false);
        t.left(false);
        t.left(false);
        assert_eq!(t.caret, 2);
        t.end();
        t.left(true);
        assert_eq!(t.caret, 13);
        t.left(true);
        assert_eq!(t.caret, 3);
        t.right(true);
        assert_eq!(t.caret, 12);
        t.right(true);
        assert_eq!(t.caret, 17);
        t.right(false);
        assert_eq!(t.caret, 17);
        assert!(t.changed());
        // Only blanks around it: no change.
        let mut t = Title::new("Groceries");
        t.insert("  ");
        assert!(!t.changed());
    }

    #[test]
    fn removing() {
        let mut t = Title::new("one two  three");
        t.backspace(false);
        assert_eq!(t.text, "one two  thre");
        t.backspace(true);
        assert_eq!(t.text, "one two  ");
        t.backspace(true);
        assert_eq!(t.text, "one ");
        t.home();
        t.delete(true);
        assert_eq!(t.text, " ");
        t.delete(false);
        t.delete(false);
        t.backspace(false);
        assert_eq!((t.text.as_str(), t.caret), ("", 0));
    }

    #[test]
    fn multibyte_chars() {
        let mut t = Title::new("Grüße 日本");
        t.left(false);
        assert_eq!(&t.text[t.caret..], "本");
        t.backspace(false);
        assert_eq!(t.text, "Grüße 本");
        t.backspace(false);
        assert_eq!(t.text, "Grüße本");
        t.set_caret(3);
        assert_eq!(t.caret, 2);
        t.delete(false);
        assert_eq!(t.text, "Grße本");
    }
}
