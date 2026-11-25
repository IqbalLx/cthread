//! Prefix-key state machine and key/mouse → terminal byte encoding.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

/// The command prefix key (default Ctrl-]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    pub ch: char,
    pub ctrl: bool,
    pub alt: bool,
}

impl Prefix {
    /// Parse "C-]", "C-a", "M-x", "C-M-b".
    pub fn parse(s: &str) -> Option<Prefix> {
        let mut ctrl = false;
        let mut alt = false;
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("C-") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("M-") {
                alt = true;
                rest = r;
            } else {
                break;
            }
        }
        let mut chars = rest.chars();
        let ch = chars.next()?;
        if chars.next().is_some() || !(ctrl || alt) {
            return None;
        }
        Some(Prefix { ch: ch.to_ascii_lowercase(), ctrl, alt })
    }

    pub fn matches(&self, k: &KeyEvent) -> bool {
        let KeyCode::Char(c) = k.code else { return false };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        if ctrl != self.ctrl || alt != self.alt {
            return false;
        }
        let c = c.to_ascii_lowercase();
        if c == self.ch {
            return true;
        }
        // Legacy terminals send 0x1c..0x1f for C-\ C-] C-^ C-_, which crossterm
        // reports as Ctrl-4..Ctrl-7.
        self.ctrl && matches!((self.ch, c), ('\\', '4') | (']', '5') | ('^', '6') | ('_', '7'))
    }

    /// Display form, e.g. `^]`.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.alt {
            s.push_str("M-");
        }
        if self.ctrl {
            s.push('^');
        }
        s.push(self.ch);
        s
    }

    /// Bytes for sending the prefix itself to a child.
    pub fn bytes(&self) -> Vec<u8> {
        let mods = (if self.ctrl { KeyModifiers::CONTROL } else { KeyModifiers::NONE })
            | (if self.alt { KeyModifiers::ALT } else { KeyModifiers::NONE });
        encode_key(&KeyEvent::new(KeyCode::Char(self.ch), mods), false)
    }
}

/// Result of feeding one key into the prefix state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Send this key to the focused pane.
    Forward(KeyEvent),
    /// Prefix pressed; waiting for the command key.
    Armed,
    /// Prefix + this key: a cthread command.
    Command(KeyEvent),
    /// Prefix + prefix: send a literal prefix to the pane.
    LiteralPrefix,
    /// Key ignored (release events, etc).
    Ignore,
}

pub const PREFIX_HINT: Duration = Duration::from_millis(1500);

#[derive(Debug)]
pub struct PrefixMachine {
    pub prefix: Prefix,
    armed_at: Option<Instant>,
}

impl PrefixMachine {
    pub fn new(prefix: Prefix) -> Self {
        Self { prefix, armed_at: None }
    }

    pub fn is_armed(&self) -> bool {
        self.armed_at.is_some()
    }

    /// Whether the status-line `^]…` hint should still show.
    pub fn hint_visible(&self, now: Instant) -> bool {
        self.armed_at.is_some_and(|t| now.duration_since(t) < PREFIX_HINT)
    }

    pub fn feed(&mut self, k: KeyEvent) -> KeyOutcome {
        if k.kind == KeyEventKind::Release {
            return KeyOutcome::Ignore;
        }
        if self.armed_at.take().is_some() {
            if self.prefix.matches(&k) {
                return KeyOutcome::LiteralPrefix;
            }
            // Bare modifier presses (kitty protocol) don't count.
            if matches!(k.code, KeyCode::Modifier(_)) {
                self.armed_at = Some(Instant::now());
                return KeyOutcome::Armed;
            }
            return KeyOutcome::Command(k);
        }
        if self.prefix.matches(&k) {
            self.armed_at = Some(Instant::now());
            return KeyOutcome::Armed;
        }
        KeyOutcome::Forward(k)
    }
}

fn mod_param(m: KeyModifiers) -> u8 {
    1 + (m.contains(KeyModifiers::SHIFT) as u8)
        + 2 * (m.contains(KeyModifiers::ALT) as u8)
        + 4 * (m.contains(KeyModifiers::CONTROL) as u8)
}

/// Encode a key event as the bytes a terminal would send.
/// `app_cursor` is DECCKM (application cursor keys) from the child's screen.
pub fn encode_key(k: &KeyEvent, app_cursor: bool) -> Vec<u8> {
    let m = k.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let has_mods = m.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
    let mut out = Vec::new();

    let csi_letter = |letter: u8, out: &mut Vec<u8>| {
        if has_mods {
            out.extend_from_slice(format!("\x1b[1;{}", mod_param(m)).as_bytes());
            out.push(letter);
        } else if app_cursor {
            out.extend_from_slice(&[0x1b, b'O', letter]);
        } else {
            out.extend_from_slice(&[0x1b, b'[', letter]);
        }
    };
    let csi_tilde = |n: u8, out: &mut Vec<u8>| {
        if has_mods {
            out.extend_from_slice(format!("\x1b[{n};{}~", mod_param(m)).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{n}~").as_bytes());
        }
    };

    match k.code {
        KeyCode::Char(c) => {
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
                    '@' | ' ' | '2' => Some(0),
                    '[' | '3' => Some(0x1b),
                    '\\' | '4' => Some(0x1c),
                    ']' | '5' => Some(0x1d),
                    '^' | '6' => Some(0x1e),
                    '_' | '7' | '-' => Some(0x1f),
                    '?' | '8' => Some(0x7f),
                    _ => None,
                };
                match b {
                    Some(b) => out.push(b),
                    None => out.extend_from_slice(c.to_string().as_bytes()),
                }
            } else {
                out.extend_from_slice(c.to_string().as_bytes());
            }
        }
        KeyCode::Enter => {
            // Shift/Alt+Enter → ESC CR, which Claude Code treats as a newline.
            if alt || m.contains(KeyModifiers::SHIFT) {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        KeyCode::Tab => {
            if m.contains(KeyModifiers::SHIFT) {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                out.push(b'\t');
            }
        }
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(if ctrl { 0x08 } else { 0x7f });
        }
        KeyCode::Esc => {
            if alt {
                out.push(0x1b);
            }
            out.push(0x1b);
        }
        KeyCode::Up => csi_letter(b'A', &mut out),
        KeyCode::Down => csi_letter(b'B', &mut out),
        KeyCode::Right => csi_letter(b'C', &mut out),
        KeyCode::Left => csi_letter(b'D', &mut out),
        KeyCode::Home => csi_letter(b'H', &mut out),
        KeyCode::End => csi_letter(b'F', &mut out),
        KeyCode::Insert => csi_tilde(2, &mut out),
        KeyCode::Delete => csi_tilde(3, &mut out),
        KeyCode::PageUp => csi_tilde(5, &mut out),
        KeyCode::PageDown => csi_tilde(6, &mut out),
        KeyCode::F(n) => match n {
            1..=4 => {
                let letter = b'P' + (n - 1);
                if has_mods {
                    out.extend_from_slice(format!("\x1b[1;{}", mod_param(m)).as_bytes());
                    out.push(letter);
                } else {
                    out.extend_from_slice(&[0x1b, b'O', letter]);
                }
            }
            5..=12 => {
                let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
                csi_tilde(code, &mut out);
            }
            _ => {}
        },
        _ => {}
    }
    out
}

/// Wrap text as a bracketed paste.
pub fn bracketed(text: &str) -> Vec<u8> {
    // Strip any embedded paste terminators so the child can't be tricked into
    // ending the paste early.
    let clean = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
    let mut v = b"\x1b[200~".to_vec();
    v.extend_from_slice(clean.as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
}

/// Encode a scroll-wheel event for a child that enabled mouse reporting.
/// `col`/`row` are 0-based, relative to the pane.
pub fn encode_wheel(up: bool, col: u16, row: u16, sgr: bool) -> Vec<u8> {
    let b = if up { 64 } else { 65 };
    if sgr {
        format!("\x1b[<{b};{};{}M", col + 1, row + 1).into_bytes()
    } else {
        vec![0x1b, b'[', b'M', 32 + b as u8, (33 + col.min(222)) as u8, (33 + row.min(222)) as u8]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn prefix_parse() {
        let p = Prefix::parse("C-]").unwrap();
        assert_eq!(p, Prefix { ch: ']', ctrl: true, alt: false });
        assert_eq!(p.label(), "^]");
        assert_eq!(p.bytes(), vec![0x1d]);
        assert!(Prefix::parse("]").is_none());
        assert!(Prefix::parse("C-ab").is_none());
        assert_eq!(Prefix::parse("M-x").unwrap().bytes(), vec![0x1b, b'x']);
    }

    #[test]
    fn prefix_matches_legacy_alias() {
        let p = Prefix::parse("C-]").unwrap();
        assert!(p.matches(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
        assert!(p.matches(&key(KeyCode::Char('5'), KeyModifiers::CONTROL)));
        assert!(!p.matches(&key(KeyCode::Char(']'), KeyModifiers::NONE)));
    }

    #[test]
    fn machine() {
        let mut m = PrefixMachine::new(Prefix::parse("C-]").unwrap());
        let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
        let pre = key(KeyCode::Char(']'), KeyModifiers::CONTROL);
        assert_eq!(m.feed(a), KeyOutcome::Forward(a));
        assert_eq!(m.feed(pre), KeyOutcome::Armed);
        assert!(m.is_armed());
        let n = key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.feed(n), KeyOutcome::Command(n));
        assert!(!m.is_armed());
        assert_eq!(m.feed(pre), KeyOutcome::Armed);
        assert_eq!(m.feed(pre), KeyOutcome::LiteralPrefix);
        assert_eq!(m.feed(a), KeyOutcome::Forward(a));
        let mut rel = a;
        rel.kind = KeyEventKind::Release;
        assert_eq!(m.feed(rel), KeyOutcome::Ignore);
    }

    #[test]
    fn encoding() {
        let n = KeyModifiers::NONE;
        assert_eq!(encode_key(&key(KeyCode::Char('a'), n), false), b"a");
        assert_eq!(encode_key(&key(KeyCode::Char('é'), n), false), "é".as_bytes());
        assert_eq!(encode_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false), vec![3]);
        assert_eq!(encode_key(&key(KeyCode::Char('b'), KeyModifiers::ALT), false), b"\x1bb");
        assert_eq!(encode_key(&key(KeyCode::Enter, n), false), b"\r");
        assert_eq!(encode_key(&key(KeyCode::Enter, KeyModifiers::SHIFT), false), b"\x1b\r");
        assert_eq!(encode_key(&key(KeyCode::Up, n), false), b"\x1b[A");
        assert_eq!(encode_key(&key(KeyCode::Up, n), true), b"\x1bOA");
        assert_eq!(encode_key(&key(KeyCode::Left, KeyModifiers::CONTROL), false), b"\x1b[1;5D");
        assert_eq!(encode_key(&key(KeyCode::Delete, n), false), b"\x1b[3~");
        assert_eq!(encode_key(&key(KeyCode::PageUp, KeyModifiers::SHIFT), false), b"\x1b[5;2~");
        assert_eq!(encode_key(&key(KeyCode::F(1), n), false), b"\x1bOP");
        assert_eq!(encode_key(&key(KeyCode::F(5), n), false), b"\x1b[15~");
        assert_eq!(encode_key(&key(KeyCode::Backspace, n), false), vec![0x7f]);
        assert_eq!(encode_key(&key(KeyCode::Esc, n), false), vec![0x1b]);
        assert_eq!(encode_key(&key(KeyCode::BackTab, KeyModifiers::SHIFT), false), b"\x1b[Z");
    }

    #[test]
    fn paste_and_mouse() {
        assert_eq!(bracketed("hi"), b"\x1b[200~hi\x1b[201~");
        assert_eq!(bracketed("a\x1b[201~b"), b"\x1b[200~ab\x1b[201~");
        assert_eq!(encode_wheel(true, 0, 0, true), b"\x1b[<64;1;1M");
        assert_eq!(encode_wheel(false, 2, 3, false), vec![0x1b, b'[', b'M', 97, 35, 36]);
    }
}
