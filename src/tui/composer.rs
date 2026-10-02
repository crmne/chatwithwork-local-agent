//! What's typed in the chat composer: text with a cursor, over several
//! lines, and the questions asked before.
//!
//! Keys move and edit the way a shell's line editor does (Ctrl-A, Ctrl-E,
//! Ctrl-K, Ctrl-U, Ctrl-W), and Up and Down recall earlier questions from
//! the first and last line.

use std::fmt;
use std::ops::Deref;

/// Questions remembered, newest last.
pub const HISTORY_KEEP: usize = 500;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Composer {
    text: String,
    /// A byte offset into `text`, always on a character boundary.
    cursor: usize,
}

impl fmt::Debug for Composer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, f)
    }
}

impl Deref for Composer {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl PartialEq<&str> for Composer {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl Composer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the text, with the cursor at its end.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.set(String::new());
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    /// To the start of the line the cursor is on.
    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Ctrl-U: delete from the start of the line to the cursor.
    pub fn kill_before(&mut self) {
        let start = self.line_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Ctrl-K: delete from the cursor to the end of the line.
    pub fn kill_after(&mut self) {
        let end = self.line_end();
        self.text.replace_range(self.cursor..end, "");
    }

    /// Ctrl-W: delete the word before the cursor, and the spaces after it.
    pub fn delete_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches([' ', '/', '\\']);
        let start = trimmed.rfind([' ', '/', '\\', '\n']).map_or(0, |i| i + 1);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Up a line, keeping the column where it can. False on the first line.
    pub fn up(&mut self) -> bool {
        let start = self.line_start();
        if start == 0 {
            return false;
        }
        let column = self.text[start..self.cursor].chars().count();
        let prev_start = self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = column_offset(&self.text, prev_start, start - 1, column);
        true
    }

    /// Down a line. False on the last line.
    pub fn down(&mut self) -> bool {
        let end = self.line_end();
        if end == self.text.len() {
            return false;
        }
        let column = self.text[self.line_start()..self.cursor].chars().count();
        let next_start = end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map_or(self.text.len(), |i| next_start + i);
        self.cursor = column_offset(&self.text, next_start, next_end, column);
        true
    }

    pub fn on_first_line(&self) -> bool {
        self.line_start() == 0
    }

    pub fn on_last_line(&self) -> bool {
        self.line_end() == self.text.len()
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        let c = self.text[self.cursor..].chars().next()?;
        Some(self.cursor + c.len_utf8())
    }
}

/// The byte offset of `column` characters into the line `start..end`, or
/// its end.
fn column_offset(text: &str, start: usize, end: usize, column: usize) -> usize {
    text[start..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(i, _)| start + i)
}

/// Questions asked before, oldest first, and where Up and Down are in them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    entries: Vec<String>,
    /// The entry shown, while going through them.
    at: Option<usize>,
    /// What was typed before going back.
    draft: String,
}

impl History {
    pub fn new(mut entries: Vec<String>) -> Self {
        if entries.len() > HISTORY_KEEP {
            entries.drain(..entries.len() - HISTORY_KEEP);
        }
        Self {
            entries,
            at: None,
            draft: String::new(),
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Remember a question. False when it repeats the last one, which
    /// isn't kept twice.
    pub fn push(&mut self, entry: &str) -> bool {
        self.at = None;
        if entry.trim().is_empty() || self.entries.last().is_some_and(|e| e == entry) {
            return false;
        }
        self.entries.push(entry.to_string());
        if self.entries.len() > HISTORY_KEEP {
            self.entries.remove(0);
        }
        true
    }

    /// The question before the one shown, keeping `current` to come back to.
    pub fn back(&mut self, current: &str) -> Option<&str> {
        let at = match self.at {
            None if self.entries.is_empty() => return None,
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(at) => at - 1,
        };
        self.at = Some(at);
        Some(&self.entries[at])
    }

    /// The question after the one shown, or what was typed before.
    pub fn forward(&mut self) -> Option<String> {
        let at = self.at?;
        if at + 1 < self.entries.len() {
            self.at = Some(at + 1);
            Some(self.entries[at + 1].clone())
        } else {
            self.at = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    /// Typing leaves the history where it is.
    pub fn reset(&mut self) {
        self.at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Composer {
        let mut c = Composer::default();
        c.set(text);
        c
    }

    #[test]
    fn edits_at_the_cursor_across_lines() {
        let mut c = typed("héllo\nworld");
        assert!(c.up());
        assert_eq!(
            &c.text()[c.cursor()..],
            "\nworld",
            "same column, line above"
        );
        c.home();
        c.insert('¡');
        assert_eq!(c.text(), "¡héllo\nworld");
        c.right();
        c.right();
        c.kill_after();
        assert_eq!(c.text(), "¡hé\nworld");
        assert!(c.down());
        assert!(!c.down());
        c.end();
        c.delete_word();
        assert_eq!(c.text(), "¡hé\n");
        c.backspace();
        c.backspace();
        assert_eq!(c.text(), "¡h");
        c.left();
        c.delete();
        assert_eq!(c.text(), "¡");
        c.backspace();
        c.delete();
    }

    #[test]
    fn ctrl_u_clears_the_line_before_the_cursor() {
        let mut c = typed("one\ntwo three");
        c.left();
        c.kill_before();
        assert_eq!(c.text(), "one\ne");
        assert!(c.on_last_line() && !c.on_first_line());
    }

    #[test]
    fn history_goes_back_and_returns_to_the_draft() {
        let mut h = History::new(vec!["first".into(), "second".into()]);
        assert!(!h.push("second"), "no repeats");
        assert_eq!(h.back("draft"), Some("second"));
        assert_eq!(h.back("ignored"), Some("first"));
        assert_eq!(h.back("ignored"), None, "nothing older");
        assert_eq!(h.forward().as_deref(), Some("second"));
        assert_eq!(h.forward().as_deref(), Some("draft"));
        assert_eq!(h.forward(), None);
        assert!(h.push("third"));
        assert_eq!(h.entries().len(), 3);
    }

    #[test]
    fn history_keeps_the_newest() {
        let many: Vec<String> = (0..HISTORY_KEEP + 5).map(|i| i.to_string()).collect();
        let mut h = History::new(many);
        assert_eq!(h.entries().len(), HISTORY_KEEP);
        assert_eq!(h.entries()[0], "5");
        h.push("new");
        assert_eq!(h.entries().len(), HISTORY_KEEP);
        assert_eq!(h.entries().last().map(String::as_str), Some("new"));
    }
}
