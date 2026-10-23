// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_enum_untagged.rs (git tag v1.0.228) and
// modified by the aterm project in 2026: ported onto aterm-serde's surface.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

#![deny(trivial_numeric_casts)]
#![allow(
    clippy::derive_partial_eq_without_eq,
    clippy::enum_variant_names,
    clippy::redundant_field_names,
    clippy::too_many_lines
)]

use crate::support::{Token, assert_de_tokens, assert_de_tokens_error, assert_tokens};
use serde_derive::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[test]
fn complex() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    enum Untagged {
        A { a: u8 },
        B { b: u8 },
        C,
        D(u8),
        E(String),
        F(u8, u8),
    }

    assert_tokens(
        &Untagged::A { a: 1 },
        &[
            Token::Struct {
                name: "Untagged",
                len: 1,
            },
            Token::Str("a"),
            Token::U8(1),
            Token::StructEnd,
        ],
    );

    assert_tokens(
        &Untagged::B { b: 2 },
        &[
            Token::Struct {
                name: "Untagged",
                len: 1,
            },
            Token::Str("b"),
            Token::U8(2),
            Token::StructEnd,
        ],
    );

    assert_tokens(&Untagged::C, &[Token::Unit]);
    assert_de_tokens(&Untagged::C, &[Token::None]);

    assert_tokens(&Untagged::D(4), &[Token::U8(4)]);
    assert_tokens(&Untagged::E("e".to_owned()), &[Token::Str("e")]);

    assert_tokens(
        &Untagged::F(1, 2),
        &[
            Token::Tuple { len: 2 },
            Token::U8(1),
            Token::U8(2),
            Token::TupleEnd,
        ],
    );

    assert_de_tokens_error::<Untagged>(
        &[Token::Tuple { len: 1 }, Token::U8(1), Token::TupleEnd],
        "data did not match any variant of untagged enum Untagged",
    );

    assert_de_tokens_error::<Untagged>(
        &[
            Token::Tuple { len: 3 },
            Token::U8(1),
            Token::U8(2),
            Token::U8(3),
            Token::TupleEnd,
        ],
        "data did not match any variant of untagged enum Untagged",
    );
}

#[test]
fn newtype_unit_and_empty_map() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Unit;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    enum Message {
        Unit(Unit),
        Map(BTreeMap<String, String>),
    }

    assert_tokens(
        &Message::Map(BTreeMap::new()),
        &[Token::Map { len: Some(0) }, Token::MapEnd],
    );
}

#[test]
fn newtype_struct() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct NewtypeStruct(u32);

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    enum E {
        Newtype(NewtypeStruct),
        Null,
    }

    let value = E::Newtype(NewtypeStruct(5));

    assert_tokens(
        &value,
        &[
            Token::NewtypeStruct {
                name: "NewtypeStruct",
            },
            Token::U32(5),
        ],
    );

    assert_de_tokens(&value, &[Token::U32(5)]);
}

mod newtype_enum {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    enum Outer {
        Inner(Inner),
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Inner {
        Unit,
        Newtype(u8),
        Tuple0(),
        Tuple2(u8, u8),
        Struct { f: u8 },
        EmptyStruct {},
    }

    #[test]
    fn unit() {
        assert_tokens(
            &Outer::Inner(Inner::Unit),
            &[Token::UnitVariant {
                name: "Inner",
                variant: "Unit",
            }],
        );
    }

    #[test]
    fn newtype() {
        assert_tokens(
            &Outer::Inner(Inner::Newtype(1)),
            &[
                Token::NewtypeVariant {
                    name: "Inner",
                    variant: "Newtype",
                },
                Token::U8(1),
            ],
        );
    }

    #[test]
    fn tuple0() {
        assert_tokens(
            &Outer::Inner(Inner::Tuple0()),
            &[
                Token::TupleVariant {
                    name: "Inner",
                    variant: "Tuple0",
                    len: 0,
                },
                Token::TupleVariantEnd,
            ],
        );
    }

    #[test]
    fn tuple2() {
        assert_tokens(
            &Outer::Inner(Inner::Tuple2(1, 1)),
            &[
                Token::TupleVariant {
                    name: "Inner",
                    variant: "Tuple2",
                    len: 2,
                },
                Token::U8(1),
                Token::U8(1),
                Token::TupleVariantEnd,
            ],
        );
    }

    #[test]
    fn struct_from_map() {
        assert_tokens(
            &Outer::Inner(Inner::Struct { f: 1 }),
            &[
                Token::StructVariant {
                    name: "Inner",
                    variant: "Struct",
                    len: 1,
                },
                Token::Str("f"),
                Token::U8(1),
                Token::StructVariantEnd,
            ],
        );
    }

    #[test]
    fn struct_from_seq() {
        assert_de_tokens(
            &Outer::Inner(Inner::Struct { f: 1 }),
            &[
                Token::Map { len: Some(1) },
                Token::Str("Struct"),
                Token::Seq { len: Some(1) },
                Token::U8(1),
                Token::SeqEnd,
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn empty_struct_from_map() {
        assert_de_tokens(
            &Outer::Inner(Inner::EmptyStruct {}),
            &[
                Token::Map { len: Some(1) },
                Token::Str("EmptyStruct"),
                Token::Map { len: Some(0) },
                Token::MapEnd,
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn empty_struct_from_seq() {
        assert_de_tokens(
            &Outer::Inner(Inner::EmptyStruct {}),
            &[
                Token::Map { len: Some(1) },
                Token::Str("EmptyStruct"),
                Token::Seq { len: Some(0) },
                Token::SeqEnd,
                Token::MapEnd,
            ],
        );
    }
}

mod with_optional_field {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    enum Enum {
        Struct { optional: Option<u32> },
        Null,
    }

    #[test]
    fn some() {
        assert_tokens(
            &Enum::Struct { optional: Some(42) },
            &[
                Token::Struct {
                    name: "Enum",
                    len: 1,
                },
                Token::Str("optional"),
                Token::Some,
                Token::U32(42),
                Token::StructEnd,
            ],
        );
    }

    #[test]
    fn some_without_marker() {
        assert_de_tokens(
            &Enum::Struct { optional: Some(42) },
            &[
                Token::Struct {
                    name: "Enum",
                    len: 1,
                },
                Token::Str("optional"),
                Token::U32(42),
                Token::StructEnd,
            ],
        );
    }

    #[test]
    fn none() {
        assert_tokens(
            &Enum::Struct { optional: None },
            &[
                Token::Struct {
                    name: "Enum",
                    len: 1,
                },
                Token::Str("optional"),
                Token::None,
                Token::StructEnd,
            ],
        );
    }

    #[test]
    fn unit() {
        assert_de_tokens(
            &Enum::Struct { optional: None },
            &[
                Token::Map { len: None },
                Token::Str("optional"),
                Token::Unit,
                Token::MapEnd,
            ],
        );
    }
}

#[test]
fn contains_flatten() {
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    #[serde(untagged)]
    enum Data {
        A {
            a: i32,
            #[serde(flatten)]
            flat: Flat,
        },
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Flat {
        b: i32,
    }

    let data = Data::A {
        a: 0,
        flat: Flat { b: 0 },
    };

    assert_tokens(
        &data,
        &[
            Token::Map { len: None },
            Token::Str("a"),
            Token::I32(0),
            Token::Str("b"),
            Token::I32(0),
            Token::MapEnd,
        ],
    );
}

#[test]
fn contains_flatten_with_integer_key() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum Untagged {
        Variant {
            #[serde(flatten)]
            map: BTreeMap<u64, String>,
        },
    }

    assert_tokens(
        &Untagged::Variant {
            map: {
                let mut map = BTreeMap::new();
                map.insert(100, "BTreeMap".to_owned());
                map
            },
        },
        &[
            Token::Map { len: None },
            Token::U64(100),
            Token::Str("BTreeMap"),
            Token::MapEnd,
        ],
    );
}
