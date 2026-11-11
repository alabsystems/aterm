// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
