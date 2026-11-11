// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn fmt_spelling(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[test]
fn encode_is_the_wires_spelling_and_decode_inverts_it() {
    for (plain, encoded) in [
        ("hello", "hello"),
        ("two words", "two%20words"),
        ("a\nb", "a%0Ab"),
        ("100%", "100%25"),
        ("tab\there", "tab%09here"),
        ("é", "%C3%A9"),
        ("", ""),
    ] {
        assert_eq!(encode(plain), encoded, "encode {plain:?}");
        assert_eq!(decode(encoded), plain, "decode {encoded:?}");
    }
    let narrow = (0u32..0x800).filter_map(char::from_u32);
    let wider = (0x800u32..=0x10_ffff)
        .step_by(61)
        .filter_map(char::from_u32);
    for c in narrow.chain(wider) {
        let s = c.to_string();
        assert_eq!(encode(&s), fmt_spelling(&s), "{c:?}");
        assert_eq!(decode(&encode(&s)), s, "{c:?}");
    }
    for s in [
        "构建 agent ✨",
        "Details ›",
        "\t\n\r\u{7f}",
        "~/src/aterm",
        "-",
    ] {
        assert_eq!(decode(&encode(s)), s, "round-trip: {s:?}");
    }
    let mut into = String::from("k=");
    encode_into(&mut into, "a b");
    assert_eq!(into, "k=a%20b");
}

#[test]
fn an_argv_line_keeps_every_word_whole() {
    let argv = [
        "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm",
        "link",
        "100%",
        "tab\there",
        "line\nend",
        "nb\u{a0}sp",
        "Caf\u{e9}",
    ];
    let line = join_words(&argv);
    assert!(!line.contains('\n') && !line.contains('\t'), "{line}");
    assert_eq!(split_words(&line), argv, "{line}");
    assert_eq!(line.split_whitespace().count(), argv.len(), "{line}");
    let joined = argv.join(" ");
    let naive: Vec<&str> = joined.split_whitespace().collect();
    assert_ne!(naive, argv);
    let plain = [
        "/usr/bin/aterm",
        "link",
        "serve",
        "--state",
        "/Users//u/Caf\u{e9}",
    ];
    assert_eq!(join_words(&plain), plain.join(" "));
    assert_eq!(split_words(&plain.join(" ")), plain);
    assert_eq!(encode_word("a%b"), "a%25b");
    assert_eq!(encode_word("a b"), "a%20b");
    assert_eq!(encode_word("nb\u{a0}sp"), "nb%C2%A0sp");
}

#[test]
fn decode_is_total() {
    for s in ["%G1", "%zz", "%2", "%4", "trail%", "%4G", "%é", "%+5"] {
        assert_eq!(decode(s), s, "passes through: {s:?}");
    }
    assert_eq!(decode("%%41"), "%A");
    assert_eq!(decode("%c3%a9"), "é", "lowercase hex decodes too");
    assert_eq!(decode("%FF"), "\u{FFFD}");
}
