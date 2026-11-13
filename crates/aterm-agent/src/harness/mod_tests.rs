// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{one_line, truncate_bytes};

#[test]
fn one_line_folds_what_a_hand_rolled_control_sweep_misses() {
    assert_eq!(one_line("  a \n b  ", 99), "a b");
    assert_eq!(one_line("with\ta tab", 99), "with a tab");
    assert_eq!(one_line("a\u{2028}b", 99), "a b");
    assert_eq!(one_line("a\u{2029}b", 99), "a b");
    assert_eq!(one_line("abcdef", 3), "abc");
    assert_eq!(one_line("é😀", 2), "é");
    assert_eq!(one_line("", 0), "");
    for (text, max) in [
        ("work", 80),
        ("a@b.example", 80),
        ("five hours left", 80),
        ("(x) ~y_z-1:2", 80),
        (" lead", 80),
        ("trail ", 80),
        ("two  spaces", 80),
        ("tab\there", 80),
        ("nb\u{a0}sp", 80),
        ("\u{3000}ideo", 80),
        ("cut here", 3),
        ("", 80),
    ] {
        let folded = crate::supervise::limit::one_line(truncate_bytes(text, max))
            .trim()
            .to_string();
        assert_eq!(one_line(text, max), folded, "{text:?} at {max}");
    }
}

#[test]
fn truncate_bytes_never_splits_a_character_and_never_grows_a_string() {
    assert_eq!(truncate_bytes("abc", 8), "abc");
    assert_eq!(truncate_bytes("abc", 3), "abc");
    assert_eq!(truncate_bytes("abc", 2), "ab");
    assert_eq!(truncate_bytes("abc", 0), "");
    assert_eq!(truncate_bytes("é", 1), "", "a two-byte character, cut at 1");
    assert_eq!(truncate_bytes("aé", 2), "a");
    for max in 0..4 {
        assert_eq!(truncate_bytes("😀", max), "", "max {max}");
    }
    assert_eq!(truncate_bytes("😀", 4), "😀");
    let mixed = "a😀é b\u{2028}ω";
    for max in 0..=mixed.len() + 2 {
        let cut = truncate_bytes(mixed, max);
        assert!(cut.len() <= max.min(mixed.len()), "max {max}");
        assert!(mixed.starts_with(cut), "max {max}");
    }
}
