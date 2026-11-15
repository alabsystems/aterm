// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Each character of the alphabet is its own value, in order, and no other byte is a character.
#[test]
fn the_alphabet_is_its_values_in_order() {
    for (value, &b) in ALPHABET.iter().enumerate() {
        assert_eq!(digit(b).map(usize::from), Some(value), "{}", b as char);
    }
    for b in (0..=u8::MAX).filter(|b| !ALPHABET.contains(b)) {
        assert_eq!(digit(b), None, "{b}");
    }
}

/// A tab ID is the `t` form exactly; a case, an alias, a length, a prefix or a legacy `s-` ID
/// is none.
#[test]
fn a_tab_id_is_the_t_form_exactly() {
    for id in ["tqn5r6e0148", "t0000000000", "tzzzzzzzzzz"] {
        assert!(is_tab_id(id), "{id}");
    }
    for word in [
        "",
        "t",
        "tqn5r",
        "tqn5r6e014",
        "tqn5r6e01480",
        "TQN5R6E0148",
        "Tqn5r6e0148",
        "tqn5r6e014i",
        "tqn5r6e014l",
        "tqn5r6e014o",
        "tqn5r6e014u",
        "uqn5r6e0148",
        "s-0123456789abcdef0123",
        "s-0123456789abcdef012",
        "s-0123456789abcdef01234",
        "s-0123456789ABCDEF0123",
        "s-0123456789abcdef012g",
        "s_0123456789abcdef0123",
    ] {
        assert!(!is_tab_id(word), "{word:?}");
    }
}
