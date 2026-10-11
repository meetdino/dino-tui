//! A one-line text field, with the keys a shell's line editor has.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Input {
    pub text: String,
    /// In characters.
    pub cursor: usize,
}

impl Input {
    pub fn with(text: &str) -> Input {
        Input { text: text.into(), cursor: text.chars().count() }
    }

    fn byte(&self, char_idx: usize) -> usize {
        self.text.char_indices().nth(char_idx).map_or(self.text.len(), |(b, _)| b)
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn insert_str(&mut self, s: &str) {
        // One line: a pasted newline is a space.
        let s: String = s.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).collect();
        let at = self.byte(self.cursor);
        self.text.insert_str(at, &s);
        self.cursor += s.chars().count();
    }

    pub fn set(&mut self, s: &str) {
        self.text = s.into();
        self.cursor = self.len();
    }

    /// Edit with `k`; false when it isn't an editing key.
    pub fn key(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.len(),
            KeyCode::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Char('u') if ctrl => {
                let at = self.byte(self.cursor);
                self.text.replace_range(..at, "");
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => {
                let at = self.byte(self.cursor);
                self.text.truncate(at);
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut i = self.cursor;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                let (from, to) = (self.byte(i), self.byte(self.cursor));
                self.text.replace_range(from..to, "");
                self.cursor = i;
            }
            KeyCode::Char(c) if !ctrl => self.insert_str(&c.to_string()),
            KeyCode::Backspace if self.cursor > 0 => {
                let (from, to) = (self.byte(self.cursor - 1), self.byte(self.cursor));
                self.text.replace_range(from..to, "");
                self.cursor -= 1;
            }
            KeyCode::Backspace => {}
            KeyCode::Delete if self.cursor < self.len() => {
                let (from, to) = (self.byte(self.cursor), self.byte(self.cursor + 1));
                self.text.replace_range(from..to, "");
            }
            KeyCode::Delete => {}
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, ctrl: bool) -> KeyEvent {
        KeyEvent::new(code, if ctrl { KeyModifiers::CONTROL } else { KeyModifiers::NONE })
    }

    #[test]
    fn edits_like_a_shell_line() {
        let mut i = Input::default();
        for c in "fix the bug".chars() {
            i.key(&key(KeyCode::Char(c), false));
        }
        i.key(&key(KeyCode::Char('w'), true));
        assert_eq!(i.text, "fix the ");
        i.key(&key(KeyCode::Left, false));
        i.key(&key(KeyCode::Backspace, false));
        assert_eq!((i.text.as_str(), i.cursor), ("fix th ", 6));
        i.key(&key(KeyCode::Char('u'), true));
        assert_eq!((i.text.as_str(), i.cursor), (" ", 0));
        i.insert_str("é\nx");
        assert_eq!(i.text, "é x ");
    }
}
