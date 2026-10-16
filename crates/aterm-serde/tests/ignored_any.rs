// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/test_ignored_any.rs (git tag v1.0.228) and
// modified by the aterm project in 2026: ported onto aterm-serde's surface.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

#![allow(clippy::derive_partial_eq_without_eq)]

use crate::support::{Error, Token};
use aterm_serde::de::{
    Deserialize, DeserializeSeed, Deserializer, EnumAccess, IgnoredAny, IntoDeserializer,
    VariantAccess, Visitor,
};
use aterm_serde::forward_to_deserialize_any;
use serde_derive::Deserialize;

#[derive(PartialEq, Debug, Deserialize)]
enum Target {
    Unit,
    Newtype(i32),
    Tuple(i32, i32),
    Struct { a: i32 },
}

struct Enum(&'static str);

impl<'de> Deserializer<'de> for Enum {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_enum(self)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

// The variant's payload is replayed from tokens: upstream builds it from
// `i32::into_deserializer`, `SeqDeserializer` and `MapDeserializer`, which
// this crate dropped (nothing in aterm called them).
fn replay<'de, T: DeserializeSeed<'de>>(seed: T, tokens: &'de [Token]) -> Result<T::Value, Error> {
    seed.deserialize(&mut crate::support::Deserializer::new(tokens))
}

impl<'de> EnumAccess<'de> for Enum {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let v = seed.deserialize(IntoDeserializer::<Error>::into_deserializer(self.0))?;
        Ok((v, self))
    }
}

impl<'de> VariantAccess<'de> for Enum {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        replay(seed, &[Token::I32(10)])
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let tokens = &[
            Token::Seq { len: Some(2) },
            Token::I32(1),
            Token::I32(2),
            Token::SeqEnd,
        ];
        (&mut crate::support::Deserializer::new(tokens)).deserialize_any(visitor)
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let tokens = &[
            Token::Map { len: Some(1) },
            Token::Str("a"),
            Token::I32(10),
            Token::MapEnd,
        ];
        (&mut crate::support::Deserializer::new(tokens)).deserialize_any(visitor)
    }
}

#[test]
fn test_deserialize_enum() {
    // First just make sure the Deserializer impl works
    assert_eq!(Target::Unit, Target::deserialize(Enum("Unit")).unwrap());
    assert_eq!(
        Target::Newtype(10),
        Target::deserialize(Enum("Newtype")).unwrap()
    );
    assert_eq!(
        Target::Tuple(1, 2),
        Target::deserialize(Enum("Tuple")).unwrap()
    );
    assert_eq!(
        Target::Struct { a: 10 },
        Target::deserialize(Enum("Struct")).unwrap()
    );

    // Now try IgnoredAny
    IgnoredAny::deserialize(Enum("Unit")).unwrap();
    IgnoredAny::deserialize(Enum("Newtype")).unwrap();
    IgnoredAny::deserialize(Enum("Tuple")).unwrap();
    IgnoredAny::deserialize(Enum("Struct")).unwrap();
}
