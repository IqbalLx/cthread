//! Minimal single- and multi-line text input used by the modals.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Default)]
pub struct TextArea {
    pub text: String,
    /// Byte offset of the cursor, always on a grapheme boundary.
    pub cursor: usize,
    pub multiline: bool,
}

impl TextArea {
    pub fn single(text: &str) -> Self {
        Self { text: text.to_string(), cursor: text.len(), multiline: false }
    }

    pub fn multi(text: &str) -> Self {
        Self { text: text.to_string(), cursor: text.len(), multiline: true }
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| self.cursor + i)
            .unwrap_or(self.text.len())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map(|i| self.cursor + i).unwrap_or(self.text.len())
    }

    pub fn insert_str(&mut self, s: &str) {
        let s: String = if self.multiline { s.replace("\r\n", "\n").replace('\r', "\n") } else { s.replace(['\r', '\n'], " ") };
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    /// Handle a key; returns true if it was consumed.
    pub fn handle(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('u') if ctrl => {
                let s = self.line_start();
                self.text.replace_range(s..self.cursor, "");
                self.cursor = s;
            }
            KeyCode::Char('w') if ctrl => {
                let before = &self.text[..self.cursor];
                let trimmed = before.trim_end_matches(' ');
                let s = trimmed.rfind([' ', '\n']).map(|i| i + 1).unwrap_or(0);
                self.text.replace_range(s..self.cursor, "");
                self.cursor = s;
            }
            KeyCode::Char(c) if !ctrl => {
                let mut b = [0u8; 4];
                self.insert_str(c.encode_utf8(&mut b));
            }
            KeyCode::Enter if self.multiline => self.insert_str("\n"),
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let p = self.prev_boundary();
                    self.text.replace_range(p..self.cursor, "");
                    self.cursor = p;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.text.len() {
                    let n = self.next_boundary();
                    self.text.replace_range(self.cursor..n, "");
                }
            }
            KeyCode::Left => self.cursor = self.prev_boundary(),
            KeyCode::Right => self.cursor = self.next_boundary(),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Up | KeyCode::Down if self.multiline => {
                let col = self.text[self.line_start()..self.cursor].chars().count();
                let target_start = if k.code == KeyCode::Up {
                    let ls = self.line_start();
                    if ls == 0 {
                        return true;
                    }
                    self.text[..ls - 1].rfind('\n').map(|i| i + 1).unwrap_or(0)
                } else {
                    let le = self.line_end();
                    if le == self.text.len() {
                        return true;
                    }
                    le + 1
                };
                let line_end = self.text[target_start..].find('\n').map(|i| target_start + i).unwrap_or(self.text.len());
                let mut pos = target_start;
                for (n, (i, _)) in self.text[target_start..line_end].char_indices().enumerate() {
                    if n == col {
                        pos = target_start + i;
                        break;
                    }
                    pos = line_end;
                }
                if col == 0 {
                    pos = target_start;
                }
                self.cursor = pos;
            }
            _ => return false,
        }
        true
    }

    /// (row, display column) of the cursor.
    pub fn cursor_rowcol(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let row = before.matches('\n').count();
        let col = crate::text::width(&before[self.line_start()..]);
        (row, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn editing() {
        let mut t = TextArea::single("");
        for c in "héllo".chars() {
            t.handle(&k(KeyCode::Char(c)));
        }
        assert_eq!(t.text, "héllo");
        t.handle(&k(KeyCode::Left));
        t.handle(&k(KeyCode::Backspace));
        assert_eq!(t.text, "hélo");
        t.handle(&k(KeyCode::Home));
        t.handle(&k(KeyCode::Delete));
        assert_eq!(t.text, "élo");
        t.insert_str("a\nb");
        assert_eq!(t.text, "a bélo");
        t.handle(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(t.text, "élo");
    }

    #[test]
    fn multiline_nav() {
        let mut t = TextArea::multi("abc\nde");
        assert_eq!(t.cursor_rowcol(), (1, 2));
        t.handle(&k(KeyCode::Up));
        assert_eq!(t.cursor_rowcol(), (0, 2));
        t.handle(&k(KeyCode::Enter));
        assert_eq!(t.text, "ab\nc\nde");
        t.handle(&k(KeyCode::Down));
        t.handle(&k(KeyCode::Down));
        assert_eq!(t.cursor_rowcol().0, 2);
    }

    #[test]
    fn emoji_backspace() {
        let mut t = TextArea::single("a👨‍👩‍👧");
        t.handle(&k(KeyCode::Backspace));
        assert_eq!(t.text, "a");
    }
}
