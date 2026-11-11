// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Key chord representation and parsing (`C-x`, `M-left`).

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
        if source.is_empty() || source.contains(char::is_whitespace) {
            return None;
        }
        let mut control = false;
        let mut meta = false;
        let mut shift = false;
        let mut rest = source;
        while let Some((prefix, after)) = rest.split_once('-') {
            if after.is_empty() {
                break;
            }
            match prefix {
                "C" if !control => control = true,
                "M" if !meta => meta = true,
                "S" if !shift => shift = true,
                _ => return None,
            }
            rest = after;
        }
        (!rest.is_empty() && (rest == "-" || !rest.contains('-'))).then(|| Self {
            control,
            meta,
            shift,
            key: rest.to_ascii_lowercase(),
        })
    }

    /// Convert a key press and modifiers into a key chord.
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

    /// Whether this chord is `ESC` alone.
    pub(super) fn is_escape(&self) -> bool {
        !self.control && !self.meta && !self.shift && self.key == "escape"
    }

    /// Format the chord as a keymap spells it.
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
#[path = "chord_tests.rs"]
mod tests;
