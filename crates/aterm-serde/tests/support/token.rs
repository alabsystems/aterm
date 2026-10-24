// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_test 1.0.177 src/token.rs (github.com/serde-rs/test) and
// modified by the aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use std::fmt::{self, Debug, Display};

/// A serialized token for testing.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Token {
    /// A serialized `bool`.
    Bool(bool),
    /// A serialized `i8`.
    I8(i8),
    /// A serialized `i16`.
    I16(i16),
    /// A serialized `i32`.
    I32(i32),
    /// A serialized `i64`.
    I64(i64),
    /// A serialized `u8`.
    U8(u8),
    /// A serialized `u16`.
    U16(u16),
    /// A serialized `u32`.
    U32(u32),
    /// A serialized `u64`.
    U64(u64),
    /// A serialized `f32`.
    F32(f32),
    /// A serialized `f64`.
    F64(f64),
    /// A serialized `char`.
    Char(char),
    /// A serialized `str`.
    Str(&'static str),
    /// A borrowed `str`.
    BorrowedStr(&'static str),
    /// A serialized `String`.
    String(&'static str),
    /// A serialized `[u8]`.
    Bytes(&'static [u8]),
    /// A borrowed `[u8]`.
    BorrowedBytes(&'static [u8]),
    /// A serialized `ByteBuf`.
    ByteBuf(&'static [u8]),
    /// A serialized `Option<T>` containing none.
    None,
    /// The header to a serialized `Option<T>` containing some value.
    Some,
    /// A serialized `()`.
    Unit,
    UnitStruct {
        name: &'static str,
    },
    /// A unit variant of an enum.
    UnitVariant {
        name: &'static str,
        variant: &'static str,
    },
    NewtypeStruct {
        name: &'static str,
    },
    /// The header to a newtype variant of an enum.
    NewtypeVariant {
        name: &'static str,
        variant: &'static str,
    },
    Seq {
        len: Option<usize>,
    },
    /// An indicator of the end of a sequence.
    SeqEnd,
    Tuple {
        len: usize,
    },
    /// An indicator of the end of a tuple.
    TupleEnd,
    TupleStruct {
        name: &'static str,
        len: usize,
    },
    /// An indicator of the end of a tuple struct.
    TupleStructEnd,
    /// The header to a tuple variant of an enum.
    TupleVariant {
        name: &'static str,
        variant: &'static str,
        len: usize,
    },
    /// An indicator of the end of a tuple variant.
    TupleVariantEnd,
    Map {
        len: Option<usize>,
    },
    /// An indicator of the end of a map.
    MapEnd,
    Struct {
        name: &'static str,
        len: usize,
    },
    /// An indicator of the end of a struct.
    StructEnd,
    /// The header of a struct variant of an enum.
    StructVariant {
        name: &'static str,
        variant: &'static str,
        len: usize,
    },
    /// An indicator of the end of a struct variant.
    StructVariantEnd,
    Enum {
        name: &'static str,
    },
}

impl Display for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        Debug::fmt(self, formatter)
    }
}
