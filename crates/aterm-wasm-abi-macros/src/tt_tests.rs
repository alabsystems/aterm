// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn source_lexes_back_to_the_same_tokens() {
    for src in [
        "Option<Vec<u8>>",
        "&'a str",
        "Result<Self, String>",
        "fn f(&mut self, x: &[u8]) -> u32 { 1 }",
        "::aterm_wasm_abi::__rt::Part",
        "#[doc = \" a \\\"quoted\\\" line\"]",
    ] {
        let tts = lex(src);
        assert_eq!(lex(&to_source(&tts)), tts, "{src}");
    }
}

#[test]
fn string_literals_unescape() {
    assert_eq!(str_value(r#"" plain""#).as_deref(), Some(" plain"));
    assert_eq!(
        str_value(r#""a\"b\\c\n\u{1F600}\x41""#).as_deref(),
        Some("a\"b\\c\n😀A")
    );
    assert_eq!(
        str_value(r##"r#" raw "with" quotes"#"##).as_deref(),
        Some(" raw \"with\" quotes")
    );
    assert_eq!(str_value(r#"r" raw""#).as_deref(), Some(" raw"));
    assert_eq!(str_value("42"), None);
}

#[test]
fn byte_literals_escape_what_rust_would_misread() {
    assert_eq!(byte_lit(b"a\"\\\x00\xff"), r#"b"a\"\\\x00\xff""#);
    assert_eq!(byte_lit("—".as_bytes()), r#"b"\xe2\x80\x94""#);
}
