// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TOML 1.1 syntax and lossless edits, following https://toml.io/en/v1.1.0.

use aterm_toml::edit::{DocumentMut, Value as EditValue};
use aterm_toml::{Datetime, Value};
use std::collections::BTreeMap;

#[test]
fn two_digit_escapes_are_unicode_scalars_and_keep_their_spelling() {
    for code in 0u8..=255 {
        let source = format!("text = \"\\x{code:02X}\"\n");
        let value: BTreeMap<String, String> = aterm_toml::from_str(&source).unwrap();
        assert_eq!(value["text"], char::from(code).to_string());
        assert_eq!(source.parse::<DocumentMut>().unwrap().to_string(), source);
        let encoded = aterm_toml::to_string(&value).unwrap();
        assert_eq!(
            aterm_toml::from_str::<BTreeMap<String, String>>(&encoded).unwrap(),
            value
        );
    }
    let source = "escaped = \"\\e\"\n\"\\x6Bey\" = \"\\xE9\"\nliteral = '\\e\\xE9'\nmulti = \"\"\"\\e\\x00\"\"\"\n";
    let value: BTreeMap<String, String> = aterm_toml::from_str(source).unwrap();
    assert_eq!(value["escaped"], "\u{1b}");
    assert_eq!(value["key"], "é");
    assert_eq!(value["literal"], r"\e\xE9");
    assert_eq!(value["multi"], "\u{1b}\0");
    assert_eq!(source.parse::<DocumentMut>().unwrap().to_string(), source);
}

#[test]
fn short_times_default_seconds_without_accepting_fractional_minutes() {
    for (short, explicit) in [
        ("07:32", "07:32:00"),
        ("00:00", "00:00:00"),
        ("23:59", "23:59:00"),
        ("2000-02-29T07:32", "2000-02-29T07:32:00"),
        ("1979-05-27 07:32Z", "1979-05-27T07:32:00Z"),
        ("1979-05-27T07:32-07:00", "1979-05-27T07:32:00-07:00"),
        ("1979-05-27t07:32+01:30", "1979-05-27T07:32:00+01:30"),
    ] {
        let source = format!("at = {short}\n");
        let expected: Datetime = explicit.parse().unwrap();
        assert_eq!(short.parse::<Datetime>().unwrap(), expected);
        let value: BTreeMap<String, Datetime> = aterm_toml::from_str(&source).unwrap();
        assert_eq!(value["at"], expected);
        assert_eq!(source.parse::<DocumentMut>().unwrap().to_string(), source);
        let encoded = aterm_toml::to_string(&value).unwrap();
        assert_eq!(
            aterm_toml::from_str::<BTreeMap<String, Datetime>>(&encoded).unwrap(),
            value
        );
    }
    for invalid in [
        "07:32.5",
        "07:32:",
        "07:3",
        "24:00",
        "07:60",
        "07:32Z",
        "2000-01-01T07:32.5Z",
        "2000-01-01T07:32+24:00",
        "2000-02-30T07:32",
    ] {
        assert!(invalid.parse::<Datetime>().is_err(), "{invalid}");
        assert!(
            aterm_toml::from_str::<Value>(&format!("at = {invalid}\n")).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn multiline_inline_tables_keep_comments_commas_and_unedited_bytes() {
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "item = {{{newline} # head{newline} first = 1, # after{newline} nested = {{{newline} value = \"\\xE9\",{newline} }},{newline} # tail{newline}}}{newline}"
        );
        let mut document: DocumentMut = source.parse().unwrap();
        assert_eq!(document.to_string(), source);
        let parsed: Value = aterm_toml::from_str(&source).unwrap();
        assert_eq!(parsed["item"]["first"].as_integer(), Some(1));
        assert_eq!(parsed["item"]["nested"]["value"].as_str(), Some("é"));
        let table = document["item"].as_table_like_mut().unwrap();
        let first = table.get_mut("first").unwrap().as_value_mut().unwrap();
        let decor = first.decor().clone();
        *first = EditValue::from(2i64);
        *first.decor_mut() = decor;
        let edited = document.to_string();
        assert_eq!(edited, source.replacen("first = 1", "first = 2", 1));
        assert_eq!(
            aterm_toml::from_str::<Value>(&edited).unwrap()["item"]["first"].as_integer(),
            Some(2)
        );
    }
    for source in [
        "t = { a = 1,\n b = 2 }\n",
        "t = { a = 1, }\n",
        "t = {\n # empty\n}\n",
        "t = { a = 1 # before comma\n, b = 2 # before close\n}\n",
    ] {
        assert!(aterm_toml::from_str::<Value>(source).is_ok(), "{source}");
        assert_eq!(source.parse::<DocumentMut>().unwrap().to_string(), source);
    }
}

#[test]
fn removing_the_last_inline_entry_does_not_leave_a_bare_comma() {
    let mut document: DocumentMut = "t = { a = 1, # end\n}\n".parse().unwrap();
    document["t"].as_table_like_mut().unwrap().remove("a");
    let encoded = document.to_string();
    assert!(encoded.contains("# end\n"));
    let value: Value = aterm_toml::from_str(&encoded).unwrap();
    assert!(value["t"].as_table().unwrap().is_empty());
}

#[test]
fn new_syntax_still_rejects_invalid_escapes_missing_commas_and_redefinitions() {
    for escape in [r"\x", r"\x0", r"\xgg", r"\x+1", r"\xé", r"\q"] {
        let source = format!("x = \"{escape}\"\n");
        assert!(aterm_toml::from_str::<Value>(&source).is_err(), "{source}");
        assert!(source.parse::<DocumentMut>().is_err(), "{source}");
    }
    for source in [
        "t = { a = 1\n b = 2 }\n",
        "t = { a = 1,, }\n",
        "t = {,}\n",
        "t = {\n a\n = 1 }\n",
        "t = {\n a =\n 1 }\n",
        "t = { a = # no value\n 1 }\n",
        "t = { a = 1, # no close\n",
        "t = { a = 1,\n \"\\x61\" = 2, }\n",
        "t = { a.b = 1,\n a.b = 2, }\n",
        "t = { a = 1, }\nt.b = 2\n",
    ] {
        assert!(aterm_toml::from_str::<Value>(source).is_err(), "{source}");
        assert!(source.parse::<DocumentMut>().is_err(), "{source}");
    }
}
