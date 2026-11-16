// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A turn's messages join by one blank line, so the joined text holds each message whole, its own
/// blank lines included; one message is itself.
#[test]
fn messages_join_by_a_blank_line() {
    assert_eq!(MESSAGE_GAP, "\n\n");
    let messages = ["first".to_string(), "second\nrow".to_string()];
    assert_eq!(join_messages(&messages), "first\n\nsecond\nrow");
    assert_eq!(join_messages(&messages[..1]), "first");
    assert_eq!(join_messages(&[]), "");
    let paragraphs = ["one\n\ntwo".to_string(), "three".to_string()];
    assert_eq!(join_messages(&paragraphs), "one\n\ntwo\n\nthree");
}

/// Every control character is hidden, and so are the format characters a spoof rides on and the
/// noncharacters (U+FDD0..U+FDEF and the last two code points of every plane); a letter, a space,
/// CJK text, an emoji and the zero-width joiner and non-joiner, which emoji sequences and Indic
/// scripts need, are not.
#[test]
fn controls_and_invisible_format_characters_are_hidden() {
    for c in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
        if c.is_control() {
            assert!(is_hidden_char(c), "{c:?}");
        }
    }
    for c in [
        '\u{00ad}',  // soft hyphen
        '\u{200b}',  // zero-width space
        '\u{200e}',  // left-to-right mark
        '\u{202e}',  // right-to-left override
        '\u{2063}',  // invisible separator
        '\u{2066}',  // left-to-right isolate
        '\u{feff}',  // byte order mark
        '\u{e0041}', // tag latin capital letter A
        '\u{fdd0}',  // noncharacter, the first of U+FDD0..U+FDEF
        '\u{fdef}',  // noncharacter, the last of U+FDD0..U+FDEF
        '\u{fffe}',  // noncharacter, plane 0
        '\u{ffff}',  // noncharacter, plane 0
        '\u{1fffe}', // noncharacter, plane 1
        '\u{2ffff}', // noncharacter, plane 2
        '\u{10ffff}',
    ] {
        assert!(is_hidden_char(c), "{c:?}");
    }
    // Negative controls: printing text, spaces, the joiners, and the code points just outside
    // the noncharacter ranges.
    for c in [
        'a',
        ' ',
        'é',
        '\u{6f22}',
        '\u{1f600}',
        '\u{200c}',
        '\u{200d}',
        '\u{fdcf}',
        '\u{fdf0}',
        '\u{fffd}',
        '\u{1fffd}',
    ] {
        assert!(!is_hidden_char(c), "{c:?}");
    }
}

/// Only ASCII digits that fit the type parse: a sign, a space, a separator, a radix prefix, an
/// empty word, another script's digit or an overflow is no number.
#[test]
fn only_ascii_digits_parse() {
    assert_eq!(parse_uint::<u32>("42"), Some(42));
    assert_eq!(parse_uint::<u32>("007"), Some(7));
    assert_eq!(parse_uint::<u64>("18446744073709551615"), Some(u64::MAX));
    assert_eq!(parse_uint::<u8>("255"), Some(255));
    for word in [
        "", "+1", "-1", " 1", "1 ", "1.0", "1_000", "1,000", "0x10", "٣",
    ] {
        assert_eq!(parse_uint::<u32>(word), None, "{word:?}");
    }
    assert_eq!(parse_uint::<u64>("18446744073709551616"), None);
    assert_eq!(parse_uint::<u8>("256"), None);
}
