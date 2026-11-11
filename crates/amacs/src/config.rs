// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `[amacs]` in `aterm.toml` (design §9, slice C1): the one parser every face reads. It reads
//! `default_editor` alone so far; the other keys §9 lists, the key bindings and the language
//! settings are planned, and a key it does not read is the config language's to name.

/// The keys of `[amacs]` this parser reads.
pub const KEYS: &[&str] = &["default_editor"];

/// What `[amacs]` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmacsConfig {
    /// `default_editor`: aterm's tabs get `EDITOR=amacs` when neither `EDITOR` nor `VISUAL` is
    /// set (design §8.3). On unless written `false`.
    pub default_editor: bool,
}

impl Default for AmacsConfig {
    fn default() -> Self {
        Self {
            default_editor: true,
        }
    }
}

impl AmacsConfig {
    /// Read `[amacs]` from `aterm.toml`'s text, with a note for each value it could not take.
    ///
    /// Text that is not TOML reads as the defaults with no note: the reader of the whole file
    /// names that error. An `[amacs]` that is not a table, and a `default_editor` that is not a
    /// boolean, keep the defaults and are each a note.
    #[must_use]
    pub fn from_aterm_toml(text: &str) -> (Self, Vec<String>) {
        let mut config = Self::default();
        let mut notes = Vec::new();
        let Ok(doc) = aterm_toml::from_str::<aterm_toml::Value>(text) else {
            return (config, notes);
        };
        let Some(amacs) = doc.get("amacs") else {
            return (config, notes);
        };
        let Some(table) = amacs.as_table() else {
            notes.push(format!(
                "amacs: not a table (a TOML {}); its settings keep their defaults",
                amacs.type_str()
            ));
            return (config, notes);
        };
        if let Some(value) = table.get("default_editor") {
            match value.as_bool() {
                Some(on) => config.default_editor = on,
                None => notes.push(format!(
                    "amacs.default_editor: wants true or false, not a TOML {}; amacs stays the \
                     default editor",
                    value.type_str()
                )),
            }
        }
        (config, notes)
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
