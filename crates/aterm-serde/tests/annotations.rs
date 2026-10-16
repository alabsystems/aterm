// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_annotations.rs (git tag v1.0.228) and
// modified by the aterm project in 2026: ported onto aterm-serde's surface.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

#![allow(
    clippy::cast_lossless,
    clippy::derive_partial_eq_without_eq,
    clippy::from_over_into,
    // Clippy bug: https://github.com/rust-lang/rust-clippy/issues/7422
    clippy::nonstandard_macro_braces,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::type_repetition_in_bounds,
    clippy::uninlined_format_args,
)]

use crate::support::{
    Token, assert_de_tokens, assert_de_tokens_error, assert_ser_tokens, assert_ser_tokens_error,
    assert_tokens,
};
use aterm_serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_derive::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct CollectOther {
    a: u32,
    b: u32,
    #[serde(flatten)]
    extra: HashMap<String, u32>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasStruct {
    a1: i32,
    #[serde(alias = "a3")]
    a2: i32,
    #[serde(alias = "a5", rename = "a6")]
    a4: i32,
}

#[test]
fn test_unknown_field_rename_struct() {
    assert_de_tokens_error::<AliasStruct>(
        &[
            Token::Struct {
                name: "AliasStruct",
                len: 3,
            },
            Token::Str("a1"),
            Token::I32(1),
            Token::Str("a3"),
            Token::I32(2),
            Token::Str("a4"),
            Token::I32(3),
        ],
        "unknown field `a4`, expected one of `a1`, `a2`, `a3`, `a5`, `a6`",
    );
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
enum AliasEnum {
    #[serde(rename = "sailor_moon")]
    SailorMoon {
        a: i8,
        #[serde(alias = "c")]
        b: i8,
        #[serde(alias = "e", rename = "f")]
        d: i8,
    },
}

#[test]
fn test_unknown_field_rename_enum() {
    assert_de_tokens_error::<AliasEnum>(
        &[Token::StructVariant {
            name: "AliasEnum",
            variant: "SailorMoon",
            len: 3,
        }],
        "unknown variant `SailorMoon`, expected `sailor_moon`",
    );

    assert_de_tokens_error::<AliasEnum>(
        &[
            Token::StructVariant {
                name: "AliasEnum",
                variant: "sailor_moon",
                len: 5,
            },
            Token::Str("a"),
            Token::I8(0),
            Token::Str("c"),
            Token::I8(1),
            Token::Str("d"),
            Token::I8(2),
        ],
        "unknown field `d`, expected one of `a`, `b`, `c`, `e`, `f`",
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct SkipStruct<B> {
    a: i8,
    #[serde(skip)]
    b: B,
}

#[test]
fn test_skip_struct() {
    assert_ser_tokens(
        &SkipStruct { a: 1, b: 2 },
        &[
            Token::Struct {
                name: "SkipStruct",
                len: 1,
            },
            Token::Str("a"),
            Token::I8(1),
            Token::StructEnd,
        ],
    );

    assert_de_tokens(
        &SkipStruct { a: 1, b: 0 },
        &[
            Token::Struct {
                name: "SkipStruct",
                len: 1,
            },
            Token::Str("a"),
            Token::I8(1),
            Token::StructEnd,
        ],
    );
}

#[test]
fn test_collect_other() {
    let mut extra = HashMap::new();
    extra.insert("c".into(), 3);
    assert_tokens(
        &CollectOther { a: 1, b: 2, extra },
        &[
            Token::Map { len: None },
            Token::Str("a"),
            Token::U32(1),
            Token::Str("b"),
            Token::U32(2),
            Token::Str("c"),
            Token::U32(3),
            Token::MapEnd,
        ],
    );
}

// Upstream's test_partially_untagged_enum_desugared, kept on its desugared
// half: aterm writes `untagged` on containers only, never on a variant.
#[test]
fn test_untagged_enum_holding_a_tagged_enum() {
    mod tagged {
        use super::*;
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        pub(super) enum Test {
            A(u32, u32),
            B(u32),
        }
    }
    use tagged::Test as TestTagged;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    #[serde(untagged)]
    enum TestUntagged {
        Tagged(TestTagged),
        C(u32),
        D(u32, u32),
    }

    assert_tokens(
        &TestUntagged::Tagged(TestTagged::A(0, 1)),
        &[
            Token::TupleVariant {
                name: "Test",
                variant: "A",
                len: 2,
            },
            Token::U32(0),
            Token::U32(1),
            Token::TupleVariantEnd,
        ],
    );

    assert_tokens(
        &TestUntagged::Tagged(TestTagged::B(1)),
        &[
            Token::NewtypeVariant {
                name: "Test",
                variant: "B",
            },
            Token::U32(1),
        ],
    );

    assert_tokens(&TestUntagged::C(2), &[Token::U32(2)]);

    assert_tokens(
        &TestUntagged::D(3, 5),
        &[
            Token::Tuple { len: 2 },
            Token::U32(3),
            Token::U32(5),
            Token::TupleEnd,
        ],
    );
}

mod flatten {
    use super::*;

    #[test]
    fn complex() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Outer {
            y: u32,
            #[serde(flatten)]
            first: First,
            #[serde(flatten)]
            second: Second,
            z: u32,
        }

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct First {
            a: u32,
            b: bool,
            c: Vec<String>,
            d: String,
            e: Option<u64>,
        }

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Second {
            f: u32,
        }

        assert_de_tokens(
            &Outer {
                y: 0,
                first: First {
                    a: 1,
                    b: true,
                    c: vec!["a".into(), "b".into()],
                    d: "c".into(),
                    e: Some(2),
                },
                second: Second { f: 3 },
                z: 4,
            },
            &[
                Token::Map { len: None },
                Token::Str("y"),
                Token::U32(0),
                Token::Str("a"),
                Token::U32(1),
                Token::Str("b"),
                Token::Bool(true),
                Token::Str("c"),
                Token::Seq { len: Some(2) },
                Token::Str("a"),
                Token::Str("b"),
                Token::SeqEnd,
                Token::Str("d"),
                Token::Str("c"),
                Token::Str("e"),
                Token::U64(2),
                Token::Str("f"),
                Token::U32(3),
                Token::Str("z"),
                Token::U32(4),
                Token::MapEnd,
            ],
        );

        assert_ser_tokens(
            &Outer {
                y: 0,
                first: First {
                    a: 1,
                    b: true,
                    c: vec!["a".into(), "b".into()],
                    d: "c".into(),
                    e: Some(2),
                },
                second: Second { f: 3 },
                z: 4,
            },
            &[
                Token::Map { len: None },
                Token::Str("y"),
                Token::U32(0),
                Token::Str("a"),
                Token::U32(1),
                Token::Str("b"),
                Token::Bool(true),
                Token::Str("c"),
                Token::Seq { len: Some(2) },
                Token::Str("a"),
                Token::Str("b"),
                Token::SeqEnd,
                Token::Str("d"),
                Token::Str("c"),
                Token::Str("e"),
                Token::Some,
                Token::U64(2),
                Token::Str("f"),
                Token::U32(3),
                Token::Str("z"),
                Token::U32(4),
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn map_twice() {
        #[derive(Debug, PartialEq, Deserialize)]
        struct Outer {
            #[serde(flatten)]
            first: BTreeMap<String, String>,
            #[serde(flatten)]
            between: Inner,
            #[serde(flatten)]
            second: BTreeMap<String, String>,
        }

        #[derive(Debug, PartialEq, Deserialize)]
        struct Inner {
            y: String,
        }

        assert_de_tokens(
            &Outer {
                first: {
                    let mut first = BTreeMap::new();
                    first.insert("x".to_owned(), "X".to_owned());
                    first.insert("y".to_owned(), "Y".to_owned());
                    first
                },
                between: Inner { y: "Y".to_owned() },
                second: {
                    let mut second = BTreeMap::new();
                    second.insert("x".to_owned(), "X".to_owned());
                    second
                },
            },
            &[
                Token::Map { len: None },
                Token::Str("x"),
                Token::Str("X"),
                Token::Str("y"),
                Token::Str("Y"),
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn unsupported_type() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Outer {
            outer: String,
            #[serde(flatten)]
            inner: String,
        }

        assert_ser_tokens_error(
            &Outer {
                outer: "foo".into(),
                inner: "bar".into(),
            },
            &[
                Token::Map { len: None },
                Token::Str("outer"),
                Token::Str("foo"),
            ],
            "can only flatten structs and maps (got a string)",
        );
        assert_de_tokens_error::<Outer>(
            &[
                Token::Map { len: None },
                Token::Str("outer"),
                Token::Str("foo"),
                Token::Str("a"),
                Token::Str("b"),
                Token::MapEnd,
            ],
            "can only flatten structs and maps",
        );
    }

    #[test]
    fn unknown_field() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Outer {
            dummy: String,
            #[serde(flatten)]
            inner: Inner,
        }

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Inner {
            foo: HashMap<String, u32>,
        }

        assert_de_tokens_error::<Outer>(
            &[
                Token::Struct {
                    name: "Outer",
                    len: 1,
                },
                Token::Str("dummy"),
                Token::Str("23"),
                Token::Str("foo"),
                Token::Map { len: None },
                Token::Str("a"),
                Token::U32(1),
                Token::Str("b"),
                Token::U32(2),
                Token::MapEnd,
                Token::Str("bar"),
                Token::U32(23),
                Token::StructEnd,
            ],
            "unknown field `bar`",
        );
    }

    #[test]
    fn non_string_keys() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct TestStruct {
            name: String,
            age: u32,
            #[serde(flatten)]
            mapping: HashMap<u32, u32>,
        }

        let mut mapping = HashMap::new();
        mapping.insert(0, 42);
        assert_tokens(
            &TestStruct {
                name: "peter".into(),
                age: 3,
                mapping,
            },
            &[
                Token::Map { len: None },
                Token::Str("name"),
                Token::Str("peter"),
                Token::Str("age"),
                Token::U32(3),
                Token::U32(0),
                Token::U32(42),
                Token::MapEnd,
            ],
        );
    }

    // Regression test for https://github.com/serde-rs/serde/issues/1904
    #[test]
    fn flatten_any_after_flatten_struct() {
        #[derive(PartialEq, Debug)]
        struct Any;

        impl<'de> Deserialize<'de> for Any {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct AnyVisitor;

                impl<'de> Visitor<'de> for AnyVisitor {
                    type Value = Any;

                    fn expecting(&self, _formatter: &mut fmt::Formatter) -> fmt::Result {
                        unimplemented!()
                    }

                    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
                    where
                        M: MapAccess<'de>,
                    {
                        while let Some((Any, Any)) = map.next_entry()? {}
                        Ok(Any)
                    }
                }

                deserializer.deserialize_any(AnyVisitor)
            }
        }

        #[derive(Deserialize, PartialEq, Debug)]
        struct Outer {
            #[serde(flatten)]
            inner: Inner,
            #[serde(flatten)]
            extra: Any,
        }

        #[derive(Deserialize, PartialEq, Debug)]
        struct Inner {
            inner: i32,
        }

        let s = Outer {
            inner: Inner { inner: 0 },
            extra: Any,
        };

        assert_de_tokens(
            &s,
            &[
                Token::Map { len: None },
                Token::Str("inner"),
                Token::I32(0),
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn enum_tuple_and_struct() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        enum Outer {
            Tuple(f64, i32),
            Flatten {
                #[serde(flatten)]
                nested: Nested,
            },
        }

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Nested {
            a: i32,
            b: i32,
        }

        assert_tokens(
            &Outer::Tuple(1.2, 3),
            &[
                Token::TupleVariant {
                    name: "Outer",
                    variant: "Tuple",
                    len: 2,
                },
                Token::F64(1.2),
                Token::I32(3),
                Token::TupleVariantEnd,
            ],
        );
        assert_tokens(
            &Outer::Flatten {
                nested: Nested { a: 1, b: 2 },
            },
            &[
                Token::NewtypeVariant {
                    name: "Outer",
                    variant: "Flatten",
                },
                Token::Map { len: None },
                Token::Str("a"),
                Token::I32(1),
                Token::Str("b"),
                Token::I32(2),
                Token::MapEnd,
            ],
        );
    }

    #[test]
    fn option() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Outer {
            #[serde(flatten)]
            inner1: Option<Inner1>,
            #[serde(flatten)]
            inner2: Option<Inner2>,
        }

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Inner1 {
            inner1: i32,
        }

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Inner2 {
            inner2: i32,
        }

        assert_tokens(
            &Outer {
                inner1: Some(Inner1 { inner1: 1 }),
                inner2: Some(Inner2 { inner2: 2 }),
            },
            &[
                Token::Map { len: None },
                Token::Str("inner1"),
                Token::I32(1),
                Token::Str("inner2"),
                Token::I32(2),
                Token::MapEnd,
            ],
        );

        assert_tokens(
            &Outer {
                inner1: Some(Inner1 { inner1: 1 }),
                inner2: None,
            },
            &[
                Token::Map { len: None },
                Token::Str("inner1"),
                Token::I32(1),
                Token::MapEnd,
            ],
        );

        assert_tokens(
            &Outer {
                inner1: None,
                inner2: Some(Inner2 { inner2: 2 }),
            },
            &[
                Token::Map { len: None },
                Token::Str("inner2"),
                Token::I32(2),
                Token::MapEnd,
            ],
        );

        assert_tokens(
            &Outer {
                inner1: None,
                inner2: None,
            },
            &[Token::Map { len: None }, Token::MapEnd],
        );
    }

    #[test]
    fn ignored_any() {
        #[derive(Deserialize, PartialEq, Debug)]
        struct Outer {
            #[serde(flatten)]
            inner: IgnoredAny,
        }

        assert_de_tokens(
            &Outer { inner: IgnoredAny },
            &[Token::Map { len: None }, Token::MapEnd],
        );

        assert_de_tokens(
            &Outer { inner: IgnoredAny },
            &[
                Token::Struct {
                    name: "DoNotMatter",
                    len: 0,
                },
                Token::StructEnd,
            ],
        );
    }

    #[test]
    fn alias() {
        #[derive(Debug, PartialEq, Deserialize)]
        struct Outer {
            #[serde(flatten)]
            a: AliasStruct,
            b: i32,
        }

        assert_de_tokens(
            &Outer {
                a: AliasStruct {
                    a1: 1,
                    a2: 2,
                    a4: 4,
                },
                b: 7,
            },
            &[
                Token::Struct {
                    name: "Outer",
                    len: 4,
                },
                Token::Str("a1"),
                Token::I32(1),
                Token::Str("a2"),
                Token::I32(2),
                Token::Str("a5"),
                Token::I32(4),
                Token::Str("b"),
                Token::I32(7),
                Token::StructEnd,
            ],
        );

        assert_de_tokens(
            &Outer {
                a: AliasStruct {
                    a1: 1,
                    a2: 2,
                    a4: 4,
                },
                b: 7,
            },
            &[
                Token::Struct {
                    name: "Outer",
                    len: 4,
                },
                Token::Str("a1"),
                Token::I32(1),
                Token::Str("a2"),
                Token::I32(2),
                Token::Str("a6"),
                Token::I32(4),
                Token::Str("b"),
                Token::I32(7),
                Token::StructEnd,
            ],
        );
    }

    mod unit {
        use super::*;

        #[test]
        fn unit() {
            #[derive(Debug, PartialEq, Serialize, Deserialize)]
            struct Response<T> {
                #[serde(flatten)]
                data: T,
                status: usize,
            }

            assert_tokens(
                &Response {
                    data: (),
                    status: 0,
                },
                &[
                    Token::Map { len: None },
                    Token::Str("status"),
                    Token::U64(0),
                    Token::MapEnd,
                ],
            );
        }

        #[test]
        fn unit_struct() {
            #[derive(Debug, PartialEq, Serialize, Deserialize)]
            struct Response<T> {
                #[serde(flatten)]
                data: T,
                status: usize,
            }

            #[derive(Debug, PartialEq, Serialize, Deserialize)]
            struct Unit;

            assert_tokens(
                &Response {
                    data: Unit,
                    status: 0,
                },
                &[
                    Token::Map { len: None },
                    Token::Str("status"),
                    Token::U64(0),
                    Token::MapEnd,
                ],
            );
        }
    }

    mod enum_ {
        use super::*;

        mod externally_tagged {
            use super::*;

            use std::iter::FromIterator;

            #[test]
            fn straightforward() {
                #[derive(Serialize, Deserialize, PartialEq, Debug)]
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
                        Token::NewtypeVariant {
                            name: "Data",
                            variant: "A",
                        },
                        Token::Map { len: None },
                        Token::Str("a"),
                        Token::I32(0),
                        Token::Str("b"),
                        Token::I32(0),
                        Token::MapEnd,
                    ],
                );
            }

            #[derive(Debug, PartialEq, Serialize, Deserialize)]
            struct Flatten {
                #[serde(flatten)]
                data: Enum,

                #[serde(flatten)]
                extra: HashMap<String, String>,
            }

            #[derive(Debug, PartialEq, Serialize, Deserialize)]
            enum Enum {
                Unit,
                Newtype(HashMap<String, String>),
                Tuple(u32, u32),
                Struct { index: u32, value: u32 },
            }

            #[test]
            fn unit() {
                let value = Flatten {
                    data: Enum::Unit,
                    extra: HashMap::from_iter([("extra_key".into(), "extra value".into())]),
                };
                assert_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // data
                        Token::Str("Unit"), // variant
                        Token::Unit,
                        // extra
                        Token::Str("extra_key"),
                        Token::Str("extra value"),
                        Token::MapEnd,
                    ],
                );
                assert_de_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // extra
                        Token::Str("extra_key"),
                        Token::Str("extra value"),
                        // data
                        Token::Str("Unit"), // variant
                        Token::Unit,
                        Token::MapEnd,
                    ],
                );
            }

            #[test]
            fn newtype() {
                let value = Flatten {
                    data: Enum::Newtype(HashMap::from_iter([("key".into(), "value".into())])),
                    extra: HashMap::from_iter([("extra_key".into(), "extra value".into())]),
                };
                assert_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // data
                        Token::Str("Newtype"), // variant
                        Token::Map { len: Some(1) },
                        Token::Str("key"),
                        Token::Str("value"),
                        Token::MapEnd,
                        // extra
                        Token::Str("extra_key"),
                        Token::Str("extra value"),
                        Token::MapEnd,
                    ],
                );
                assert_de_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // extra
                        Token::Str("extra_key"),
                        Token::Str("extra value"),
                        // data
                        Token::Str("Newtype"), // variant
                        Token::Map { len: Some(1) },
                        Token::Str("key"),
                        Token::Str("value"),
                        Token::MapEnd,
                        Token::MapEnd,
                    ],
                );
            }
        }

        mod internally_tagged {
            use super::*;

            #[test]
            fn structs() {
                #[derive(Debug, PartialEq, Serialize, Deserialize)]
                struct Flatten {
                    #[serde(flatten)]
                    x: X,
                    #[serde(flatten)]
                    y: Y,
                }

                #[derive(Debug, PartialEq, Serialize, Deserialize)]
                #[serde(tag = "typeX")]
                enum X {
                    A { a: i32 },
                    B { b: i32 },
                }

                #[derive(Debug, PartialEq, Serialize, Deserialize)]
                #[serde(tag = "typeY")]
                enum Y {
                    C { c: i32 },
                    D { d: i32 },
                }

                let value = Flatten {
                    x: X::B { b: 1 },
                    y: Y::D { d: 2 },
                };
                assert_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // x
                        Token::Str("typeX"),
                        Token::Str("B"),
                        Token::Str("b"),
                        Token::I32(1),
                        // y
                        Token::Str("typeY"),
                        Token::Str("D"),
                        Token::Str("d"),
                        Token::I32(2),
                        Token::MapEnd,
                    ],
                );
                assert_de_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // y
                        Token::Str("typeY"),
                        Token::Str("D"),
                        Token::Str("d"),
                        Token::I32(2),
                        // x
                        Token::Str("typeX"),
                        Token::Str("B"),
                        Token::Str("b"),
                        Token::I32(1),
                        Token::MapEnd,
                    ],
                );
            }

            #[test]
            fn unit_enum_with_unknown_fields() {
                #[derive(Debug, PartialEq, Serialize, Deserialize)]
                struct Flatten {
                    #[serde(flatten)]
                    x: X,
                    #[serde(flatten)]
                    y: Y,
                }

                #[derive(Debug, PartialEq, Serialize, Deserialize)]
                #[serde(tag = "typeX")]
                enum X {
                    A,
                }

                #[derive(Debug, PartialEq, Serialize, Deserialize)]
                #[serde(tag = "typeY")]
                enum Y {
                    B { c: u32 },
                }

                let value = Flatten {
                    x: X::A,
                    y: Y::B { c: 0 },
                };
                assert_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // x
                        Token::Str("typeX"),
                        Token::Str("A"),
                        // y
                        Token::Str("typeY"),
                        Token::Str("B"),
                        Token::Str("c"),
                        Token::U32(0),
                        Token::MapEnd,
                    ],
                );
                assert_de_tokens(
                    &value,
                    &[
                        Token::Map { len: None },
                        // y
                        Token::Str("typeY"),
                        Token::Str("B"),
                        Token::Str("c"),
                        Token::U32(0),
                        // x
                        Token::Str("typeX"),
                        Token::Str("A"),
                        Token::MapEnd,
                    ],
                );
            }
        }

        mod untagged {
            use super::*;

            #[derive(Debug, PartialEq, Serialize, Deserialize)]
            struct Flatten {
                #[serde(flatten)]
                data: Enum,
            }

            #[derive(Debug, PartialEq, Serialize, Deserialize)]
            #[serde(untagged)]
            enum Enum {
                Struct { a: i32 },
            }

            #[test]
            fn struct_() {
                assert_tokens(
                    &Flatten {
                        data: Enum::Struct { a: 0 },
                    },
                    &[
                        Token::Map { len: None },
                        Token::Str("a"),
                        Token::I32(0),
                        Token::MapEnd,
                    ],
                );
            }
        }
    }
}
