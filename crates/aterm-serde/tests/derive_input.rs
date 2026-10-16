// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The input shapes aterm-serde-derive's parser (crates/aterm-macro-support)
//! has to read, each held to the tokens it writes and reads back
//! (docs/AUDIT-third-party-2026-10-15.md, SP.5): generics with bounds,
//! defaults and where clauses; lifetimes; raw identifiers and raw strings; doc
//! attributes; visibilities, including `pub (A, B)` in a tuple; the
//! None-delimited groups `macro_rules!` fragments arrive in; nested generics
//! and `->` inside a field type; associated-type fields; `Self` in a field
//! type; discriminants, shifts and comparisons in them included; and a packed
//! struct.
//!
//! `proc_macro` runs only inside the compiler, so the parser is tested here,
//! through the derive, and not by unit tests.

#![expect(
    clippy::type_complexity,
    reason = "`Nested::map` and `Arrow::f` are the input under test; the lint fires in the \
              generated impls, where registry serde_derive 1.0.228 draws the same two \
              (checked on these structs in a crate outside the workspace)"
)]

use crate::support::{
    Token, assert_de_tokens, assert_de_tokens_error, assert_ser_tokens, assert_tokens,
};
use serde_derive::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::marker::PhantomData;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Generic<'a, T: Clone + Debug, U = u8, const N: usize = 2>
where
    T: PartialEq,
    'a: 'a,
{
    t: T,
    u: Vec<U>,
    #[serde(skip)]
    marker: PhantomData<&'a ()>,
}

#[test]
fn generics_with_bounds_defaults_and_a_where_clause() {
    let value: Generic<'_, i32> = Generic {
        t: 1,
        u: vec![7],
        marker: PhantomData,
    };
    assert_tokens(
        &value,
        &[
            Token::Struct {
                name: "Generic",
                len: 2,
            },
            Token::Str("t"),
            Token::I32(1),
            Token::Str("u"),
            Token::Seq { len: Some(1) },
            Token::U8(7),
            Token::SeqEnd,
            Token::StructEnd,
        ],
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Pair<T>(T, T)
where
    T: Copy;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Either<T>
where
    T: Copy,
{
    One(T),
    Named { t: T },
}

#[test]
fn where_clauses_after_a_tuple_and_before_an_enum_body() {
    assert_tokens(
        &Pair(1u16, 2),
        &[
            Token::TupleStruct {
                name: "Pair",
                len: 2,
            },
            Token::U16(1),
            Token::U16(2),
            Token::TupleStructEnd,
        ],
    );
    assert_tokens(
        &Either::Named { t: 3u8 },
        &[
            Token::StructVariant {
                name: "Either",
                variant: "Named",
                len: 1,
            },
            Token::Str("t"),
            Token::U8(3),
            Token::StructVariantEnd,
        ],
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum r#Kind {
    r#Match,
    #[serde(rename = r#"say "hi""#)]
    Quoted,
    #[serde(rename = "caf\u{e9}\t\x21")]
    Escaped,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct r#Raw {
    r#type: u8,
    r#fn: Option<u8>,
}

#[test]
fn raw_identifiers_and_strings_name_the_data() {
    assert_tokens(
        &r#Kind::r#Match,
        &[Token::UnitVariant {
            name: "Kind",
            variant: "match",
        }],
    );
    assert_tokens(
        &Kind::Quoted,
        &[Token::UnitVariant {
            name: "Kind",
            variant: "say \"hi\"",
        }],
    );
    assert_tokens(
        &Kind::Escaped,
        &[Token::UnitVariant {
            name: "Kind",
            variant: "café\t!",
        }],
    );
    assert_tokens(
        &Raw {
            r#type: 1,
            r#fn: None,
        },
        &[
            Token::Struct {
                name: "Raw",
                len: 2,
            },
            Token::Str("type"),
            Token::U8(1),
            Token::Str("fn"),
            Token::None,
            Token::StructEnd,
        ],
    );
}

mod visibility {
    use super::*;

    /// A documented struct.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[doc = "More documentation."]
    pub(crate) struct Documented {
        /// The first field.
        #[doc = "More of it."]
        pub a: u8,
        pub(crate) b: u8,
        pub(super) c: u8,
        pub(in crate::derive_input) d: u8,
    }

    /// `pub (u8, u8)` is a visibility and a tuple type, not `pub(...)`.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    pub struct Spaced(pub (u8, u8), pub(crate) u8);
}

#[test]
fn doc_attributes_and_visibilities() {
    assert_tokens(
        &visibility::Documented {
            a: 1,
            b: 2,
            c: 3,
            d: 4,
        },
        &[
            Token::Struct {
                name: "Documented",
                len: 4,
            },
            Token::Str("a"),
            Token::U8(1),
            Token::Str("b"),
            Token::U8(2),
            Token::Str("c"),
            Token::U8(3),
            Token::Str("d"),
            Token::U8(4),
            Token::StructEnd,
        ],
    );
    assert_tokens(
        &visibility::Spaced((1, 2), 3),
        &[
            Token::TupleStruct {
                name: "Spaced",
                len: 2,
            },
            Token::Tuple { len: 2 },
            Token::U8(1),
            Token::U8(2),
            Token::TupleEnd,
            Token::U8(3),
            Token::TupleStructEnd,
        ],
    );
}

// `$vis`, `$ty` and `$key` arrive wrapped in None-delimited groups; `$name`
// and `$field` arrive bare.
macro_rules! model {
    ($vis:vis struct $name:ident { $($fvis:vis $field:ident : $ty:ty = $key:literal),* $(,)? }) => {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        $vis struct $name {
            $(
                #[serde(rename = $key)]
                $fvis $field: $ty,
            )*
        }
    };
}

model!(
    pub(crate) struct Fragments {
    pub a: Vec<u8> = "alpha",
    b: Option<(u8, u16)> = "beta",
}
);

macro_rules! newtype {
    ($name:ident($ty:ty)) => {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct $name($ty);
    };
}

// The field's `$ty` None-group starts the tuple field: a type, not a `$vis`.
newtype!(Wrapped(Vec<u8>));

// This is an opaque type macro, rather than a `$ty` fragment. `Self` is
// its input syntax; replacing it with the enclosing type changes the macro.
macro_rules! wire_ty {
    (Self) => {
        u8
    };
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct MacroTyped {
    value: wire_ty!(Self),
}

#[test]
fn self_inside_a_type_macro_remains_the_macros_input() {
    assert_tokens(
        &MacroTyped { value: 7 },
        &[
            Token::Struct {
                name: "MacroTyped",
                len: 1,
            },
            Token::Str("value"),
            Token::U8(7),
            Token::StructEnd,
        ],
    );
}

#[test]
fn macro_fragments_read_through_their_none_groups() {
    assert_tokens(
        &Fragments {
            a: vec![1],
            b: Some((2, 3)),
        },
        &[
            Token::Struct {
                name: "Fragments",
                len: 2,
            },
            Token::Str("alpha"),
            Token::Seq { len: Some(1) },
            Token::U8(1),
            Token::SeqEnd,
            Token::Str("beta"),
            Token::Some,
            Token::Tuple { len: 2 },
            Token::U8(2),
            Token::U16(3),
            Token::TupleEnd,
            Token::StructEnd,
        ],
    );
    assert_tokens(
        &Wrapped(vec![9]),
        &[
            Token::NewtypeStruct { name: "Wrapped" },
            Token::Seq { len: Some(1) },
            Token::U8(9),
            Token::SeqEnd,
        ],
    );
}

pub trait Assoc {
    type Out;
}

impl Assoc for u8 {
    type Out = String;
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Nested<K: Ord, V, A: Assoc> {
    map: BTreeMap<K, Vec<Option<(V, [u8; 2])>>>,
    out: A::Out,
    #[serde(skip)]
    assoc: PhantomData<A>,
}

#[test]
fn nested_generics_and_an_associated_type_are_bounded() {
    let mut map = BTreeMap::new();
    map.insert(1u8, vec![Some((2i8, [3u8, 4]))]);
    let value: Nested<u8, i8, u8> = Nested {
        map,
        out: "x".to_owned(),
        assoc: PhantomData,
    };
    assert_tokens(
        &value,
        &[
            Token::Struct {
                name: "Nested",
                len: 2,
            },
            Token::Str("map"),
            Token::Map { len: Some(1) },
            Token::U8(1),
            Token::Seq { len: Some(1) },
            Token::Some,
            Token::Tuple { len: 2 },
            Token::I8(2),
            Token::Tuple { len: 2 },
            Token::U8(3),
            Token::U8(4),
            Token::TupleEnd,
            Token::TupleEnd,
            Token::SeqEnd,
            Token::MapEnd,
            Token::Str("out"),
            Token::Str("x"),
            Token::StructEnd,
        ],
    );
}

// A `->` inside a field's type closes no angle bracket, and the field after it
// still splits at its comma. The skipped closure type gets no `Serialize`
// bound, so a function pointer serves.
#[derive(Serialize)]
#[expect(dead_code, reason = "`f` and `g` exist to be parsed and skipped")]
struct Arrow<T, F>
where
    F: for<'x> Fn(&'x u8) -> Option<u8>,
{
    #[serde(skip)]
    f: Option<fn(Vec<T>) -> BTreeMap<u8, T>>,
    #[serde(skip)]
    g: Option<F>,
    t: T,
}

fn first(x: &u8) -> Option<u8> {
    Some(*x)
}

#[test]
fn arrows_in_types_and_higher_ranked_bounds() {
    let value: Arrow<u8, fn(&u8) -> Option<u8>> = Arrow {
        f: None,
        g: Some(first),
        t: 5,
    };
    assert_ser_tokens(
        &value,
        &[
            Token::Struct {
                name: "Arrow",
                len: 1,
            },
            Token::Str("t"),
            Token::U8(5),
            Token::StructEnd,
        ],
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Tree {
    label: u8,
    children: Vec<Self>,
}

#[test]
fn self_in_a_field_type_is_the_type_itself() {
    assert_tokens(
        &Tree {
            label: 1,
            children: vec![Tree {
                label: 2,
                children: vec![],
            }],
        },
        &[
            Token::Struct {
                name: "Tree",
                len: 2,
            },
            Token::Str("label"),
            Token::U8(1),
            Token::Str("children"),
            Token::Seq { len: Some(1) },
            Token::Struct {
                name: "Tree",
                len: 2,
            },
            Token::Str("label"),
            Token::U8(2),
            Token::Str("children"),
            Token::Seq { len: Some(0) },
            Token::SeqEnd,
            Token::StructEnd,
            Token::SeqEnd,
            Token::StructEnd,
        ],
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
enum Level {
    Low = 1,
    #[serde(rename = "HIGH")]
    High = 10,
    #[deprecated = "kept so the impl must allow deprecated variants"]
    Old = 20,
}

#[test]
fn discriminants_and_deprecated_variants() {
    assert_tokens(
        &Level::Low,
        &[Token::UnitVariant {
            name: "Level",
            variant: "Low",
        }],
    );
    assert_tokens(
        &Level::High,
        &[Token::UnitVariant {
            name: "Level",
            variant: "HIGH",
        }],
    );
    assert_de_tokens_error::<Level>(
        &[Token::UnitVariant {
            name: "Level",
            variant: "High",
        }],
        "unknown variant `High`, expected one of `Low`, `HIGH`, `Old`",
    );
}

pub trait Bit<X, Y> {
    const BIT: u16;
}

impl Bit<u8, u16> for u8 {
    const BIT: u16 = 32;
}

// 64 for `pick::<u8, u16>()`.
const fn pick<A, B>() -> u16 {
    (size_of::<A>() * 32 + size_of::<B>() * 16) as u16
}

const SHIFT: u32 = 2;

// A discriminant is an expression: its `<` compares and `<<` shifts unless it
// opens generic arguments (a turbofish, a qualified path, a cast's type), and
// only a comma outside those ends it. Deserialize alone, because a variant
// swallowed into the one before it still compiles there and is then refused
// as an unknown name.
#[derive(Debug, PartialEq, Deserialize)]
#[repr(u16)]
enum Shifts {
    A = 1 << SHIFT >> SHIFT,
    B = 1 << 1,
    C = (1 << SHIFT) >> 1 << 2,
    D = if SHIFT < 3 { 16 } else { 0 },
    E = <u8 as Bit<u8, u16>>::BIT,
    F = pick::<u8, u16>(),
    G = SHIFT as u16 >> 1 << 8,
    H,
}

#[test]
fn shifts_and_comparisons_in_a_discriminant_end_at_its_comma() {
    for (variant, value) in [
        ("A", Shifts::A),
        ("B", Shifts::B),
        ("C", Shifts::C),
        ("D", Shifts::D),
        ("E", Shifts::E),
        ("F", Shifts::F),
        ("G", Shifts::G),
        ("H", Shifts::H),
    ] {
        assert_de_tokens(
            &value,
            &[Token::UnitVariant {
                name: "Shifts",
                variant,
            }],
        );
    }
    assert_de_tokens_error::<Shifts>(
        &[Token::UnitVariant {
            name: "Shifts",
            variant: "Z",
        }],
        "unknown variant `Z`, expected one of `A`, `B`, `C`, `D`, `E`, `F`, `G`, `H`",
    );
    assert_eq!(Shifts::G as u16, 256);
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[repr(C, packed)]
struct Packed {
    a: u8,
    b: u32,
}

#[test]
fn a_packed_struct_is_read_by_copy() {
    assert_tokens(
        &Packed { a: 1, b: 2 },
        &[
            Token::Struct {
                name: "Packed",
                len: 2,
            },
            Token::Str("a"),
            Token::U8(1),
            Token::Str("b"),
            Token::U32(2),
            Token::StructEnd,
        ],
    );
}

#[derive(Serialize)]
struct Borrowing<'a, 'b: 'a> {
    name: &'a str,
    list: &'b [u8],
}

#[test]
fn lifetime_parameters_with_bounds() {
    assert_ser_tokens(
        &Borrowing {
            name: "n",
            list: &[1, 2],
        },
        &[
            Token::Struct {
                name: "Borrowing",
                len: 2,
            },
            Token::Str("name"),
            Token::Str("n"),
            Token::Str("list"),
            Token::Seq { len: Some(2) },
            Token::U8(1),
            Token::U8(2),
            Token::SeqEnd,
            Token::StructEnd,
        ],
    );
}
