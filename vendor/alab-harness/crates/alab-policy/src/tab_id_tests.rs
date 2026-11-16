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

/// A tab ID is the `t` form exactly, and every alphabet character can stand in it; a case, an
/// alias, a length, a prefix, punctuation, a space, a multi-byte character or a legacy `s-` ID
/// is none.
#[test]
fn a_tab_id_is_the_t_form_exactly() {
    for id in [
        "tqn5r6e0148",
        "t0000000000",
        "tzzzzzzzzzz",
        "tabcdefghjk",
        "tmnpqrstvwx",
    ] {
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
        "tQN5R6E0148",
        "tqn5r6e014i",
        "tqn5r6e014l",
        "tqn5r6e014o",
        "tqn5r6e014u",
        "uqn5r6e0148",
        "sqn5r6e0148",
        "tqn5r6e014-",
        "tqn5r6e014 ",
        "t-qn5r6e014",
        "tqn5r6e01\u{e9}", // 11 bytes, a multi-byte character at the end
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

/// [`ALPHABET`] ascends in ASCII, as its doc says (aterm-types orders tab IDs by it), and the
/// lengths and bits are the grammar's: `t` and 10 characters of 5 bits, shown as `t` and 4.
#[test]
fn the_alphabet_ascends_and_the_lengths_are_the_grammars() {
    assert!(ALPHABET.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(LEN, 1 + 10);
    assert_eq!(SHORT_LEN, 5);
    assert_eq!("tqn5r6e0148".get(..SHORT_LEN), Some("tqn5r"));
    assert_eq!(BITS, 5 * 10);
}
