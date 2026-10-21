// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A key chord (`C-x`, `M-left`): one key with its modifiers, the unit a
//! keymap binds and a key sequence is made of.

use aterm_types::keyboard::{Key, Modifiers, NamedKey};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyChord {
    pub control: bool,
    pub meta: bool,
    pub shift: bool,
    pub key: String,
}

impl KeyChord {
    pub fn parse(source: &str) -> Option<Self> {
        // A chord names one key; whitespace only ever separates the chords of
        // a sequence, so `"C-x u"` is not a chord (with key `x u`).
        if source.contains(char::is_whitespace) {
            return None;
        }
        let mut control = false;
        let mut meta = false;
        let mut shift = false;
        let mut parts = source.split('-').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                return (!part.is_empty()).then(|| Self {
                    control,
                    meta,
                    shift,
                    key: part.to_ascii_lowercase(),
                });
            }
            match part {
                "C" => control = true,
                "M" => meta = true,
                "S" => shift = true,
                _ => return None,
            }
        }
        None
    }

    /// The chord a key press makes: Ctrl is `C-`, Alt or Meta is `M-`, Shift
    /// is `S-`, and a letter is its lowercase self. `None` for a key the
    /// keymap has no name for (a function key, Up, Home), which then reaches
    /// no binding. Both faces spell their keys through this one function.
    pub fn from_key(key: &Key, mods: Modifiers) -> Option<Self> {
        let key = match key {
            Key::Character(character) => character.to_ascii_lowercase().to_string(),
            Key::Named(named) => match named {
                NamedKey::Space => "space",
                NamedKey::Backspace => "backspace",
                NamedKey::Delete => "delete",
                NamedKey::Enter => "enter",
                NamedKey::Escape => "escape",
                NamedKey::Tab => "tab",
                // Word motion on modified arrows (`M-left` and, off macOS,
                // `C-left`) is the only binding arrows have.
                NamedKey::ArrowLeft => "left",
                NamedKey::ArrowRight => "right",
                _ => return None,
            }
            .to_string(),
            _ => return None,
        };
        Some(Self {
            control: mods.contains(Modifiers::CTRL),
            meta: mods.intersects(Modifiers::ALT | Modifiers::META),
            shift: mods.contains(Modifiers::SHIFT),
            key,
        })
    }

    /// ESC alone, the Meta prefix (`ESC x` is `M-x`).
    pub(super) fn is_escape(&self) -> bool {
        !self.control && !self.meta && !self.shift && self.key == "escape"
    }

    /// The chord as a keymap spells it (`C-x`, `M-S-,`), and
    /// [`Self::parse`] reads back.
    pub fn display(&self) -> String {
        let mut out = String::new();
        if self.control {
            out.push_str("C-");
        }
        if self.meta {
            out.push_str("M-");
        }
        if self.shift {
            out.push_str("S-");
        }
        out.push_str(&self.key);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How a key is spelled as a chord, pinned to the spelling aterm's native
    /// Editor built by hand before both faces shared this function: Ctrl is
    /// `C-`, Alt and Meta are both `M-`, Shift is `S-`, Super and Hyper are
    /// not spelled, a letter is its lowercase self, and only these named keys
    /// have a name.
    #[test]
    fn from_key_spells_keys_as_the_gui_host_did() {
        let named = |key| Key::Named(key);
        let rows: &[(Key, Modifiers, Option<&str>)] = &[
            (Key::Character('x'), Modifiers::CTRL, Some("C-x")),
            (Key::Character('X'), Modifiers::CTRL, Some("C-x")),
            (Key::Character('x'), Modifiers::ALT, Some("M-x")),
            (Key::Character('x'), Modifiers::META, Some("M-x")),
            (
                Key::Character('a'),
                Modifiers::CTRL | Modifiers::SHIFT,
                Some("C-S-a"),
            ),
            (
                Key::Character('f'),
                Modifiers::CTRL | Modifiers::ALT,
                Some("C-M-f"),
            ),
            (Key::Character('a'), Modifiers::SUPER, Some("a")),
            (Key::Character('a'), Modifiers::HYPER, Some("a")),
            (Key::Character('/'), Modifiers::CTRL, Some("C-/")),
            (named(NamedKey::Space), Modifiers::CTRL, Some("C-space")),
            (
                named(NamedKey::Backspace),
                Modifiers::CTRL,
                Some("C-backspace"),
            ),
            (named(NamedKey::Delete), Modifiers::ALT, Some("M-delete")),
            (named(NamedKey::Enter), Modifiers::ALT, Some("M-enter")),
            (named(NamedKey::Escape), Modifiers::empty(), Some("escape")),
            (named(NamedKey::Tab), Modifiers::SHIFT, Some("S-tab")),
            (named(NamedKey::ArrowLeft), Modifiers::ALT, Some("M-left")),
            (
                named(NamedKey::ArrowRight),
                Modifiers::CTRL,
                Some("C-right"),
            ),
            (named(NamedKey::ArrowUp), Modifiers::CTRL, None),
            (named(NamedKey::ArrowDown), Modifiers::ALT, None),
            (named(NamedKey::Home), Modifiers::CTRL, None),
            (named(NamedKey::End), Modifiers::empty(), None),
            (named(NamedKey::PageDown), Modifiers::CTRL, None),
            (named(NamedKey::F5), Modifiers::CTRL, None),
            (named(NamedKey::F6), Modifiers::empty(), None),
            (named(NamedKey::Insert), Modifiers::empty(), None),
        ];
        for (key, mods, spelled) in rows {
            let chord = KeyChord::from_key(key, *mods);
            assert_eq!(
                chord.as_ref().map(KeyChord::display).as_deref(),
                *spelled,
                "{key:?} {mods:?}"
            );
            // A spelled chord reads back as itself.
            if let Some(chord) = chord {
                assert_eq!(KeyChord::parse(&chord.display()), Some(chord));
            }
        }
        assert!(KeyChord::parse("escape").unwrap().is_escape());
        assert!(!KeyChord::parse("M-escape").unwrap().is_escape());
    }
}
