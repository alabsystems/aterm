// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/de/value.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Building blocks for deserializing basic values using the `IntoDeserializer`
//! trait.

use crate::lib::*;

use crate::de::{self, IntoDeserializer};
use crate::ser;

// aterm's own (`de/text_key.rs`): a map key read back from the text a
// string-keyed format wrote it as.
pub use crate::de::text_key::{KeyText, TextKeyDeserializer};

////////////////////////////////////////////////////////////////////////////////

// For structs that contain a PhantomData. We do not want the trait
// bound `E: Clone` inferred by derive(Clone).
macro_rules! impl_copy_clone {
    ($ty:ident $(<$lifetime:tt>)*) => {
        impl<$($lifetime,)* E> Copy for $ty<$($lifetime,)* E> {}

        impl<$($lifetime,)* E> Clone for $ty<$($lifetime,)* E> {
            fn clone(&self) -> Self {
                *self
            }
        }
    };
}

////////////////////////////////////////////////////////////////////////////////

/// A minimal representation of all possible errors that can occur using the
/// `IntoDeserializer` trait.
#[derive(Clone, PartialEq)]
pub struct Error {
    err: ErrorImpl,
}

type ErrorImpl = Box<str>;

impl de::Error for Error {
    #[cold]
    fn custom<T>(msg: T) -> Self
    where
        T: Display,
    {
        Error {
            err: msg.to_string().into_boxed_str(),
        }
    }
}

impl ser::Error for Error {
    #[cold]
    fn custom<T>(msg: T) -> Self
    where
        T: Display,
    {
        de::Error::custom(msg)
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(&self.err)
    }
}

impl Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let mut debug = formatter.debug_tuple("Error");
        debug.field(&self.err);
        debug.finish()
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        &self.err
    }
}

/// A deserializer holding a `u32`.
pub struct U32Deserializer<E> {
    value: u32,
    marker: PhantomData<E>,
}

impl_copy_clone!(U32Deserializer);

impl<'de, E> IntoDeserializer<'de, E> for u32
where
    E: de::Error,
{
    type Deserializer = U32Deserializer<E>;

    fn into_deserializer(self) -> U32Deserializer<E> {
        U32Deserializer::new(self)
    }
}

impl<E> U32Deserializer<E> {
    #[allow(missing_docs)]
    #[inline]
    pub const fn new(value: u32) -> Self {
        U32Deserializer {
            value,
            marker: PhantomData,
        }
    }
}

impl<'de, E> de::Deserializer<'de> for U32Deserializer<E>
where
    E: de::Error,
{
    type Error = E;

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_u32(self.value)
    }

    fn deserialize_enum<V>(
        self,
        name: &str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let _ = name;
        let _ = variants;
        visitor.visit_enum(self)
    }
}

impl<'de, E> IntoDeserializer<'de, E> for U32Deserializer<E>
where
    E: de::Error,
{
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

impl<'de, E> de::EnumAccess<'de> for U32Deserializer<E>
where
    E: de::Error,
{
    type Error = E;
    type Variant = private::UnitOnly<E>;

    fn variant_seed<T>(self, seed: T) -> Result<(T::Value, Self::Variant), Self::Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        seed.deserialize(self).map(private::unit_only)
    }
}

impl<E> Debug for U32Deserializer<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter
            .debug_struct("U32Deserializer")
            .field("value", &self.value)
            .finish()
    }
}

////////////////////////////////////////////////////////////////////////////////

/// A deserializer holding a `&str`.
pub struct StrDeserializer<'a, E> {
    value: &'a str,
    marker: PhantomData<E>,
}

impl_copy_clone!(StrDeserializer<'de>);

impl<'de, 'a, E> IntoDeserializer<'de, E> for &'a str
where
    E: de::Error,
{
    type Deserializer = StrDeserializer<'a, E>;

    fn into_deserializer(self) -> StrDeserializer<'a, E> {
        StrDeserializer::new(self)
    }
}

impl<'a, E> StrDeserializer<'a, E> {
    #[allow(missing_docs)]
    #[inline]
    pub const fn new(value: &'a str) -> Self {
        StrDeserializer {
            value,
            marker: PhantomData,
        }
    }
}

impl<'de, 'a, E> de::Deserializer<'de> for StrDeserializer<'a, E>
where
    E: de::Error,
{
    type Error = E;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_str(self.value)
    }

    fn deserialize_enum<V>(
        self,
        name: &str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let _ = name;
        let _ = variants;
        visitor.visit_enum(self)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

impl<'de, 'a, E> IntoDeserializer<'de, E> for StrDeserializer<'a, E>
where
    E: de::Error,
{
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

impl<'de, 'a, E> de::EnumAccess<'de> for StrDeserializer<'a, E>
where
    E: de::Error,
{
    type Error = E;
    type Variant = private::UnitOnly<E>;

    fn variant_seed<T>(self, seed: T) -> Result<(T::Value, Self::Variant), Self::Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        seed.deserialize(self).map(private::unit_only)
    }
}

impl<'a, E> Debug for StrDeserializer<'a, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter
            .debug_struct("StrDeserializer")
            .field("value", &self.value)
            .finish()
    }
}

/// A deserializer holding a `SeqAccess`.
#[derive(Clone, Debug)]
pub struct SeqAccessDeserializer<A> {
    seq: A,
}

impl<A> SeqAccessDeserializer<A> {
    /// Construct a new `SeqAccessDeserializer<A>`.
    #[inline]
    pub const fn new(seq: A) -> Self {
        SeqAccessDeserializer { seq }
    }
}

impl<'de, A> de::Deserializer<'de> for SeqAccessDeserializer<A>
where
    A: de::SeqAccess<'de>,
{
    type Error = A::Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_seq(self.seq)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

impl<'de, A> IntoDeserializer<'de, A::Error> for SeqAccessDeserializer<A>
where
    A: de::SeqAccess<'de>,
{
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

////////////////////////////////////////////////////////////////////////////////

/// A deserializer holding a `MapAccess`.
#[derive(Clone, Debug)]
pub struct MapAccessDeserializer<A> {
    map: A,
}

impl<A> MapAccessDeserializer<A> {
    /// Construct a new `MapAccessDeserializer<A>`.
    #[inline]
    pub const fn new(map: A) -> Self {
        MapAccessDeserializer { map }
    }
}

impl<'de, A> de::Deserializer<'de> for MapAccessDeserializer<A>
where
    A: de::MapAccess<'de>,
{
    type Error = A::Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_map(self.map)
    }

    fn deserialize_enum<V>(
        self,
        _name: &str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_enum(self)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

impl<'de, A> IntoDeserializer<'de, A::Error> for MapAccessDeserializer<A>
where
    A: de::MapAccess<'de>,
{
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

impl<'de, A> de::EnumAccess<'de> for MapAccessDeserializer<A>
where
    A: de::MapAccess<'de>,
{
    type Error = A::Error;
    type Variant = private::MapAsEnum<A>;

    fn variant_seed<T>(mut self, seed: T) -> Result<(T::Value, Self::Variant), Self::Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        match tri!(self.map.next_key_seed(seed)) {
            Some(key) => Ok((key, private::map_as_enum(self.map))),
            None => Err(de::Error::invalid_type(de::Unexpected::Map, &"enum")),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

mod private {
    use crate::lib::*;

    use crate::de::{
        self, DeserializeSeed, Deserializer, MapAccess, Unexpected, VariantAccess, Visitor,
    };

    pub struct UnitOnly<E> {
        marker: PhantomData<E>,
    }

    pub const fn unit_only<T, E>(t: T) -> (T, UnitOnly<E>) {
        (
            t,
            UnitOnly {
                marker: PhantomData,
            },
        )
    }

    impl<'de, E> de::VariantAccess<'de> for UnitOnly<E>
    where
        E: de::Error,
    {
        type Error = E;

        fn unit_variant(self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn newtype_variant_seed<T>(self, _seed: T) -> Result<T::Value, Self::Error>
        where
            T: de::DeserializeSeed<'de>,
        {
            Err(de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"newtype variant",
            ))
        }

        fn tuple_variant<V>(self, _len: usize, _visitor: V) -> Result<V::Value, Self::Error>
        where
            V: de::Visitor<'de>,
        {
            Err(de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"tuple variant",
            ))
        }

        fn struct_variant<V>(
            self,
            _fields: &'static [&'static str],
            _visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: de::Visitor<'de>,
        {
            Err(de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"struct variant",
            ))
        }
    }

    pub struct MapAsEnum<A> {
        map: A,
    }

    pub const fn map_as_enum<A>(map: A) -> MapAsEnum<A> {
        MapAsEnum { map }
    }

    impl<'de, A> VariantAccess<'de> for MapAsEnum<A>
    where
        A: MapAccess<'de>,
    {
        type Error = A::Error;

        fn unit_variant(mut self) -> Result<(), Self::Error> {
            self.map.next_value()
        }

        fn newtype_variant_seed<T>(mut self, seed: T) -> Result<T::Value, Self::Error>
        where
            T: DeserializeSeed<'de>,
        {
            self.map.next_value_seed(seed)
        }

        fn tuple_variant<V>(mut self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.map.next_value_seed(SeedTupleVariant { len, visitor })
        }

        fn struct_variant<V>(
            mut self,
            _fields: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.map.next_value_seed(SeedStructVariant { visitor })
        }
    }

    struct SeedTupleVariant<V> {
        len: usize,
        visitor: V,
    }

    impl<'de, V> DeserializeSeed<'de> for SeedTupleVariant<V>
    where
        V: Visitor<'de>,
    {
        type Value = V::Value;

        fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_tuple(self.len, self.visitor)
        }
    }

    struct SeedStructVariant<V> {
        visitor: V,
    }

    impl<'de, V> DeserializeSeed<'de> for SeedStructVariant<V>
    where
        V: Visitor<'de>,
    {
        type Value = V::Value;

        fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_map(self.visitor)
        }
    }
}
