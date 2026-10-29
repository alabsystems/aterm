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
mod tests {
    use super::*;

    /// The opt-out reads off, and nothing else does.
    /// NEGATIVE CONTROL: the same files with `default_editor = true`, the key absent, the key
    /// under another table, or a string `"false"` read on.
    #[test]
    fn default_editor_is_off_only_where_written_false() {
        let off = AmacsConfig::from_aterm_toml("[amacs]\ndefault_editor = false\n");
        assert_eq!(
            off,
            (
                AmacsConfig {
                    default_editor: false,
                },
                Vec::new()
            )
        );
        let dotted = AmacsConfig::from_aterm_toml("amacs.default_editor = false\n");
        assert!(!dotted.0.default_editor, "a dotted key is the same key");

        for on in [
            "[amacs]\ndefault_editor = true\n",
            "",
            "font_px = 14\n[amacs]\nfill_column = 72\n",
            "[packages]\ndefault_editor = false\n",
        ] {
            let (config, notes) = AmacsConfig::from_aterm_toml(on);
            assert!(config.default_editor, "{on:?} reads on");
            assert!(notes.is_empty(), "{on:?}: {notes:?}");
        }
    }

    /// A value it cannot take keeps the default and says so; text that is not TOML is the
    /// whole file's reader's to name.
    #[test]
    fn a_value_it_cannot_take_keeps_the_default_with_a_note() {
        let (config, notes) = AmacsConfig::from_aterm_toml("[amacs]\ndefault_editor = \"false\"\n");
        assert!(config.default_editor);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("amacs.default_editor: wants true or false, not a TOML string"),
            "{notes:?}"
        );

        let (config, notes) = AmacsConfig::from_aterm_toml("amacs = false\n");
        assert!(config.default_editor);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("amacs: not a table (a TOML boolean)"));

        let (config, notes) = AmacsConfig::from_aterm_toml("[amacs\ndefault_editor = false\n");
        assert_eq!(config, AmacsConfig::default());
        assert!(notes.is_empty(), "{notes:?}");
    }
}
