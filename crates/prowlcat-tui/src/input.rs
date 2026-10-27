//! A single-line text field with a cursor, like the prompt of a shell.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    text: String,
    /// Cursor position in characters, not bytes.
    cursor: usize,
}

impl Input {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn set(&mut self, text: impl Into<String>) {
        *self = Self::new(text);
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Return the text and leave the field empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(i, _)| i)
    }

    fn len_chars(&self) -> usize {
        self.text.chars().count()
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        let at = self.byte_at(self.cursor);
        self.text.remove(at);
    }

    pub fn delete(&mut self) {
        if self.cursor < self.len_chars() {
            let at = self.byte_at(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len_chars());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.len_chars();
    }

    /// Delete everything before the cursor (`ctrl+u`).
    pub fn kill_to_start(&mut self) {
        let at = self.byte_at(self.cursor);
        self.text.drain(..at);
        self.cursor = 0;
    }

    /// Delete the word before the cursor (`ctrl+w`).
    pub fn kill_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let (a, b) = (self.byte_at(start), self.byte_at(self.cursor));
        self.text.drain(a..b);
        self.cursor = start;
    }

    /// The whitespace-delimited word under the cursor, with its character range.
    pub fn word_before_cursor(&self) -> (usize, &str) {
        let before = &self.text[..self.byte_at(self.cursor)];
        let start = before.rfind(char::is_whitespace).map_or(0, |i| i + 1);
        (before[..start].chars().count(), &before[start..])
    }

    /// Replace the word that ends at the cursor.
    pub fn replace_word_before_cursor(&mut self, with: &str) {
        let (start_chars, word) = self.word_before_cursor();
        let word_len = word.chars().count();
        let (a, b) = (self.byte_at(start_chars), self.byte_at(start_chars + word_len));
        self.text.replace_range(a..b, with);
        self.cursor = start_chars + with.chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_editing() {
        let mut i = Input::default();
        for c in "acme.corp".chars() {
            i.insert(c);
        }
        assert_eq!(i.text(), "acme.corp");
        i.home();
        i.delete();
        assert_eq!(i.text(), "cme.corp");
        i.end();
        i.backspace();
        assert_eq!(i.text(), "cme.cor");
        i.left();
        i.insert('!');
        assert_eq!(i.text(), "cme.co!r");
    }

    #[test]
    fn cursor_counts_characters_not_bytes() {
        let mut i = Input::new("héllo");
        assert_eq!(i.cursor(), 5);
        i.home();
        i.right();
        i.right();
        i.insert('X');
        assert_eq!(i.text(), "héXllo");
        i.backspace();
        i.backspace();
        assert_eq!(i.text(), "hllo");
    }

    #[test]
    fn kill_word_and_line() {
        let mut i = Input::new("one two  three");
        i.kill_word();
        assert_eq!(i.text(), "one two  ");
        i.kill_word();
        assert_eq!(i.text(), "one ");
        let mut j = Input::new("hello world");
        j.left();
        j.left();
        j.kill_to_start();
        assert_eq!(j.text(), "ld");
        assert_eq!(j.cursor(), 0);
    }

    #[test]
    fn replace_word_before_cursor_for_completion() {
        let mut i = Input::new("acme.corp /pa");
        assert_eq!(i.word_before_cursor(), (10, "/pa"));
        i.replace_word_before_cursor("/path");
        assert_eq!(i.text(), "acme.corp /path");
        assert_eq!(i.cursor(), 15);
    }

    #[test]
    fn take_empties_the_field() {
        let mut i = Input::new("x");
        assert_eq!(i.take(), "x");
        assert!(i.is_empty());
        assert_eq!(i.cursor(), 0);
    }

    #[test]
    fn pasted_control_characters_are_dropped() {
        let mut i = Input::default();
        i.insert_str("a\nb\tc");
        assert_eq!(i.text(), "abc");
    }
}
