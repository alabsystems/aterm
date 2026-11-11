// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn absent_is_on_and_false_is_off() {
    assert!(parse_update_enabled(""));
    assert!(parse_update_enabled("font_px = 12.0\n"));
    assert!(parse_update_enabled("[update]\nauto_apply = false\n"));
    assert!(parse_update_enabled("[update]\nenabled = true\n"));
    assert!(!parse_update_enabled("[update]\nenabled = false\n"));
}

#[test]
fn auto_apply_is_on_unless_the_table_says_false() {
    assert!(parse_update_auto_apply(""));
    assert!(parse_update_auto_apply("[update]\nenabled = false\n"));
    assert!(!parse_update_auto_apply("[update]\nauto_apply = false\n"));
    assert!(parse_update_auto_apply("[update]\nauto_apply = \"no\"\n"));
    assert!(!parse_update_enabled(
        "[update]\nenabled = false\nauto_apply = \"no\"\n"
    ));
    assert!(!parse_update_auto_apply(
        "[update]\nenabled = \"no\"\nauto_apply = false\n"
    ));
}

#[test]
fn only_the_update_table_is_read() {
    assert!(!parse_update_enabled(
        "[packages]\nenabled = \"yes\"\n[update]\nenabled = false\n"
    ));
    assert!(parse_update_enabled("[update]\nenabled = \"no\"\n"));
    assert!(parse_update_enabled("[update\nbroken"));
}

#[test]
fn a_missing_or_foreign_file_is_on() {
    let dir = crate::test_scratch_dir("upd-settings");
    assert!(update_enabled_at(&dir.path().join("absent.toml")));
    assert!(
        update_enabled_at(dir.path()),
        "a directory is not a config file"
    );
    let off = dir.path().join("aterm.toml");
    std::fs::write(&off, "[update]\nenabled = false\n").unwrap();
    assert!(!update_enabled_at(&off));
}

/// THE SAVED SETTING IS READ AGAIN AT EACH CALL, AS THE PARSER READS IT: it follows the file
/// as the file changes, and reads every text, one that does not parse included, as
/// `parse_update_enabled` does (which alone warns).
#[test]
fn the_saved_setting_follows_the_file_and_reads_as_the_parser_does() {
    for text in [
        "",
        "[update]\nenabled = false\n",
        "[update]\nenabled = true\n",
        "[update]\nenabled = \"no\"\n",
        "[update\nbroken",
    ] {
        assert_eq!(
            update_enabled_in(text).unwrap_or(true),
            parse_update_enabled(text),
            "{text:?}"
        );
    }
    let dir = crate::test_scratch_dir("upd-settings-saved");
    let path = dir.path().join("aterm.toml");
    assert!(update_enabled_saved_at(None), "no config path reads as on");
    assert!(update_enabled_saved_at(Some(&path)), "no file reads as on");
    std::fs::write(&path, "[update]\nenabled = false\n").unwrap();
    assert!(!update_enabled_saved_at(Some(&path)), "turned off");
    std::fs::write(&path, "[update]\nenabled = true\n").unwrap();
    assert!(update_enabled_saved_at(Some(&path)), "turned on again");
    std::fs::write(&path, "[update\nbroken").unwrap();
    assert!(
        update_enabled_saved_at(Some(&path)),
        "a broken file reads as on"
    );
}
