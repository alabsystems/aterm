// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_enum_internally_tagged.rs (git tag v1.0.228) and
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
use std::iter::FromIterator;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Unit;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Newtype(BTreeMap<String, String>);

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Struct {
    f: u8,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Enum {
    Unit,
    Newtype(u8),
    Tuple(u8, u8),
    Struct { f: u8 },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tag")]
enum InternallyTagged {
    Unit,
    NewtypeUnit(()),
    NewtypeUnitStruct(Unit),
    NewtypeNewtype(Newtype),
    NewtypeMap(BTreeMap<String, String>),
    NewtypeStruct(Struct),
    NewtypeEnum(Enum),
    Struct { a: u8 },
    StructEnum { enum_: Enum },
}

#[test]
fn unit() {
    assert_tokens(
        &InternallyTagged::Unit,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 1,
            },
            Token::Str("tag"),
            Token::Str("Unit"),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &InternallyTagged::Unit,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 1,
            },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("Unit"),
            Token::StructEnd,
        ],
    );

    assert_de_tokens(
        &InternallyTagged::Unit,
        &[
            Token::Map { len: Some(1) },
            Token::Str("tag"),
            Token::Str("Unit"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &InternallyTagged::Unit,
        &[
            Token::Map { len: Some(1) },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("Unit"),
            Token::MapEnd,
        ],
    );

    assert_de_tokens(
        &InternallyTagged::Unit,
        &[
            Token::Seq { len: Some(1) },
            Token::Str("Unit"), // tag
            Token::SeqEnd,
        ],
    );
    assert_de_tokens(
        &InternallyTagged::Unit,
        &[
            Token::Seq { len: Some(1) },
            Token::BorrowedStr("Unit"), // tag
            Token::SeqEnd,
        ],
    );
}

#[test]
fn newtype_unit() {
    let value = InternallyTagged::NewtypeUnit(());

    assert_tokens(
        &value,
        &[
            Token::Map { len: Some(1) },
            Token::Str("tag"),
            Token::Str("NewtypeUnit"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(1) },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeUnit"),
            Token::MapEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 1,
            },
            Token::Str("tag"),
            Token::Str("NewtypeUnit"),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 1,
            },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeUnit"),
            Token::StructEnd,
        ],
    );
}

#[test]
fn newtype_unit_struct() {
    let value = InternallyTagged::NewtypeUnitStruct(Unit);

    assert_tokens(
        &value,
        &[
            Token::Map { len: Some(1) },
            Token::Str("tag"),
            Token::Str("NewtypeUnitStruct"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(1) },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeUnitStruct"),
            Token::MapEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 1,
            },
            Token::Str("tag"),
            Token::Str("NewtypeUnitStruct"),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 1,
            },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeUnitStruct"),
            Token::StructEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Seq { len: Some(1) },
            Token::Str("NewtypeUnitStruct"), // tag
            Token::SeqEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Seq { len: Some(1) },
            Token::BorrowedStr("NewtypeUnitStruct"), // tag
            Token::SeqEnd,
        ],
    );
}

#[test]
fn newtype_newtype() {
    assert_tokens(
        &InternallyTagged::NewtypeNewtype(Newtype(BTreeMap::new())),
        &[
            Token::Map { len: Some(1) },
            Token::Str("tag"),
            Token::Str("NewtypeNewtype"),
            Token::MapEnd,
        ],
    );
}

#[test]
fn newtype_map() {
    let value = InternallyTagged::NewtypeMap(BTreeMap::new());

    assert_tokens(
        &value,
        &[
            Token::Map { len: Some(1) },
            Token::Str("tag"),
            Token::Str("NewtypeMap"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(1) },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeMap"),
            Token::MapEnd,
        ],
    );

    let value = InternallyTagged::NewtypeMap(BTreeMap::from_iter([(
        "field".to_string(),
        "value".to_string(),
    )]));

    assert_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::Str("tag"),
            Token::Str("NewtypeMap"),
            Token::Str("field"),
            Token::Str("value"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeMap"),
            Token::BorrowedStr("field"),
            Token::BorrowedStr("value"),
            Token::MapEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::Str("field"),
            Token::Str("value"),
            Token::Str("tag"),
            Token::Str("NewtypeMap"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::BorrowedStr("field"),
            Token::BorrowedStr("value"),
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeMap"),
            Token::MapEnd,
        ],
    );

    assert_de_tokens_error::<InternallyTagged>(
        &[
            Token::Seq { len: Some(2) },
            Token::Str("NewtypeMap"), // tag
            Token::Map { len: Some(0) },
            Token::MapEnd,
            Token::SeqEnd,
        ],
        "invalid type: sequence, expected a map",
    );
}

#[test]
fn newtype_struct() {
    let value = InternallyTagged::NewtypeStruct(Struct { f: 6 });

    assert_tokens(
        &value,
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::Str("tag"),
            Token::Str("NewtypeStruct"),
            Token::Str("f"),
            Token::U8(6),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeStruct"),
            Token::BorrowedStr("f"),
            Token::U8(6),
            Token::StructEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::Str("f"),
            Token::U8(6),
            Token::Str("tag"),
            Token::Str("NewtypeStruct"),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::BorrowedStr("f"),
            Token::U8(6),
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("NewtypeStruct"),
            Token::StructEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Seq { len: Some(2) },
            Token::Str("NewtypeStruct"), // tag
            Token::U8(6),
            Token::SeqEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Seq { len: Some(2) },
            Token::BorrowedStr("NewtypeStruct"), // tag
            Token::U8(6),
            Token::SeqEnd,
        ],
    );
}

mod newtype_enum {
    use super::*;

    #[test]
    fn unit() {
        let value = InternallyTagged::NewtypeEnum(Enum::Unit);

        assert_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::Str("Unit"),
                Token::Unit,
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::BorrowedStr("Unit"),
                Token::Unit,
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("Unit"),
                Token::Unit,
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("Unit"),
                Token::Unit,
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn newtype() {
        let value = InternallyTagged::NewtypeEnum(Enum::Newtype(1));

        assert_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::Str("Newtype"),
                Token::U8(1),
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::BorrowedStr("Newtype"),
                Token::U8(1),
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("Newtype"),
                Token::U8(1),
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("Newtype"),
                Token::U8(1),
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn tuple() {
        let value = InternallyTagged::NewtypeEnum(Enum::Tuple(1, 1));

        assert_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::Str("Tuple"),
                Token::TupleStruct {
                    name: "Tuple",
                    len: 2,
                },
                Token::U8(1),
                Token::U8(1),
                Token::TupleStructEnd,
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::BorrowedStr("Tuple"),
                Token::TupleStruct {
                    name: "Tuple",
                    len: 2,
                },
                Token::U8(1),
                Token::U8(1),
                Token::TupleStructEnd,
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("Tuple"),
                Token::TupleStruct {
                    name: "Tuple",
                    len: 2,
                },
                Token::U8(1),
                Token::U8(1),
                Token::TupleStructEnd,
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("Tuple"),
                Token::TupleStruct {
                    name: "Tuple",
                    len: 2,
                },
                Token::U8(1),
                Token::U8(1),
                Token::TupleStructEnd,
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn struct_() {
        let value = InternallyTagged::NewtypeEnum(Enum::Struct { f: 1 });

        assert_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::Str("Struct"),
                Token::Struct {
                    name: "Struct",
                    len: 1,
                },
                Token::Str("f"),
                Token::U8(1),
                Token::StructEnd,
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::BorrowedStr("Struct"),
                Token::Struct {
                    name: "Struct",
                    len: 1,
                },
                Token::BorrowedStr("f"),
                Token::U8(1),
                Token::StructEnd,
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("Struct"),
                Token::Struct {
                    name: "Struct",
                    len: 1,
                },
                Token::Str("f"),
                Token::U8(1),
                Token::StructEnd,
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("Struct"),
                Token::Struct {
                    name: "Struct",
                    len: 1,
                },
                Token::BorrowedStr("f"),
                Token::U8(1),
                Token::StructEnd,
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::Str("Struct"),
                Token::Seq { len: Some(1) },
                Token::U8(1), // f
                Token::SeqEnd,
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::BorrowedStr("Struct"),
                Token::Seq { len: Some(1) },
                Token::U8(1), // f
                Token::SeqEnd,
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("Struct"),
                Token::Seq { len: Some(1) },
                Token::U8(1), // f
                Token::SeqEnd,
                Token::Str("tag"),
                Token::Str("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("Struct"),
                Token::Seq { len: Some(1) },
                Token::U8(1), // f
                Token::SeqEnd,
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("NewtypeEnum"),
                Token::MapEnd,
            ],
        );
    }
}

#[test]
fn struct_() {
    let value = InternallyTagged::Struct { a: 1 };

    assert_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 2,
            },
            Token::Str("tag"),
            Token::Str("Struct"),
            Token::Str("a"),
            Token::U8(1),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 2,
            },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("Struct"),
            Token::BorrowedStr("a"),
            Token::U8(1),
            Token::StructEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 2,
            },
            Token::Str("a"),
            Token::U8(1),
            Token::Str("tag"),
            Token::Str("Struct"),
            Token::StructEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 2,
            },
            Token::BorrowedStr("a"),
            Token::U8(1),
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("Struct"),
            Token::StructEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::Str("tag"),
            Token::Str("Struct"),
            Token::Str("a"),
            Token::U8(1),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("Struct"),
            Token::BorrowedStr("a"),
            Token::U8(1),
            Token::MapEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::Str("a"),
            Token::U8(1),
            Token::Str("tag"),
            Token::Str("Struct"),
            Token::MapEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Map { len: Some(2) },
            Token::BorrowedStr("a"),
            Token::U8(1),
            Token::BorrowedStr("tag"),
            Token::BorrowedStr("Struct"),
            Token::MapEnd,
        ],
    );

    assert_de_tokens(
        &value,
        &[
            Token::Seq { len: Some(2) },
            Token::Str("Struct"), // tag
            Token::U8(1),
            Token::SeqEnd,
        ],
    );
    assert_de_tokens(
        &value,
        &[
            Token::Seq { len: Some(2) },
            Token::BorrowedStr("Struct"), // tag
            Token::U8(1),
            Token::SeqEnd,
        ],
    );
}

mod struct_enum {
    use super::*;

    #[test]
    fn unit() {
        assert_de_tokens(
            &Enum::Unit,
            &[
                Token::Enum { name: "Enum" },
                Token::BorrowedStr("Unit"),
                Token::Unit,
            ],
        );

        let value = InternallyTagged::StructEnum { enum_: Enum::Unit };

        assert_tokens(
            &value,
            &[
                Token::Struct {
                    name: "InternallyTagged",
                    len: 2,
                },
                Token::Str("tag"),
                Token::Str("StructEnum"),
                Token::Str("enum_"),
                Token::Enum { name: "Enum" },
                Token::Str("Unit"),
                Token::Unit,
                Token::StructEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Struct {
                    name: "InternallyTagged",
                    len: 2,
                },
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("StructEnum"),
                Token::BorrowedStr("enum_"),
                Token::Enum { name: "Enum" },
                Token::BorrowedStr("Unit"),
                Token::Unit,
                Token::StructEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Struct {
                    name: "InternallyTagged",
                    len: 2,
                },
                Token::Str("enum_"),
                Token::Enum { name: "Enum" },
                Token::Str("Unit"),
                Token::Unit,
                Token::Str("tag"),
                Token::Str("StructEnum"),
                Token::StructEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Struct {
                    name: "InternallyTagged",
                    len: 2,
                },
                Token::BorrowedStr("enum_"),
                Token::Enum { name: "Enum" },
                Token::BorrowedStr("Unit"),
                Token::Unit,
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("StructEnum"),
                Token::StructEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("tag"),
                Token::Str("StructEnum"),
                Token::Str("enum_"),
                Token::Enum { name: "Enum" },
                Token::Str("Unit"),
                Token::Unit,
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("StructEnum"),
                Token::BorrowedStr("enum_"),
                Token::Enum { name: "Enum" },
                Token::BorrowedStr("Unit"),
                Token::Unit,
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::Str("enum_"),
                Token::Enum { name: "Enum" },
                Token::Str("Unit"),
                Token::Unit,
                Token::Str("tag"),
                Token::Str("StructEnum"),
                Token::MapEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Map { len: Some(2) },
                Token::BorrowedStr("enum_"),
                Token::Enum { name: "Enum" },
                Token::BorrowedStr("Unit"),
                Token::Unit,
                Token::BorrowedStr("tag"),
                Token::BorrowedStr("StructEnum"),
                Token::MapEnd,
            ],
        );

        assert_de_tokens(
            &value,
            &[
                Token::Seq { len: Some(2) },
                Token::Str("StructEnum"),     // tag
                Token::Enum { name: "Enum" }, // enum_
                Token::Str("Unit"),
                Token::Unit,
                Token::SeqEnd,
            ],
        );
        assert_de_tokens(
            &value,
            &[
                Token::Seq { len: Some(2) },
                Token::BorrowedStr("StructEnum"), // tag
                Token::Enum { name: "Enum" },     // enum_
                Token::BorrowedStr("Unit"),
                Token::Unit,
                Token::SeqEnd,
            ],
        );
    }
}

#[test]
fn wrong_tag() {
    assert_de_tokens_error::<InternallyTagged>(
        &[Token::Map { len: Some(0) }, Token::MapEnd],
        "missing field `tag`",
    );

    assert_de_tokens_error::<InternallyTagged>(
        &[
            Token::Map { len: Some(1) },
            Token::Str("tag"),
            Token::Str("Z"),
            Token::MapEnd,
        ],
        "unknown variant `Z`, expected one of \
        `Unit`, \
        `NewtypeUnit`, \
        `NewtypeUnitStruct`, \
        `NewtypeNewtype`, \
        `NewtypeMap`, \
        `NewtypeStruct`, \
        `NewtypeEnum`, \
        `Struct`, \
        `StructEnum`",
    );
}

#[test]
fn with_skipped_field() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "tag")]
    enum Data {
        A,
        C {
            #[serde(skip)]
            t: String,
        },
    }

    let data = Data::C { t: String::new() };

    assert_tokens(
        &data,
        &[
            Token::Struct {
                name: "Data",
                len: 1,
            },
            Token::Str("tag"),
            Token::Str("C"),
            Token::StructEnd,
        ],
    );
}

#[test]
fn containing_flatten() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "tag")]
    enum Data {
        A {
            a: i32,
            #[serde(flatten)]
            flat: Flat,
        },
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
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
            Token::Str("tag"),
            Token::Str("A"),
            Token::Str("a"),
            Token::I32(0),
            Token::Str("b"),
            Token::I32(0),
            Token::MapEnd,
        ],
    );
}

#[test]
fn unit_variant_with_unknown_fields() {
    let value = InternallyTagged::Unit;

    assert_de_tokens(
        &value,
        &[
            Token::Map { len: None },
            Token::Str("tag"),
            Token::Str("Unit"),
            Token::Str("b"),
            Token::I32(0),
            Token::MapEnd,
        ],
    );

    assert_de_tokens_error::<InternallyTagged>(
        &[
            Token::Seq { len: None },
            Token::Str("Unit"), // tag
            Token::I32(0),
            Token::SeqEnd,
        ],
        "invalid length 1, expected 0 elements in sequence",
    );
}
