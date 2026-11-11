// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn hyphenated(word: &str) -> String {
    let mut out = String::new();
    let mut from = 0;
    for at in hyphen_points(word) {
        out.push_str(&word[from..at]);
        out.push('-');
        from = at;
    }
    out.push_str(&word[from..]);
    out
}

/// Syllable breaks follow Liang's English patterns and saturate `LEFT_MIN + right_min`.
/// NEGATIVE CONTROL: unchecked `LEFT_MIN + right_min` panics on `right_min == usize::MAX`.
#[test]
fn words_break_at_their_syllables() {
    for (word, broken) in [
        ("Brightness", "Bright-ness"),
        ("AUTOMATIC", "AU-TO-MATIC"),
        ("notifications", "no-ti-fi-ca-tions"),
        ("Messages.", "Mes-sages."),
        ("hyphenation", "hy-phen-ation"),
        ("table", "ta-ble"),
        ("associate", "as-so-ciate"),
        ("project", "project"),
        ("Choose", "Choose"),
        ("Theme", "Theme"),
        ("the", "the"),
        ("(default)", "(de-fault)"),
        ("built-in", "built-in"),
    ] {
        assert_eq!(hyphenated(word), broken, "{word:?}");
    }
    assert!(letter_run_points("hyphenation", usize::MAX).is_empty());
}

#[test]
fn identifiers_take_no_hyphen() {
    for identifier in [
        "alab.systems",
        "aterm.toml",
        "0.11.0+g20565c325",
        "ec73ab1c007d90ca",
        "/Applications/aterm.app",
        "~/.config/aterm",
        "--columns",
        "macOS",
        "MacBook-Pro-M4",
        "trust_verify",
    ] {
        assert!(
            hyphen_points(identifier).is_empty(),
            "{identifier:?}: {:?}",
            hyphen_points(identifier)
        );
    }
}
