// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A turn's messages join by one blank line, and one message is itself.
#[test]
fn messages_join_by_a_blank_line() {
    let messages = ["first".to_string(), "second\nrow".to_string()];
    assert_eq!(join_messages(&messages), "first\n\nsecond\nrow");
    assert_eq!(join_messages(&messages[..1]), "first");
    assert_eq!(join_messages(&[]), "");
}

/// Every control character is hidden, and so are the format characters a spoof rides on; a
/// letter, a space and an emoji are not.
#[test]
fn controls_and_invisible_format_characters_are_hidden() {
    for c in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
        if c.is_control() {
            assert!(is_hidden_char(c), "{c:?}");
        }
    }
    for c in [
        '\u{200b}',
        '\u{202e}',
        '\u{2066}',
        '\u{feff}',
        '\u{e0041}',
        '\u{fffe}',
    ] {
        assert!(is_hidden_char(c), "{c:?}");
    }
    for c in ['a', ' ', 'é', '\u{1f600}', '\u{200d}'] {
        assert!(!is_hidden_char(c), "{c:?}");
    }
}

/// Only ASCII digits parse: a sign, a space, an empty word or an overflow is no number.
#[test]
fn only_ascii_digits_parse() {
    assert_eq!(parse_uint::<u32>("42"), Some(42));
    assert_eq!(parse_uint::<u32>("007"), Some(7));
    for word in ["", "+1", "-1", " 1", "1 ", "1.0", "٣"] {
        assert_eq!(parse_uint::<u32>(word), None, "{word:?}");
    }
    assert_eq!(parse_uint::<u8>("256"), None);
}
