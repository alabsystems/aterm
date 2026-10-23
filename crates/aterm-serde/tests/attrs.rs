// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the `#[serde]` attribute forms used by aterm models.

use crate::support::{
    Token, assert_de_tokens, assert_de_tokens_error, assert_ser_tokens, assert_tokens,
};
use aterm_serde::Deserializer;
use aterm_serde::de::{Deserialize, Error};
use serde_derive::{Deserialize, Serialize};

fn seven() -> u32 {
    7
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct FieldDefaults {
    #[serde(default)]
    plain: u32,
    #[serde(default = "seven")]
    path: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    maybe: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    list: Vec<u8>,
}

#[test]
fn field_default_bare_and_path_fill_missing_keys() {
    assert_de_tokens(
        &FieldDefaults {
            plain: 0,
            path: 7,
            maybe: None,
            list: Vec::new(),
        },
        &[
            Token::Struct {
                name: "FieldDefaults",
                len: 0,
            },
            Token::StructEnd,
        ],
    );
}

#[test]
fn skip_serializing_if_drops_the_field_and_its_length_slot() {
    assert_ser_tokens(
        &FieldDefaults {
            plain: 1,
            path: 2,
            maybe: None,
            list: Vec::new(),
        },
        &[
            Token::Struct {
                name: "FieldDefaults",
                len: 2,
            },
            Token::Str("plain"),
            Token::U32(1),
            Token::Str("path"),
            Token::U32(2),
            Token::StructEnd,
        ],
    );
    assert_tokens(
        &FieldDefaults {
            plain: 1,
            path: 2,
            maybe: Some("m".to_owned()),
            list: vec![3],
        },
        &[
            Token::Struct {
                name: "FieldDefaults",
                len: 4,
            },
            Token::Str("plain"),
            Token::U32(1),
            Token::Str("path"),
            Token::U32(2),
            Token::Str("maybe"),
            Token::Some,
            Token::Str("m"),
            Token::Str("list"),
            Token::Seq { len: Some(1) },
            Token::U8(3),
            Token::SeqEnd,
            Token::StructEnd,
        ],
    );
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Strict {
    a: u32,
    b: String,
}

impl Default for Strict {
    fn default() -> Self {
        Strict {
            a: 5,
            b: "five".to_owned(),
        }
    }
}

#[test]
fn container_default_fills_from_the_type_and_unknown_keys_still_fail() {
    assert_de_tokens(
        &Strict {
            a: 9,
            b: "five".to_owned(),
        },
        &[
            Token::Struct {
                name: "Strict",
                len: 1,
            },
            Token::Str("a"),
            Token::U32(9),
            Token::StructEnd,
        ],
    );
    assert_de_tokens_error::<Strict>(
        &[
            Token::Struct {
                name: "Strict",
                len: 1,
            },
            Token::Str("c"),
        ],
        "unknown field `c`, expected `a` or `b`",
    );
}

#[test]
fn a_missing_field_without_default_names_the_field() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Needs {
        a: u32,
        b: Option<u32>,
    }
    assert_de_tokens(
        &Needs { a: 1, b: None },
        &[
            Token::Struct {
                name: "Needs",
                len: 1,
            },
            Token::Str("a"),
            Token::U32(1),
            Token::StructEnd,
        ],
    );
    assert_de_tokens_error::<Needs>(
        &[
            Token::Struct {
                name: "Needs",
                len: 0,
            },
            Token::StructEnd,
        ],
        "missing field `a`",
    );
}

#[test]
fn a_struct_reads_positionally_from_a_sequence() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Pair {
        a: u32,
        b: u32,
    }
    assert_de_tokens(
        &Pair { a: 1, b: 2 },
        &[
            Token::Seq { len: Some(2) },
            Token::U32(1),
            Token::U32(2),
            Token::SeqEnd,
        ],
    );
    assert_de_tokens_error::<Pair>(
        &[Token::Seq { len: Some(1) }, Token::U32(1), Token::SeqEnd],
        "invalid length 1, expected struct Pair with 2 elements",
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Snake {
    FirstOne,
    #[serde(rename = "renamed")]
    SecondOne,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Pascal {
    field_one: u8,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Kebab {
    field_one: u8,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Lower {
    FirstOne,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum Upper {
    FirstOne,
}

#[test]
fn rename_all_cases_aterm_uses() {
    assert_tokens(
        &Snake::FirstOne,
        &[Token::UnitVariant {
            name: "Snake",
            variant: "first_one",
        }],
    );
    assert_tokens(
        &Snake::SecondOne,
        &[Token::UnitVariant {
            name: "Snake",
            variant: "renamed",
        }],
    );
    assert_tokens(
        &Pascal { field_one: 1 },
        &[
            Token::Struct {
                name: "Pascal",
                len: 1,
            },
            Token::Str("FieldOne"),
            Token::U8(1),
            Token::StructEnd,
        ],
    );
    assert_tokens(
        &Kebab { field_one: 1 },
        &[
            Token::Struct {
                name: "Kebab",
                len: 1,
            },
            Token::Str("field-one"),
            Token::U8(1),
            Token::StructEnd,
        ],
    );
    assert_tokens(
        &Lower::FirstOne,
        &[Token::UnitVariant {
            name: "Lower",
            variant: "firstone",
        }],
    );
    assert_tokens(
        &Upper::FirstOne,
        &[Token::UnitVariant {
            name: "Upper",
            variant: "FIRSTONE",
        }],
    );
    assert_de_tokens_error::<Snake>(
        &[Token::UnitVariant {
            name: "Snake",
            variant: "FirstOne",
        }],
        "unknown variant `FirstOne`, expected `first_one` or `renamed`",
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
struct Id(u64);

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
struct Named {
    inner: String,
}

#[test]
fn transparent_is_the_inner_value() {
    assert_tokens(&Id(42), &[Token::U64(42)]);
    assert_tokens(
        &Named {
            inner: "x".to_owned(),
        },
        &[Token::Str("x")],
    );
}

fn lenient_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    struct Lenient;
    impl<'de> aterm_serde::de::Visitor<'de> for Lenient {
        type Value = u32;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("anything")
        }
        fn visit_u32<E: Error>(self, v: u32) -> Result<u32, E> {
            Ok(v)
        }
        fn visit_str<E: Error>(self, _: &str) -> Result<u32, E> {
            Ok(0)
        }
    }
    deserializer.deserialize_any(Lenient)
}

fn strict_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(deserializer)?;
    if v > 10 {
        return Err(D::Error::custom(format_args!("{v} is over ten")));
    }
    Ok(v)
}

#[derive(Debug, PartialEq, Deserialize)]
struct With {
    #[serde(default, deserialize_with = "lenient_u32")]
    lenient: u32,
    #[serde(deserialize_with = "strict_u32")]
    strict: u32,
}

#[test]
fn deserialize_with_runs_the_named_function() {
    assert_de_tokens(
        &With {
            lenient: 0,
            strict: 3,
        },
        &[
            Token::Struct {
                name: "With",
                len: 2,
            },
            Token::Str("lenient"),
            Token::Str("not a number"),
            Token::Str("strict"),
            Token::U32(3),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &With {
            lenient: 0,
            strict: 4,
        },
        &[
            Token::Struct {
                name: "With",
                len: 1,
            },
            Token::Str("strict"),
            Token::U32(4),
            Token::StructEnd,
        ],
    );
    assert_de_tokens_error::<With>(
        &[
            Token::Struct {
                name: "With",
                len: 1,
            },
            Token::Str("strict"),
            Token::U32(11),
        ],
        "11 is over ten",
    );
}
