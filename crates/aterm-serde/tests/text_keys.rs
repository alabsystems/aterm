// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests that buffered map keys (`#[serde(flatten)]`, untagged, and internally tagged) parse string-encoded integer and `bool` keys.

use crate::support::{Token, assert_de_tokens, assert_de_tokens_error};
use serde_derive::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Deserialize)]
struct Flat {
    name: String,
    #[serde(flatten)]
    ids: BTreeMap<u32, String>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum Untagged {
    Ids(BTreeMap<i64, String>),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "kind")]
enum Tagged {
    A { flags: BTreeMap<bool, u8> },
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
struct Id(u128);

#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum Wrapped {
    Ids(BTreeMap<Id, u8>, BTreeMap<Option<i8>, u8>),
}

#[test]
fn a_buffered_text_key_reads_back_as_what_it_spells() {
    assert_de_tokens(
        &Flat {
            name: "a".to_owned(),
            ids: BTreeMap::from([(7, "x".to_owned()), (0, "y".to_owned())]),
        },
        &[
            Token::Map { len: None },
            Token::Str("name"),
            Token::Str("a"),
            Token::Str("7"),
            Token::Str("x"),
            Token::BorrowedStr("0"),
            Token::Str("y"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &Untagged::Ids(BTreeMap::from([
            (-3, "x".to_owned()),
            (i64::MAX, "y".to_owned()),
        ])),
        &[
            Token::Map { len: None },
            Token::String("-3"),
            Token::Str("x"),
            Token::Str("9223372036854775807"),
            Token::Str("y"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &Tagged::A {
            flags: BTreeMap::from([(true, 1), (false, 0)]),
        },
        &[
            Token::Map { len: None },
            Token::Str("kind"),
            Token::Str("A"),
            Token::Str("flags"),
            Token::Map { len: None },
            Token::Str("true"),
            Token::U8(1),
            Token::Str("false"),
            Token::U8(0),
            Token::MapEnd,
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &Wrapped::Ids(
            BTreeMap::from([(Id(u128::MAX), 1)]),
            BTreeMap::from([(Some(-128), 2)]),
        ),
        &[
            Token::Seq { len: Some(2) },
            Token::Map { len: None },
            Token::Str("340282366920938463463374607431768211455"),
            Token::U8(1),
            Token::MapEnd,
            Token::Map { len: None },
            Token::Str("-128"),
            Token::U8(2),
            Token::MapEnd,
            Token::SeqEnd,
        ],
    );
    assert_de_tokens(
        &Flat {
            name: "a".to_owned(),
            ids: BTreeMap::from([(7, "x".to_owned())]),
        },
        &[
            Token::Map { len: None },
            Token::Str("name"),
            Token::Str("a"),
            Token::U32(7),
            Token::Str("x"),
            Token::MapEnd,
        ],
    );
}

/// The buffer reads an integer or `bool` key from the one spelling a writer
/// gives it and no other: the rest reach the key type as the strings they
/// are, which it refuses, and the visitor's own range check refuses a key too
/// wide for its type.
#[test]
fn a_buffered_text_key_reads_back_only_from_its_one_spelling() {
    for (key, error) in [
        ("007", r#"invalid type: string "007", expected u32"#),
        ("+7", r#"invalid type: string "+7", expected u32"#),
        ("-0", r#"invalid type: string "-0", expected u32"#),
        (" 7", r#"invalid type: string " 7", expected u32"#),
        ("", r#"invalid type: string "", expected u32"#),
        ("-1", "invalid value: integer `-1`, expected u32"),
        (
            "4294967296",
            "invalid value: integer `4294967296`, expected u32",
        ),
    ] {
        let tokens = [
            Token::Map { len: None },
            Token::Str("name"),
            Token::Str("a"),
            Token::Str(key),
            Token::Str("x"),
            Token::MapEnd,
        ];
        assert_de_tokens_error::<Flat>(&tokens, error);
    }
    assert_de_tokens_error::<Tagged>(
        &[
            Token::Map { len: None },
            Token::Str("kind"),
            Token::Str("A"),
            Token::Str("flags"),
            Token::Map { len: None },
            Token::Str("True"),
            Token::U8(1),
            Token::MapEnd,
            Token::MapEnd,
        ],
        r#"invalid type: string "True", expected a boolean"#,
    );
}
