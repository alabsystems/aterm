// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{IntLit, StrLit, parse_int_source, parse_str_source};

fn value(src: &str) -> Option<String> {
    parse_str_source(src).map(|lit| lit.value)
}

fn int(src: &str) -> Option<(String, String)> {
    parse_int_source(src).map(|IntLit { digits, suffix }| (digits, suffix))
}

fn some(digits: &str, suffix: &str) -> Option<(String, String)> {
    Some((digits.to_owned(), suffix.to_owned()))
}

#[test]
fn integers_in_every_base_read_as_base_ten() {
    assert_eq!(int("0"), some("0", ""));
    assert_eq!(int("007"), some("7", ""));
    assert_eq!(int("1_000_000"), some("1000000", ""));
    assert_eq!(int("0x1F"), some("31", ""));
    assert_eq!(int("0xdead_BEEF"), some("3735928559", ""));
    assert_eq!(int("0o777"), some("511", ""));
    assert_eq!(int("0b1010_0101"), some("165", ""));
    assert_eq!(int("-42"), some("-42", ""));
    assert_eq!(int("-0"), some("-0", ""));
    assert_eq!(
        int("0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff_ff"),
        some("87112285931760246646623899502532662132735", "")
    );
}

#[test]
fn integer_suffixes_are_kept() {
    assert_eq!(int("7u8"), some("7", "u8"));
    assert_eq!(int("0x10_i64"), some("16", "i64"));
    assert_eq!(int("1f32"), some("1", "f32"));
    assert_eq!(int("1em"), some("1", "em"));
    assert_eq!(int("0x1e3"), some("483", ""));
}

#[test]
fn what_is_not_an_integer_is_refused() {
    assert_eq!(int("1.0"), None);
    assert_eq!(int("1e3"), None);
    assert_eq!(int("1e-3"), None);
    assert_eq!(int("1E+3"), None);
    assert_eq!(int("2e1_0f64"), None);
    assert_eq!(int("0b102"), None);
    assert_eq!(int("0o8"), None);
    assert_eq!(int("0x"), None);
    assert_eq!(int("0x_"), None);
    assert_eq!(int("x1"), None);
    assert_eq!(int("\"1\""), None);
    assert_eq!(int(""), None);
}

#[test]
fn to_i64_says_what_does_not_fit() {
    let read = |src: &str| parse_int_source(src).map(|lit| lit.to_i64());
    assert_eq!(read("9223372036854775807"), Some(Ok(i64::MAX)));
    assert_eq!(read("-9223372036854775808"), Some(Ok(i64::MIN)));
    assert_eq!(read("0xffu8"), Some(Ok(255)));
    assert_eq!(
        read("9223372036854775808"),
        Some(Err("number too large to fit in target type".to_owned()))
    );
    assert_eq!(
        read("-9223372036854775809"),
        Some(Err("number too small to fit in target type".to_owned()))
    );
}

#[test]
fn cooked_escapes() {
    assert_eq!(value(r#""plain""#).as_deref(), Some("plain"));
    assert_eq!(
        value(r#""a\nb\tc\\d\0e\'f\"g""#).as_deref(),
        Some("a\nb\tc\\d\0e'f\"g")
    );
    assert_eq!(value(r#""\x41\x7f""#).as_deref(), Some("A\x7f"));
    assert_eq!(
        value(r#""\u{1F600}\u{1_f6_00}\u{e9}""#).as_deref(),
        Some("😀😀é")
    );
    assert_eq!(value("\"a\\\n    b\"").as_deref(), Some("ab"));
    assert_eq!(value("\"ünï\"").as_deref(), Some("ünï"));
}

#[test]
fn cooked_rejects_what_the_lexer_would() {
    assert_eq!(value(r#""\x80""#), None);
    assert_eq!(value(r#""\u{D800}""#), None);
    assert_eq!(value(r#""\u{}""#), None);
    assert_eq!(value(r#""\u{1234567}""#), None);
    assert_eq!(value(r#""\q""#), None);
    assert_eq!(value(r#""unterminated"#), None);
}

#[test]
fn raw_strings() {
    assert_eq!(value(r##"r"a\nb""##).as_deref(), Some("a\\nb"));
    assert_eq!(value(r###"r#"say "hi""#"###).as_deref(), Some("say \"hi\""));
    assert_eq!(value(r###"r##"a"#b"##"###).as_deref(), Some("a\"#b"));
    assert_eq!(value(r##"r#"open""##), None);
}

#[test]
fn suffixes_are_kept_and_other_literals_refused() {
    assert_eq!(
        parse_str_source(r#""x"suf"#),
        Some(StrLit {
            value: "x".to_owned(),
            suffix: "suf".to_owned()
        })
    );
    assert_eq!(value(r#"b"bytes""#), None);
    assert_eq!(value(r#"c"cstr""#), None);
    assert_eq!(value("'c'"), None);
    assert_eq!(value("42"), None);
}
