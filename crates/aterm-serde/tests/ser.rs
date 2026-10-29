// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_ser.rs (git tag v1.0.228) and
// modified by the aterm project in 2026: ported onto aterm-serde's surface.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

#![allow(clippy::derive_partial_eq_without_eq, clippy::unreadable_literal)]

use crate::support::{Token, assert_ser_tokens, assert_ser_tokens_error};
use serde_derive::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

//////////////////////////////////////////////////////////////////////////

#[derive(Serialize)]
struct UnitStruct;

#[derive(Serialize)]
struct TupleStruct(i32, i32, i32);

#[derive(Serialize)]
struct Struct {
    a: i32,
    b: i32,
    c: i32,
}

#[derive(PartialEq, Debug)]
struct NotSerializable;

#[derive(Serialize, PartialEq, Debug)]
enum Enum {
    Unit,
    One(i32),
    Seq(i32, i32),
    Map { a: i32, b: i32 },
    OneWithSkipped(#[serde(skip)] NotSerializable),
}

//////////////////////////////////////////////////////////////////////////

#[test]
fn test_unit() {
    assert_ser_tokens(&(), &[Token::Unit]);
}

#[test]
fn test_bool() {
    assert_ser_tokens(&true, &[Token::Bool(true)]);
    assert_ser_tokens(&false, &[Token::Bool(false)]);
}

#[test]
fn test_isizes() {
    assert_ser_tokens(&0i8, &[Token::I8(0)]);
    assert_ser_tokens(&0i16, &[Token::I16(0)]);
    assert_ser_tokens(&0i32, &[Token::I32(0)]);
    assert_ser_tokens(&0i64, &[Token::I64(0)]);
}

#[test]
fn test_usizes() {
    assert_ser_tokens(&0u8, &[Token::U8(0)]);
    assert_ser_tokens(&0u16, &[Token::U16(0)]);
    assert_ser_tokens(&0u32, &[Token::U32(0)]);
    assert_ser_tokens(&0u64, &[Token::U64(0)]);
}

#[test]
fn test_floats() {
    assert_ser_tokens(&0f32, &[Token::F32(0.)]);
    assert_ser_tokens(&0f64, &[Token::F64(0.)]);
}

#[test]
fn test_char() {
    assert_ser_tokens(&'a', &[Token::Char('a')]);
}

#[test]
fn test_str() {
    assert_ser_tokens(&"abc", &[Token::Str("abc")]);
    assert_ser_tokens(&"abc".to_owned(), &[Token::Str("abc")]);
}

#[test]
fn test_option() {
    assert_ser_tokens(&None::<i32>, &[Token::None]);
    assert_ser_tokens(&Some(1), &[Token::Some, Token::I32(1)]);
}

#[test]
fn test_slice() {
    assert_ser_tokens(&[0][..0], &[Token::Seq { len: Some(0) }, Token::SeqEnd]);
    assert_ser_tokens(
        &[1, 2, 3][..],
        &[
            Token::Seq { len: Some(3) },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::SeqEnd,
        ],
    );
}

#[test]
fn test_array() {
    assert_ser_tokens(&[0; 0], &[Token::Tuple { len: 0 }, Token::TupleEnd]);
    assert_ser_tokens(
        &[1, 2, 3],
        &[
            Token::Tuple { len: 3 },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::TupleEnd,
        ],
    );
}

#[test]
fn test_vec() {
    assert_ser_tokens(
        &Vec::<isize>::new(),
        &[Token::Seq { len: Some(0) }, Token::SeqEnd],
    );
    assert_ser_tokens(
        &vec![vec![], vec![1], vec![2, 3]],
        &[
            Token::Seq { len: Some(3) },
            Token::Seq { len: Some(0) },
            Token::SeqEnd,
            Token::Seq { len: Some(1) },
            Token::I32(1),
            Token::SeqEnd,
            Token::Seq { len: Some(2) },
            Token::I32(2),
            Token::I32(3),
            Token::SeqEnd,
            Token::SeqEnd,
        ],
    );
}

#[test]
fn test_tuple() {
    assert_ser_tokens(
        &(1,),
        &[Token::Tuple { len: 1 }, Token::I32(1), Token::TupleEnd],
    );
    assert_ser_tokens(
        &(1, 2, 3),
        &[
            Token::Tuple { len: 3 },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::TupleEnd,
        ],
    );
}

#[test]
fn test_btreemap() {
    assert_ser_tokens(
        &btreemap![1 => 2],
        &[
            Token::Map { len: Some(1) },
            Token::I32(1),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    assert_ser_tokens(
        &btreemap![1 => 2, 3 => 4],
        &[
            Token::Map { len: Some(2) },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::I32(4),
            Token::MapEnd,
        ],
    );
    assert_ser_tokens(
        &btreemap![1 => btreemap![], 2 => btreemap![3 => 4, 5 => 6]],
        &[
            Token::Map { len: Some(2) },
            Token::I32(1),
            Token::Map { len: Some(0) },
            Token::MapEnd,
            Token::I32(2),
            Token::Map { len: Some(2) },
            Token::I32(3),
            Token::I32(4),
            Token::I32(5),
            Token::I32(6),
            Token::MapEnd,
            Token::MapEnd,
        ],
    );
}

#[test]
fn test_unit_struct() {
    assert_ser_tokens(&UnitStruct, &[Token::UnitStruct { name: "UnitStruct" }]);
}

#[test]
fn test_tuple_struct() {
    assert_ser_tokens(
        &TupleStruct(1, 2, 3),
        &[
            Token::TupleStruct {
                name: "TupleStruct",
                len: 3,
            },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn test_struct() {
    assert_ser_tokens(
        &Struct { a: 1, b: 2, c: 3 },
        &[
            Token::Struct {
                name: "Struct",
                len: 3,
            },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::Str("c"),
            Token::I32(3),
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_enum() {
    assert_ser_tokens(
        &Enum::Unit,
        &[Token::UnitVariant {
            name: "Enum",
            variant: "Unit",
        }],
    );
    assert_ser_tokens(
        &Enum::One(42),
        &[
            Token::NewtypeVariant {
                name: "Enum",
                variant: "One",
            },
            Token::I32(42),
        ],
    );
    assert_ser_tokens(
        &Enum::Seq(1, 2),
        &[
            Token::TupleVariant {
                name: "Enum",
                variant: "Seq",
                len: 2,
            },
            Token::I32(1),
            Token::I32(2),
            Token::TupleVariantEnd,
        ],
    );
    assert_ser_tokens(
        &Enum::Map { a: 1, b: 2 },
        &[
            Token::StructVariant {
                name: "Enum",
                variant: "Map",
                len: 2,
            },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::StructVariantEnd,
        ],
    );
    assert_ser_tokens(
        &Enum::OneWithSkipped(NotSerializable),
        &[Token::UnitVariant {
            name: "Enum",
            variant: "OneWithSkipped",
        }],
    );
}

#[test]
fn test_box() {
    assert_ser_tokens(&Box::new(0i32), &[Token::I32(0)]);
}

#[test]
fn test_boxed_array() {
    assert_ser_tokens(
        &Box::new([0, 1, 2]),
        &[
            Token::Tuple { len: 3 },
            Token::I32(0),
            Token::I32(1),
            Token::I32(2),
            Token::TupleEnd,
        ],
    );
}

#[test]
fn test_path_buf() {
    assert_ser_tokens(
        &PathBuf::from("/usr/local/lib"),
        &[Token::Str("/usr/local/lib")],
    );
}

#[test]
#[cfg(unix)]
fn test_cannot_serialize_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    assert_ser_tokens_error(
        &Path::new(OsStr::from_bytes(b"Hello \xF0\x90\x80World")),
        &[],
        "path contains invalid UTF-8 characters",
    );
}

#[test]
fn test_integer128() {
    assert_ser_tokens_error(&1i128, &[], "i128 is not supported");

    assert_ser_tokens_error(&1u128, &[], "u128 is not supported");
}
