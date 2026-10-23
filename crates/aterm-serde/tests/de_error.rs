// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_de_error.rs (git tag v1.0.228) and
// modified by the aterm project in 2026: ported onto aterm-serde's surface.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

#![allow(
    clippy::derive_partial_eq_without_eq,
    clippy::empty_enums,
    clippy::unreadable_literal
)]

use crate::support::{Token, assert_de_tokens_error};
use serde_derive::Deserialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Copy, Clone, PartialEq, Debug, Deserialize)]
struct UnitStruct;

#[derive(PartialEq, Debug, Deserialize)]
struct Struct {
    a: i32,
    b: i32,
    #[serde(skip)]
    c: i32,
}

#[derive(PartialEq, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StructDenyUnknown {
    a: i32,
    #[serde(skip)]
    b: i32,
}

#[derive(PartialEq, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StructSkipAllDenyUnknown {
    #[serde(skip)]
    a: i32,
}

#[derive(Default, PartialEq, Debug)]
struct NotDeserializable;

#[derive(PartialEq, Debug, Deserialize)]
enum Enum {
    Unit,
    Simple(i32),
    Seq(i32, i32, i32),
    Map { a: i32, b: i32, c: i32 },
    SimpleWithSkipped(#[serde(skip)] NotDeserializable),
}

#[test]
fn test_i8() {
    let test = assert_de_tokens_error::<i8>;

    test(
        &[Token::I16(-129)],
        "invalid value: integer `-129`, expected i8",
    );
    test(
        &[Token::I32(-129)],
        "invalid value: integer `-129`, expected i8",
    );
    test(
        &[Token::I64(-129)],
        "invalid value: integer `-129`, expected i8",
    );
    test(
        &[Token::I16(128)],
        "invalid value: integer `128`, expected i8",
    );
    test(
        &[Token::I32(128)],
        "invalid value: integer `128`, expected i8",
    );
    test(
        &[Token::I64(128)],
        "invalid value: integer `128`, expected i8",
    );

    test(
        &[Token::U8(128)],
        "invalid value: integer `128`, expected i8",
    );
    test(
        &[Token::U16(128)],
        "invalid value: integer `128`, expected i8",
    );
    test(
        &[Token::U32(128)],
        "invalid value: integer `128`, expected i8",
    );
    test(
        &[Token::U64(128)],
        "invalid value: integer `128`, expected i8",
    );
}

#[test]
fn test_i16() {
    let test = assert_de_tokens_error::<i16>;

    test(
        &[Token::I32(-32769)],
        "invalid value: integer `-32769`, expected i16",
    );
    test(
        &[Token::I64(-32769)],
        "invalid value: integer `-32769`, expected i16",
    );
    test(
        &[Token::I32(32768)],
        "invalid value: integer `32768`, expected i16",
    );
    test(
        &[Token::I64(32768)],
        "invalid value: integer `32768`, expected i16",
    );

    test(
        &[Token::U16(32768)],
        "invalid value: integer `32768`, expected i16",
    );
    test(
        &[Token::U32(32768)],
        "invalid value: integer `32768`, expected i16",
    );
    test(
        &[Token::U64(32768)],
        "invalid value: integer `32768`, expected i16",
    );
}

#[test]
fn test_i32() {
    let test = assert_de_tokens_error::<i32>;

    test(
        &[Token::I64(-2147483649)],
        "invalid value: integer `-2147483649`, expected i32",
    );
    test(
        &[Token::I64(2147483648)],
        "invalid value: integer `2147483648`, expected i32",
    );

    test(
        &[Token::U32(2147483648)],
        "invalid value: integer `2147483648`, expected i32",
    );
    test(
        &[Token::U64(2147483648)],
        "invalid value: integer `2147483648`, expected i32",
    );
}

#[test]
fn test_i64() {
    let test = assert_de_tokens_error::<i64>;

    test(
        &[Token::U64(9223372036854775808)],
        "invalid value: integer `9223372036854775808`, expected i64",
    );
}

#[test]
fn test_u8() {
    let test = assert_de_tokens_error::<u8>;

    test(&[Token::I8(-1)], "invalid value: integer `-1`, expected u8");
    test(
        &[Token::I16(-1)],
        "invalid value: integer `-1`, expected u8",
    );
    test(
        &[Token::I32(-1)],
        "invalid value: integer `-1`, expected u8",
    );
    test(
        &[Token::I64(-1)],
        "invalid value: integer `-1`, expected u8",
    );
    test(
        &[Token::I16(256)],
        "invalid value: integer `256`, expected u8",
    );
    test(
        &[Token::I32(256)],
        "invalid value: integer `256`, expected u8",
    );
    test(
        &[Token::I64(256)],
        "invalid value: integer `256`, expected u8",
    );

    test(
        &[Token::U16(256)],
        "invalid value: integer `256`, expected u8",
    );
    test(
        &[Token::U32(256)],
        "invalid value: integer `256`, expected u8",
    );
    test(
        &[Token::U64(256)],
        "invalid value: integer `256`, expected u8",
    );
}

#[test]
fn test_u16() {
    let test = assert_de_tokens_error::<u16>;

    test(
        &[Token::I8(-1)],
        "invalid value: integer `-1`, expected u16",
    );
    test(
        &[Token::I16(-1)],
        "invalid value: integer `-1`, expected u16",
    );
    test(
        &[Token::I32(-1)],
        "invalid value: integer `-1`, expected u16",
    );
    test(
        &[Token::I64(-1)],
        "invalid value: integer `-1`, expected u16",
    );
    test(
        &[Token::I32(65536)],
        "invalid value: integer `65536`, expected u16",
    );
    test(
        &[Token::I64(65536)],
        "invalid value: integer `65536`, expected u16",
    );

    test(
        &[Token::U32(65536)],
        "invalid value: integer `65536`, expected u16",
    );
    test(
        &[Token::U64(65536)],
        "invalid value: integer `65536`, expected u16",
    );
}

#[test]
fn test_u32() {
    let test = assert_de_tokens_error::<u32>;

    test(
        &[Token::I8(-1)],
        "invalid value: integer `-1`, expected u32",
    );
    test(
        &[Token::I16(-1)],
        "invalid value: integer `-1`, expected u32",
    );
    test(
        &[Token::I32(-1)],
        "invalid value: integer `-1`, expected u32",
    );
    test(
        &[Token::I64(-1)],
        "invalid value: integer `-1`, expected u32",
    );
    test(
        &[Token::I64(4294967296)],
        "invalid value: integer `4294967296`, expected u32",
    );

    test(
        &[Token::U64(4294967296)],
        "invalid value: integer `4294967296`, expected u32",
    );
}

#[test]
fn test_u64() {
    let test = assert_de_tokens_error::<u64>;

    test(
        &[Token::I8(-1)],
        "invalid value: integer `-1`, expected u64",
    );
    test(
        &[Token::I16(-1)],
        "invalid value: integer `-1`, expected u64",
    );
    test(
        &[Token::I32(-1)],
        "invalid value: integer `-1`, expected u64",
    );
    test(
        &[Token::I64(-1)],
        "invalid value: integer `-1`, expected u64",
    );
}

#[test]
fn test_u128() {
    let test = assert_de_tokens_error::<u128>;

    test(
        &[Token::I8(-1)],
        "invalid value: integer `-1`, expected u128",
    );
    test(
        &[Token::I16(-1)],
        "invalid value: integer `-1`, expected u128",
    );
    test(
        &[Token::I32(-1)],
        "invalid value: integer `-1`, expected u128",
    );
    test(
        &[Token::I64(-1)],
        "invalid value: integer `-1`, expected u128",
    );
}

#[test]
fn test_usize() {
    let test = assert_de_tokens_error::<usize>;

    test(
        &[Token::I8(-1)],
        "invalid value: integer `-1`, expected usize",
    );
    test(
        &[Token::I16(-1)],
        "invalid value: integer `-1`, expected usize",
    );
    test(
        &[Token::I32(-1)],
        "invalid value: integer `-1`, expected usize",
    );
    test(
        &[Token::I64(-1)],
        "invalid value: integer `-1`, expected usize",
    );
}

#[test]
fn test_unknown_field() {
    assert_de_tokens_error::<StructDenyUnknown>(
        &[
            Token::Struct {
                name: "StructDenyUnknown",
                len: 1,
            },
            Token::Str("a"),
            Token::I32(0),
            Token::Str("d"),
        ],
        "unknown field `d`, expected `a`",
    );
}

#[test]
fn test_skipped_field_is_unknown() {
    assert_de_tokens_error::<StructDenyUnknown>(
        &[
            Token::Struct {
                name: "StructDenyUnknown",
                len: 1,
            },
            Token::Str("b"),
        ],
        "unknown field `b`, expected `a`",
    );
}

#[test]
fn test_skip_all_deny_unknown() {
    assert_de_tokens_error::<StructSkipAllDenyUnknown>(
        &[
            Token::Struct {
                name: "StructSkipAllDenyUnknown",
                len: 0,
            },
            Token::Str("a"),
        ],
        "unknown field `a`, there are no fields",
    );
}

#[test]
fn test_unknown_variant() {
    assert_de_tokens_error::<Enum>(
        &[Token::UnitVariant {
            name: "Enum",
            variant: "Foo",
        }],
        "unknown variant `Foo`, expected one of `Unit`, `Simple`, `Seq`, `Map`, `SimpleWithSkipped`",
    );
}

#[test]
fn test_duplicate_field_struct() {
    assert_de_tokens_error::<Struct>(
        &[
            Token::Map { len: Some(3) },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("a"),
        ],
        "duplicate field `a`",
    );
}

#[test]
fn test_duplicate_field_enum() {
    assert_de_tokens_error::<Enum>(
        &[
            Token::StructVariant {
                name: "Enum",
                variant: "Map",
                len: 3,
            },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("a"),
        ],
        "duplicate field `a`",
    );
}

#[test]
fn test_enum_out_of_range() {
    assert_de_tokens_error::<Enum>(
        &[Token::Enum { name: "Enum" }, Token::U32(5), Token::Unit],
        "invalid value: integer `5`, expected variant index 0 <= i < 5",
    );
}

#[test]
fn test_short_tuple() {
    assert_de_tokens_error::<(u8, u8, u8)>(
        &[Token::Tuple { len: 1 }, Token::U8(1), Token::TupleEnd],
        "invalid length 1, expected a tuple of size 3",
    );
}

#[test]
fn test_short_array() {
    assert_de_tokens_error::<[u8; 3]>(
        &[Token::Seq { len: Some(1) }, Token::U8(1), Token::SeqEnd],
        "invalid length 1, expected an array of length 3",
    );
}

#[test]
fn test_unit_from_empty_seq() {
    assert_de_tokens_error::<()>(
        &[Token::Seq { len: Some(0) }, Token::SeqEnd],
        "invalid type: sequence, expected unit",
    );
}

#[test]
fn test_unit_from_empty_seq_without_len() {
    assert_de_tokens_error::<()>(
        &[Token::Seq { len: None }, Token::SeqEnd],
        "invalid type: sequence, expected unit",
    );
}

#[test]
fn test_unit_from_tuple_struct() {
    assert_de_tokens_error::<()>(
        &[
            Token::TupleStruct {
                name: "Anything",
                len: 0,
            },
            Token::TupleStructEnd,
        ],
        "invalid type: sequence, expected unit",
    );
}

#[test]
fn test_string_from_unit() {
    assert_de_tokens_error::<String>(
        &[Token::Unit],
        "invalid type: unit value, expected a string",
    );
}

#[test]
fn test_vec_from_unit() {
    assert_de_tokens_error::<Vec<isize>>(
        &[Token::Unit],
        "invalid type: unit value, expected a sequence",
    );
}

#[test]
fn test_vec_from_unit_struct() {
    assert_de_tokens_error::<Vec<isize>>(
        &[Token::UnitStruct { name: "Anything" }],
        "invalid type: unit value, expected a sequence",
    );
}

#[test]
fn test_zero_array_from_unit() {
    assert_de_tokens_error::<[isize; 0]>(
        &[Token::Unit],
        "invalid type: unit value, expected an empty array",
    );
}

#[test]
fn test_zero_array_from_unit_struct() {
    assert_de_tokens_error::<[isize; 0]>(
        &[Token::UnitStruct { name: "Anything" }],
        "invalid type: unit value, expected an empty array",
    );
}

#[test]
fn test_btreemap_from_unit() {
    assert_de_tokens_error::<BTreeMap<isize, isize>>(
        &[Token::Unit],
        "invalid type: unit value, expected a map",
    );
}

#[test]
fn test_btreemap_from_unit_struct() {
    assert_de_tokens_error::<BTreeMap<isize, isize>>(
        &[Token::UnitStruct { name: "Anything" }],
        "invalid type: unit value, expected a map",
    );
}

#[test]
fn test_hashmap_from_unit() {
    assert_de_tokens_error::<HashMap<isize, isize>>(
        &[Token::Unit],
        "invalid type: unit value, expected a map",
    );
}

#[test]
fn test_hashmap_from_unit_struct() {
    assert_de_tokens_error::<HashMap<isize, isize>>(
        &[Token::UnitStruct { name: "Anything" }],
        "invalid type: unit value, expected a map",
    );
}

#[test]
fn test_bool_from_string() {
    assert_de_tokens_error::<bool>(
        &[Token::Str("false")],
        "invalid type: string \"false\", expected a boolean",
    );
}

#[test]
fn test_number_from_string() {
    assert_de_tokens_error::<isize>(
        &[Token::Str("1")],
        "invalid type: string \"1\", expected isize",
    );
}

#[test]
fn test_integer_from_float() {
    assert_de_tokens_error::<isize>(
        &[Token::F32(0.0)],
        "invalid type: floating point `0.0`, expected isize",
    );
}

#[test]
fn test_nan_no_decimal_point() {
    assert_de_tokens_error::<isize>(
        &[Token::F32(f32::NAN)],
        "invalid type: floating point `NaN`, expected isize",
    );
}

#[test]
fn test_unit_struct_from_seq() {
    assert_de_tokens_error::<UnitStruct>(
        &[Token::Seq { len: Some(0) }, Token::SeqEnd],
        "invalid type: sequence, expected unit struct UnitStruct",
    );
}
