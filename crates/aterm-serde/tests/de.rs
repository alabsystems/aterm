// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_de.rs (git tag v1.0.228) and
// modified by the aterm project in 2026: ported onto aterm-serde's surface.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

#![allow(
    clippy::cast_lossless,
    clippy::decimal_literal_representation,
    clippy::derive_partial_eq_without_eq,
    clippy::empty_enums,
    clippy::manual_assert,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::unreadable_literal
)]

use crate::support::{Token, assert_de_tokens};
use aterm_serde::de::{Deserialize, Deserializer};
use serde_derive::Deserialize;
use std::collections::BTreeMap;
use std::default::Default;
use std::fmt::Debug;
use std::iter;
use std::path::PathBuf;

#[derive(Copy, Clone, PartialEq, Debug, Deserialize)]
struct UnitStruct;

#[derive(Copy, Clone, PartialEq, Debug, Deserialize)]
struct GenericUnitStruct<const N: u8>;

#[derive(PartialEq, Debug, Deserialize)]
struct NewtypeStruct(i32);

#[derive(PartialEq, Debug, Deserialize)]
struct TupleStruct(i32, i32, i32);

#[derive(PartialEq, Debug, Deserialize)]
struct Struct {
    a: i32,
    b: i32,
    #[serde(skip)]
    c: i32,
}

#[derive(PartialEq, Debug, Deserialize)]
#[serde(default)]
struct StructDefault<T> {
    a: i32,
    b: T,
}

impl Default for StructDefault<String> {
    fn default() -> Self {
        StructDefault {
            a: 100,
            b: "default".to_string(),
        }
    }
}

#[derive(PartialEq, Debug, Deserialize)]
struct StructSkipAll {
    #[serde(skip)]
    a: i32,
}

#[derive(PartialEq, Debug, Deserialize)]
#[serde(default)]
struct StructSkipDefault {
    #[serde(skip)]
    a: i32,
}

impl Default for StructSkipDefault {
    fn default() -> Self {
        StructSkipDefault { a: 16 }
    }
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

#[derive(PartialEq, Debug)]
struct IgnoredAny;

impl<'de> Deserialize<'de> for IgnoredAny {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        aterm_serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(IgnoredAny)
    }
}

#[track_caller]
fn test<'de, T>(value: T, tokens: &'de [Token])
where
    T: Deserialize<'de> + PartialEq + Debug,
{
    assert_de_tokens(&value, tokens);

    assert_de_tokens_ignore(tokens);
}

#[track_caller]
fn assert_de_tokens_ignore(ignorable_tokens: &[Token]) {
    #[derive(PartialEq, Debug, Deserialize)]
    struct IgnoreBase {
        a: i32,
    }

    let concated_tokens: Vec<Token> = vec![
        Token::Map { len: Some(2) },
        Token::Str("a"),
        Token::I32(1),
        Token::Str("ignored"),
    ]
    .into_iter()
    .chain(ignorable_tokens.iter().copied())
    .chain(iter::once(Token::MapEnd))
    .collect();

    let expected = IgnoreBase { a: 1 };
    assert_de_tokens(&expected, &concated_tokens);
}

#[test]
fn test_bool() {
    test(true, &[Token::Bool(true)]);
    test(false, &[Token::Bool(false)]);
}

#[test]
fn test_i8() {
    let test = test::<i8>;

    test(-128, &[Token::I8(-128)]);
    test(-128, &[Token::I16(-128)]);
    test(-128, &[Token::I32(-128)]);
    test(-128, &[Token::I64(-128)]);
    test(127, &[Token::I8(127)]);
    test(127, &[Token::I16(127)]);
    test(127, &[Token::I32(127)]);
    test(127, &[Token::I64(127)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(127, &[Token::U8(127)]);
    test(127, &[Token::U16(127)]);
    test(127, &[Token::U32(127)]);
    test(127, &[Token::U64(127)]);
}

#[test]
fn test_i16() {
    let test = test::<i16>;

    test(-128, &[Token::I8(-128)]);
    test(-32768, &[Token::I16(-32768)]);
    test(-32768, &[Token::I32(-32768)]);
    test(-32768, &[Token::I64(-32768)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(32767, &[Token::I32(32767)]);
    test(32767, &[Token::I64(32767)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(32767, &[Token::U16(32767)]);
    test(32767, &[Token::U32(32767)]);
    test(32767, &[Token::U64(32767)]);
}

#[test]
fn test_i32() {
    let test = test::<i32>;

    test(-128, &[Token::I8(-128)]);
    test(-32768, &[Token::I16(-32768)]);
    test(-2147483648, &[Token::I32(-2147483648)]);
    test(-2147483648, &[Token::I64(-2147483648)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(2147483647, &[Token::I32(2147483647)]);
    test(2147483647, &[Token::I64(2147483647)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(65535, &[Token::U16(65535)]);
    test(2147483647, &[Token::U32(2147483647)]);
    test(2147483647, &[Token::U64(2147483647)]);
}

#[test]
fn test_i64() {
    let test = test::<i64>;

    test(-128, &[Token::I8(-128)]);
    test(-32768, &[Token::I16(-32768)]);
    test(-2147483648, &[Token::I32(-2147483648)]);
    test(-9223372036854775808, &[Token::I64(-9223372036854775808)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(2147483647, &[Token::I32(2147483647)]);
    test(9223372036854775807, &[Token::I64(9223372036854775807)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(65535, &[Token::U16(65535)]);
    test(4294967295, &[Token::U32(4294967295)]);
    test(9223372036854775807, &[Token::U64(9223372036854775807)]);
}

#[test]
fn test_i128() {
    let test = test::<i128>;

    test(-128, &[Token::I8(-128)]);
    test(-32768, &[Token::I16(-32768)]);
    test(-2147483648, &[Token::I32(-2147483648)]);
    test(-9223372036854775808, &[Token::I64(-9223372036854775808)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(2147483647, &[Token::I32(2147483647)]);
    test(9223372036854775807, &[Token::I64(9223372036854775807)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(65535, &[Token::U16(65535)]);
    test(4294967295, &[Token::U32(4294967295)]);
    test(18446744073709551615, &[Token::U64(18446744073709551615)]);
}

#[test]
fn test_isize() {
    let test = test::<isize>;

    test(-10, &[Token::I8(-10)]);
    test(-10, &[Token::I16(-10)]);
    test(-10, &[Token::I32(-10)]);
    test(-10, &[Token::I64(-10)]);
    test(10, &[Token::I8(10)]);
    test(10, &[Token::I16(10)]);
    test(10, &[Token::I32(10)]);
    test(10, &[Token::I64(10)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(10, &[Token::U8(10)]);
    test(10, &[Token::U16(10)]);
    test(10, &[Token::U32(10)]);
    test(10, &[Token::U64(10)]);
}

#[test]
fn test_u8() {
    let test = test::<u8>;

    test(0, &[Token::I8(0)]);
    test(0, &[Token::I16(0)]);
    test(0, &[Token::I32(0)]);
    test(0, &[Token::I64(0)]);
    test(127, &[Token::I8(127)]);
    test(255, &[Token::I16(255)]);
    test(255, &[Token::I32(255)]);
    test(255, &[Token::I64(255)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(255, &[Token::U16(255)]);
    test(255, &[Token::U32(255)]);
    test(255, &[Token::U64(255)]);
}

#[test]
fn test_u16() {
    let test = test::<u16>;

    test(0, &[Token::I8(0)]);
    test(0, &[Token::I16(0)]);
    test(0, &[Token::I32(0)]);
    test(0, &[Token::I64(0)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(65535, &[Token::I32(65535)]);
    test(65535, &[Token::I64(65535)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(65535, &[Token::U16(65535)]);
    test(65535, &[Token::U32(65535)]);
    test(65535, &[Token::U64(65535)]);
}

#[test]
fn test_u32() {
    let test = test::<u32>;

    test(0, &[Token::I8(0)]);
    test(0, &[Token::I16(0)]);
    test(0, &[Token::I32(0)]);
    test(0, &[Token::I64(0)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(2147483647, &[Token::I32(2147483647)]);
    test(4294967295, &[Token::I64(4294967295)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(65535, &[Token::U16(65535)]);
    test(4294967295, &[Token::U32(4294967295)]);
    test(4294967295, &[Token::U64(4294967295)]);
}

#[test]
fn test_u64() {
    let test = test::<u64>;

    test(0, &[Token::I8(0)]);
    test(0, &[Token::I16(0)]);
    test(0, &[Token::I32(0)]);
    test(0, &[Token::I64(0)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(2147483647, &[Token::I32(2147483647)]);
    test(9223372036854775807, &[Token::I64(9223372036854775807)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(65535, &[Token::U16(65535)]);
    test(4294967295, &[Token::U32(4294967295)]);
    test(18446744073709551615, &[Token::U64(18446744073709551615)]);
}

#[test]
fn test_u128() {
    let test = test::<u128>;

    test(0, &[Token::I8(0)]);
    test(0, &[Token::I16(0)]);
    test(0, &[Token::I32(0)]);
    test(0, &[Token::I64(0)]);
    test(127, &[Token::I8(127)]);
    test(32767, &[Token::I16(32767)]);
    test(2147483647, &[Token::I32(2147483647)]);
    test(9223372036854775807, &[Token::I64(9223372036854775807)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(255, &[Token::U8(255)]);
    test(65535, &[Token::U16(65535)]);
    test(4294967295, &[Token::U32(4294967295)]);
    test(18446744073709551615, &[Token::U64(18446744073709551615)]);
}

#[test]
fn test_usize() {
    let test = test::<usize>;

    test(0, &[Token::I8(0)]);
    test(0, &[Token::I16(0)]);
    test(0, &[Token::I32(0)]);
    test(0, &[Token::I64(0)]);
    test(10, &[Token::I8(10)]);
    test(10, &[Token::I16(10)]);
    test(10, &[Token::I32(10)]);
    test(10, &[Token::I64(10)]);

    test(0, &[Token::U8(0)]);
    test(0, &[Token::U16(0)]);
    test(0, &[Token::U32(0)]);
    test(0, &[Token::U64(0)]);
    test(10, &[Token::U8(10)]);
    test(10, &[Token::U16(10)]);
    test(10, &[Token::U32(10)]);
    test(10, &[Token::U64(10)]);
}

#[test]
fn test_f32() {
    let test = test::<f32>;

    test(1.11, &[Token::F32(1.11)]);
    test(1.11, &[Token::F64(1.11)]);
}

#[test]
fn test_f64() {
    let test = test::<f64>;

    test(1.11f32 as f64, &[Token::F32(1.11)]);
    test(1.11, &[Token::F64(1.11)]);
}

#[test]
fn test_nan() {
    fn de<'de, T: Deserialize<'de>>(token: &'de [Token]) -> T {
        T::deserialize(&mut crate::support::Deserializer::new(token)).unwrap()
    }
    let pos_f32 = [Token::F32(f32::NAN.copysign(1.0))];
    let pos_f64 = [Token::F64(f64::NAN.copysign(1.0))];
    assert!(de::<f32>(&pos_f32).is_sign_positive());
    assert!(de::<f32>(&pos_f64).is_sign_positive());
    assert!(de::<f64>(&pos_f32).is_sign_positive());
    assert!(de::<f64>(&pos_f64).is_sign_positive());

    let neg_f32 = [Token::F32(f32::NAN.copysign(-1.0))];
    let neg_f64 = [Token::F64(f64::NAN.copysign(-1.0))];
    assert!(de::<f32>(&neg_f32).is_sign_negative());
    assert!(de::<f32>(&neg_f64).is_sign_negative());
    assert!(de::<f64>(&neg_f32).is_sign_negative());
    assert!(de::<f64>(&neg_f64).is_sign_negative());
}

#[test]
fn test_char() {
    test('a', &[Token::Char('a')]);
    test('a', &[Token::Str("a")]);
    test('a', &[Token::String("a")]);
}

#[test]
fn test_string() {
    test("abc".to_owned(), &[Token::Str("abc")]);
    test("abc".to_owned(), &[Token::String("abc")]);
    test("a".to_owned(), &[Token::Char('a')]);
}

#[test]
fn test_option() {
    test(None::<i32>, &[Token::Unit]);
    test(None::<i32>, &[Token::None]);
    test(Some(1), &[Token::Some, Token::I32(1)]);
}

#[test]
fn test_unit() {
    test((), &[Token::Unit]);
}

#[test]
fn test_unit_struct() {
    test(UnitStruct, &[Token::Unit]);
    test(UnitStruct, &[Token::UnitStruct { name: "UnitStruct" }]);
}

#[test]
fn test_generic_unit_struct() {
    test(GenericUnitStruct::<8>, &[Token::Unit]);
    test(
        GenericUnitStruct::<8>,
        &[Token::UnitStruct {
            name: "GenericUnitStruct",
        }],
    );
}

#[test]
fn test_newtype_struct() {
    test(
        NewtypeStruct(1),
        &[
            Token::NewtypeStruct {
                name: "NewtypeStruct",
            },
            Token::I32(1),
        ],
    );
}

#[test]
fn test_tuple_struct() {
    test(
        TupleStruct(1, 2, 3),
        &[
            Token::Seq { len: Some(3) },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::SeqEnd,
        ],
    );
    test(
        TupleStruct(1, 2, 3),
        &[
            Token::Seq { len: None },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::SeqEnd,
        ],
    );
    test(
        TupleStruct(1, 2, 3),
        &[
            Token::Tuple { len: 3 },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::TupleEnd,
        ],
    );
    test(
        TupleStruct(1, 2, 3),
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
fn test_vec() {
    test(
        Vec::<isize>::new(),
        &[Token::Seq { len: Some(0) }, Token::SeqEnd],
    );

    test(
        vec![vec![], vec![1], vec![2, 3]],
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
    test(
        Vec::<isize>::new(),
        &[
            Token::TupleStruct {
                name: "Anything",
                len: 0,
            },
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn test_array() {
    test([0; 0], &[Token::Seq { len: Some(0) }, Token::SeqEnd]);
    test([0; 0], &[Token::Tuple { len: 0 }, Token::TupleEnd]);
    test(
        ([0; 0], [1], [2, 3]),
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
    test(
        ([0; 0], [1], [2, 3]),
        &[
            Token::Tuple { len: 3 },
            Token::Tuple { len: 0 },
            Token::TupleEnd,
            Token::Tuple { len: 1 },
            Token::I32(1),
            Token::TupleEnd,
            Token::Tuple { len: 2 },
            Token::I32(2),
            Token::I32(3),
            Token::TupleEnd,
            Token::TupleEnd,
        ],
    );
    test(
        [0; 0],
        &[
            Token::TupleStruct {
                name: "Anything",
                len: 0,
            },
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn test_tuple() {
    test(
        (1,),
        &[Token::Seq { len: Some(1) }, Token::I32(1), Token::SeqEnd],
    );
    test(
        (1, 2, 3),
        &[
            Token::Seq { len: Some(3) },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::SeqEnd,
        ],
    );
    test(
        (1,),
        &[Token::Tuple { len: 1 }, Token::I32(1), Token::TupleEnd],
    );
    test(
        (1, 2, 3),
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
    test(
        BTreeMap::<isize, isize>::new(),
        &[Token::Map { len: Some(0) }, Token::MapEnd],
    );
    test(
        btreemap![1 => 2],
        &[
            Token::Map { len: Some(1) },
            Token::I32(1),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        btreemap![1 => 2, 3 => 4],
        &[
            Token::Map { len: Some(2) },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::I32(4),
            Token::MapEnd,
        ],
    );
    test(
        btreemap![1 => btreemap![], 2 => btreemap![3 => 4, 5 => 6]],
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
    test(
        BTreeMap::<isize, isize>::new(),
        &[
            Token::Struct {
                name: "Anything",
                len: 0,
            },
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_struct() {
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::U8(0),
            Token::I32(1),
            Token::U8(1),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::U16(0),
            Token::I32(1),
            Token::U16(1),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::U32(0),
            Token::I32(1),
            Token::U32(1),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::U64(0),
            Token::I32(1),
            Token::U64(1),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::U8(0),
            Token::I32(1),
            Token::U64(1),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::U8(0),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::StructEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Seq { len: Some(3) },
            Token::I32(1),
            Token::I32(2),
            Token::SeqEnd,
        ],
    );
}

#[test]
fn test_struct_borrowed_keys() {
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::BorrowedStr("a"),
            Token::I32(1),
            Token::BorrowedStr("b"),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::BorrowedStr("a"),
            Token::I32(1),
            Token::BorrowedStr("b"),
            Token::I32(2),
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_struct_owned_keys() {
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::String("a"),
            Token::I32(1),
            Token::String("b"),
            Token::I32(2),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::String("a"),
            Token::I32(1),
            Token::String("b"),
            Token::I32(2),
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_struct_with_skip() {
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::Str("c"),
            Token::I32(3),
            Token::Str("d"),
            Token::I32(4),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Map { len: Some(3) },
            Token::U8(0),
            Token::I32(1),
            Token::U16(1),
            Token::I32(2),
            Token::U32(2),
            Token::I32(3),
            Token::U64(3),
            Token::I32(4),
            Token::MapEnd,
        ],
    );
    test(
        Struct { a: 1, b: 2, c: 0 },
        &[
            Token::Struct {
                name: "Struct",
                len: 2,
            },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::Str("c"),
            Token::I32(3),
            Token::Str("d"),
            Token::I32(4),
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_struct_skip_all() {
    test(
        StructSkipAll { a: 0 },
        &[
            Token::Struct {
                name: "StructSkipAll",
                len: 0,
            },
            Token::StructEnd,
        ],
    );
    test(
        StructSkipAll { a: 0 },
        &[
            Token::Struct {
                name: "StructSkipAll",
                len: 0,
            },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_struct_skip_default() {
    test(
        StructSkipDefault { a: 16 },
        &[
            Token::Struct {
                name: "StructSkipDefault",
                len: 0,
            },
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_struct_skip_all_deny_unknown() {
    test(
        StructSkipAllDenyUnknown { a: 0 },
        &[
            Token::Struct {
                name: "StructSkipAllDenyUnknown",
                len: 0,
            },
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_struct_default() {
    test(
        StructDefault {
            a: 50,
            b: "overwritten".to_string(),
        },
        &[
            Token::Struct {
                name: "StructDefault",
                len: 2,
            },
            Token::Str("a"),
            Token::I32(50),
            Token::Str("b"),
            Token::String("overwritten"),
            Token::StructEnd,
        ],
    );
    test(
        StructDefault {
            a: 100,
            b: "default".to_string(),
        },
        &[
            Token::Struct {
                name: "StructDefault",
                len: 2,
            },
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_enum_unit() {
    test(
        Enum::Unit,
        &[Token::UnitVariant {
            name: "Enum",
            variant: "Unit",
        }],
    );
}

#[test]
fn test_enum_simple() {
    test(
        Enum::Simple(1),
        &[
            Token::NewtypeVariant {
                name: "Enum",
                variant: "Simple",
            },
            Token::I32(1),
        ],
    );
}

#[test]
fn test_enum_simple_with_skipped() {
    test(
        Enum::SimpleWithSkipped(NotDeserializable),
        &[Token::UnitVariant {
            name: "Enum",
            variant: "SimpleWithSkipped",
        }],
    );
}

#[test]
fn test_enum_seq() {
    test(
        Enum::Seq(1, 2, 3),
        &[
            Token::TupleVariant {
                name: "Enum",
                variant: "Seq",
                len: 3,
            },
            Token::I32(1),
            Token::I32(2),
            Token::I32(3),
            Token::TupleVariantEnd,
        ],
    );
}

#[test]
fn test_enum_map() {
    test(
        Enum::Map { a: 1, b: 2, c: 3 },
        &[
            Token::StructVariant {
                name: "Enum",
                variant: "Map",
                len: 3,
            },
            Token::Str("a"),
            Token::I32(1),
            Token::Str("b"),
            Token::I32(2),
            Token::Str("c"),
            Token::I32(3),
            Token::StructVariantEnd,
        ],
    );
}

#[test]
fn test_enum_unit_usize() {
    test(
        Enum::Unit,
        &[Token::Enum { name: "Enum" }, Token::U32(0), Token::Unit],
    );
}

#[test]
fn test_enum_unit_bytes() {
    test(
        Enum::Unit,
        &[
            Token::Enum { name: "Enum" },
            Token::Bytes(b"Unit"),
            Token::Unit,
        ],
    );
}

#[test]
fn test_box() {
    test(Box::new(0i32), &[Token::I32(0)]);
}

#[test]
fn test_boxed_array() {
    test(
        Box::new([0, 1, 2]),
        &[
            Token::Seq { len: Some(3) },
            Token::I32(0),
            Token::I32(1),
            Token::I32(2),
            Token::SeqEnd,
        ],
    );
}

#[test]
fn test_path_buf() {
    test(
        PathBuf::from("/usr/local/lib"),
        &[Token::Str("/usr/local/lib")],
    );
    test(
        PathBuf::from("/usr/local/lib"),
        &[Token::String("/usr/local/lib")],
    );
    test(
        PathBuf::from("/usr/local/lib"),
        &[Token::Bytes(b"/usr/local/lib")],
    );
    test(
        PathBuf::from("/usr/local/lib"),
        &[Token::ByteBuf(b"/usr/local/lib")],
    );
}

#[test]
fn test_ignored_any() {
    test(IgnoredAny, &[Token::Str("s")]);
    test(
        IgnoredAny,
        &[
            Token::Seq { len: Some(1) },
            Token::Bool(true),
            Token::SeqEnd,
        ],
    );
    test(
        IgnoredAny,
        &[Token::Enum { name: "E" }, Token::Str("Rust"), Token::Unit],
    );
}
