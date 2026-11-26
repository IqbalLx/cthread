//! Command keys: the key pressed after the prefix for each action.
//! Every action has a default; `[keys.commands]` in the config overrides them.

use crate::keys::Prefix;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    NewThread,
    Pull,
    Close,
    FocusLeft,
    FocusRight,
    NextThread,
    PrevThread,
    FocusMain,
    Rename,
    MoveDown,
    MoveUp,
    Archive,
    ShowArchived,
    Delete,
    Shrink,
    Grow,
    ToggleSidebar,
    Help,
    Quit,
}

impl Action {
    /// Every action, in help-screen order: (action, config name, default key, help text).
    pub const ALL: &'static [(Action, &'static str, &'static str, &'static str)] = &[
        (Action::NewThread, "new_thread", "n", "New thread (forks main)"),
        (Action::Pull, "pull", "p", "Pull the open thread into main"),
        (Action::Close, "close", "x", "Close the thread pane"),
        (Action::FocusLeft, "focus_left", "h", "Focus left"),
        (Action::FocusRight, "focus_right", "l", "Focus right"),
        (Action::NextThread, "next_thread", "j", "Open next thread"),
        (Action::PrevThread, "prev_thread", "k", "Open previous thread"),
        (Action::FocusMain, "focus_main", "m", "Focus main"),
        (Action::Rename, "rename", "r", "Rename thread"),
        (Action::MoveDown, "move_down", "J", "Move thread down"),
        (Action::MoveUp, "move_up", "K", "Move thread up"),
        (Action::Archive, "archive", "a", "Archive thread"),
        (Action::ShowArchived, "show_archived", "A", "Show archived threads"),
        (Action::Delete, "delete", "D", "Delete thread"),
        (Action::Shrink, "shrink", "<", "Shrink the thread pane"),
        (Action::Grow, "grow", ">", "Grow the thread pane"),
        (Action::ToggleSidebar, "toggle_sidebar", "b", "Toggle sidebar"),
        (Action::Help, "help", "?", "This help"),
        (Action::Quit, "quit", "q", "Quit"),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|a| a.0 == self).map(|a| a.1).unwrap_or("?")
    }

    pub fn help(self) -> &'static str {
        Self::ALL.iter().find(|a| a.0 == self).map(|a| a.3).unwrap_or("")
    }

    pub fn from_name(name: &str) -> Option<Action> {
        Self::ALL.iter().find(|a| a.1 == name).map(|a| a.0)
    }
}

/// A command key: a character, optionally with Ctrl/Alt. Plain letters are
/// case-sensitive (`J` is Shift-j); Ctrl letters are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub ch: char,
    pub ctrl: bool,
    pub alt: bool,
}

impl Key {
    /// Parse "n", "N", "?", "<", "Space", "C-n", "M-x".
    pub fn parse(s: &str) -> Option<Key> {
        let s = s.trim();
        if s.starts_with("C-") || s.starts_with("M-") {
            let p = Prefix::parse(s)?;
            return Some(Key { ch: p.ch, ctrl: p.ctrl, alt: p.alt });
        }
        let ch = if s.eq_ignore_ascii_case("space") {
            ' '
        } else {
            let mut it = s.chars();
            let c = it.next()?;
            if it.next().is_some() || c.is_control() {
                return None;
            }
            c
        };
        Some(Key { ch, ctrl: false, alt: false })
    }

    pub fn matches(&self, k: &KeyEvent) -> bool {
        if self.ctrl || self.alt {
            return Prefix { ch: self.ch, ctrl: self.ctrl, alt: self.alt }.matches(k);
        }
        let KeyCode::Char(c) = k.code else { return false };
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        // Some terminals report Shift-j as ('j', SHIFT), others as ('J', SHIFT).
        let c = if k.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() {
            c.to_ascii_uppercase()
        } else {
            c
        };
        c == self.ch
    }

    pub fn label(&self) -> String {
        if self.ctrl || self.alt {
            return Prefix { ch: self.ch, ctrl: self.ctrl, alt: self.alt }.label();
        }
        if self.ch == ' ' { "Space".into() } else { self.ch.to_string() }
    }
}

#[derive(Debug, Clone)]
pub struct Keymap {
    /// Keys per action; an action may have several, the first is shown.
    pub bindings: Vec<(Action, Vec<Key>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::build(&BTreeMap::new(), &mut Vec::new())
    }
}

/// Keys the prefix machine reserves: digits open threads by position.
fn reserved(k: &Key) -> bool {
    !k.ctrl && !k.alt && k.ch.is_ascii_digit() && k.ch != '0'
}

impl Keymap {
    /// Build from the config's `[keys.commands]` table (action name → key or
    /// keys). Unknown names, bad keys and clashes produce warnings; the
    /// affected action keeps its default.
    pub fn build(overrides: &BTreeMap<String, Vec<String>>, warnings: &mut Vec<String>) -> Keymap {
        for name in overrides.keys() {
            if Action::from_name(name).is_none() {
                let names: Vec<&str> = Action::ALL.iter().map(|a| a.1).collect();
                warnings.push(format!("config: keys.commands.{name} is not an action (one of: {})", names.join(", ")));
            }
        }
        let defaults = |a: Action| -> Vec<Key> {
            let d = Action::ALL.iter().find(|x| x.0 == a).unwrap().2;
            vec![Key::parse(d).unwrap()]
        };
        let mut bindings: Vec<(Action, Vec<Key>)> = Vec::new();
        for &(action, name, _, _) in Action::ALL {
            let keys = match overrides.get(name) {
                None => defaults(action),
                Some(list) => {
                    let mut keys = Vec::new();
                    for s in list {
                        match Key::parse(s) {
                            Some(k) if reserved(&k) => warnings.push(format!(
                                "config: keys.commands.{name} = {s:?}: 1-9 open threads by position; ignoring it"
                            )),
                            Some(k) => keys.push(k),
                            None => warnings.push(format!(
                                "config: keys.commands.{name} = {s:?} is not a key like \"n\", \"N\", \"?\" or \"C-n\"; ignoring it"
                            )),
                        }
                    }
                    // An explicit empty list unbinds the action.
                    if keys.is_empty() && !list.is_empty() { defaults(action) } else { keys }
                }
            };
            bindings.push((action, keys));
        }
        // Resolve clashes. A key set in the config beats a default key; between
        // two of the same kind, the action listed first in Action::ALL wins.
        let explicit = |a: Action| overrides.get(a.name()).is_some_and(|l| !l.is_empty());
        let mut taken: Vec<(Key, Action, bool)> = Vec::new();
        for pass_explicit in [true, false] {
            for (action, keys) in bindings.iter_mut() {
                if explicit(*action) != pass_explicit {
                    continue;
                }
                keys.retain(|k| match taken.iter().find(|(t, _, _)| t == k) {
                    None => {
                        taken.push((*k, *action, pass_explicit));
                        true
                    }
                    Some((_, other, _)) => {
                        if pass_explicit {
                            warnings.push(format!(
                                "config: keys.commands.{} = {:?} is already bound to {}; ignoring it",
                                action.name(),
                                k.label(),
                                other.name()
                            ));
                        }
                        false
                    }
                });
            }
        }
        // An action that lost all its configured keys falls back to its default, if free.
        for (action, keys) in bindings.iter_mut() {
            if keys.is_empty() && explicit(*action) {
                let d = defaults(*action)[0];
                if !taken.iter().any(|(t, _, _)| *t == d) {
                    taken.push((d, *action, false));
                    keys.push(d);
                }
            }
        }
        Keymap { bindings }
    }

    pub fn action_for(&self, k: &KeyEvent) -> Option<Action> {
        self.bindings.iter().find(|(_, keys)| keys.iter().any(|key| key.matches(k))).map(|(a, _)| *a)
    }

    /// Display label of an action's first key, or "" when unbound.
    pub fn label(&self, a: Action) -> String {
        self.bindings
            .iter()
            .find(|(x, _)| *x == a)
            .and_then(|(_, keys)| keys.first())
            .map(|k| k.label())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(c: char, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), m)
    }

    fn over(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect()
    }

    #[test]
    fn defaults() {
        let m = Keymap::default();
        assert_eq!(m.action_for(&ev('n', KeyModifiers::NONE)), Some(Action::NewThread));
        assert_eq!(m.action_for(&ev('J', KeyModifiers::SHIFT)), Some(Action::MoveDown));
        assert_eq!(m.action_for(&ev('j', KeyModifiers::SHIFT)), Some(Action::MoveDown));
        assert_eq!(m.action_for(&ev('j', KeyModifiers::NONE)), Some(Action::NextThread));
        assert_eq!(m.action_for(&ev('?', KeyModifiers::NONE)), Some(Action::Help));
        assert_eq!(m.action_for(&ev('n', KeyModifiers::CONTROL)), None);
        assert_eq!(m.label(Action::Pull), "p");
    }

    #[test]
    fn overrides_and_warnings() {
        let mut w = Vec::new();
        let m = Keymap::build(
            &over(&[
                ("new_thread", &["t", "C-n"]),
                ("pull", &["9"]),
                ("bogus", &["z"]),
                ("quit", &["Q"]),
                ("close", &["t"]),
            ]),
            &mut w,
        );
        assert_eq!(m.action_for(&ev('t', KeyModifiers::NONE)), Some(Action::NewThread));
        assert_eq!(m.action_for(&ev('n', KeyModifiers::CONTROL)), Some(Action::NewThread));
        // 'n' is no longer new_thread
        assert_eq!(m.action_for(&ev('n', KeyModifiers::NONE)), None);
        // digits are reserved → pull keeps its default
        assert_eq!(m.label(Action::Pull), "p");
        assert_eq!(m.action_for(&ev('Q', KeyModifiers::SHIFT)), Some(Action::Quit));
        assert_eq!(m.action_for(&ev('q', KeyModifiers::NONE)), None);
        // close and new_thread both asked for 't': new_thread is listed first and
        // wins; close falls back to its default
        assert_eq!(m.label(Action::Close), "x");
        assert_eq!(w.len(), 3, "{w:?}");
    }

    #[test]
    fn explicit_beats_default() {
        let mut w = Vec::new();
        // 'p' is pull's default; binding it to close takes it away from pull silently.
        let m = Keymap::build(&over(&[("close", &["p"])]), &mut w);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(m.action_for(&ev('p', KeyModifiers::NONE)), Some(Action::Close));
        assert_eq!(m.label(Action::Pull), "");
    }

    #[test]
    fn empty_list_unbinds() {
        let mut w = Vec::new();
        let m = Keymap::build(&over(&[("quit", &[])]), &mut w);
        assert!(w.is_empty());
        assert_eq!(m.action_for(&ev('q', KeyModifiers::NONE)), None);
        assert_eq!(m.label(Action::Quit), "");
    }
}
