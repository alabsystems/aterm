// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The class, character by character: what is active and what is text.
#[test]
fn the_active_class_is_controls_bidi_and_the_separators() {
    for c in [
        '\u{0}', '\u{7}', '\t', '\n', '\r', '\u{1b}', '\u{1f}', '\u{7f}', '\u{80}', '\u{85}',
        '\u{9b}', '\u{9d}', '\u{9f}', '\u{61c}', '\u{200e}', '\u{200f}', '\u{2028}', '\u{2029}',
        '\u{202a}', '\u{202e}', '\u{2066}', '\u{2069}',
    ] {
        assert!(is_active(c), "{:04x} is active", c as u32);
    }
    for c in [
        ' ', 'a', '~', '\u{a0}', 'é', '\u{200b}', '\u{200d}', '\u{2027}', '\u{202f}', '\u{2065}',
        '\u{206a}', '界', '🙂',
    ] {
        assert!(!is_active(c), "{:04x} is text", c as u32);
    }
}

/// The escape table: each class's spelling, and printable text unchanged.
#[test]
fn safe_spells_each_class_and_keeps_text() {
    for (raw, shown) in [
        (
            &b"plain text, \xc3\xa9 \xe7\x95\x8c \xf0\x9f\x99\x82"[..],
            "plain text, é 界 🙂",
        ),
        (b"two\nlines\tand a tab\n", "two\nlines\tand a tab\n"),
        (b"cr\r", "cr\\r"),
        (b"\x1b]2;T\x07", "\\x1b]2;T\\x07"),
        (b"\x00\x1f\x7f", "\\x00\\x1f\\x7f"),
        (
            "\u{9b}31m\u{9d}0;t\u{9c}".as_bytes(),
            "\\u{009b}31m\\u{009d}0;t\\u{009c}",
        ),
        (
            "a\u{202e}b\u{2066}c\u{61c}d\u{200f}".as_bytes(),
            "a\\u{202e}b\\u{2066}c\\u{061c}d\\u{200f}",
        ),
        ("x\u{2028}y\u{2029}".as_bytes(), "x\\u{2028}y\\u{2029}"),
        (b"bad \xff byte", "bad \\xff byte"),
        (b"cut \xe2\x80", "cut \\xe2\\x80"),
        (b"overlong \xc0\xaf", "overlong \\xc0\\xaf"),
        (b"surrogate \xed\xa0\x80", "surrogate \\xed\\xa0\\x80"),
    ] {
        assert_eq!(safe(raw), shown, "{raw:?}");
    }
    assert!(matches!(safe(b"ok\n"), std::borrow::Cow::Borrowed(_)));
}

/// In pieces, cut at every byte: the same rendering as the whole, so a
/// character split across two reads is never shown as two bad bytes.
#[test]
fn a_stream_cut_anywhere_renders_as_the_whole() {
    let body = "a\u{9b}é界🙂\u{202e}\x1b\n\u{2028}z".as_bytes();
    let mut bad = body.to_vec();
    bad.extend_from_slice(b"\xff\xf0\x9f");
    for whole in [body, &bad[..]] {
        let want = safe(whole).into_owned();
        for cut in 0..=whole.len() {
            let mut out = String::new();
            let mut s = SafeStream::new();
            s.push(&whole[..cut], &mut out);
            s.push(&whole[cut..], &mut out);
            s.finish(&mut out);
            assert_eq!(out, want, "cut at {cut}");
        }
        let mut out = String::new();
        let mut s = SafeStream::new();
        for b in whole {
            s.push(std::slice::from_ref(b), &mut out);
        }
        s.finish(&mut out);
        assert_eq!(out, want, "byte by byte");
    }
}

/// `push_escape` names the three whitespace controls and spells the rest.
#[test]
fn push_escape_spells_every_class() {
    let mut out = String::new();
    for c in [
        '\n',
        '\r',
        '\t',
        '\u{1b}',
        '\u{7f}',
        '\u{85}',
        '\u{202e}',
        '\u{1f642}',
    ] {
        push_escape(&mut out, c);
    }
    assert_eq!(out, "\\n\\r\\t\\x1b\\x7f\\u{0085}\\u{202e}\\u{1f642}");
}
