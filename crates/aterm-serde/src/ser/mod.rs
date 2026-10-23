// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/ser/mod.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Generic data structure serialization framework.
//!
//! Data structures implement [`Serialize`] and data formats implement [`Serializer`].

use crate::lib::*;

mod impls;
mod impossible;

pub use self::impossible::Impossible;

#[doc(no_inline)]
pub use std::error::Error as StdError;

macro_rules! declare_error_trait {
    (Error: Sized $(+ $($supertrait:ident)::+)*) => {
        /// Trait used by [`Serialize`] implementations to construct [`Serializer`] errors.
        #[diagnostic::on_unimplemented(
            message = "the trait bound `{Self}: serde::ser::Error` is not satisfied",
        )]
        pub trait Error: Sized $(+ $($supertrait)::+)* {
            /// Construct an error when a [`Serialize`] implementation fails.
            fn custom<T>(msg: T) -> Self
            where
                T: Display;
        }
    }
}

declare_error_trait!(Error: Sized + StdError);

/// A data structure that can be serialized into any data format supported by Serde.
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::Serialize` is not satisfied",
    note = "for local types consider adding `#[derive(serde::Serialize)]` to your `{Self}` type",
    note = "for types from other crates check whether the crate offers a `serde` feature flag"
)]
pub trait Serialize {
    /// Serialize this value into the given Serde serializer.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer;
}

/// A data format that can serialize any data structure supported by Serde.
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::Serializer` is not satisfied"
)]
pub trait Serializer: Sized {
    /// Output type produced during successful serialization.
    type Ok;

    /// Error type when serialization fails.
    type Error: Error;

    /// State returned from [`serialize_seq`](Self::serialize_seq).
    type SerializeSeq: SerializeSeq<Ok = Self::Ok, Error = Self::Error>;

    /// State returned from [`serialize_tuple`](Self::serialize_tuple).
    type SerializeTuple: SerializeTuple<Ok = Self::Ok, Error = Self::Error>;

    /// State returned from [`serialize_tuple_struct`](Self::serialize_tuple_struct).
    type SerializeTupleStruct: SerializeTupleStruct<Ok = Self::Ok, Error = Self::Error>;

    /// State returned from [`serialize_tuple_variant`](Self::serialize_tuple_variant).
    type SerializeTupleVariant: SerializeTupleVariant<Ok = Self::Ok, Error = Self::Error>;

    /// State returned from [`serialize_map`](Self::serialize_map).
    type SerializeMap: SerializeMap<Ok = Self::Ok, Error = Self::Error>;

    /// State returned from [`serialize_struct`](Self::serialize_struct).
    type SerializeStruct: SerializeStruct<Ok = Self::Ok, Error = Self::Error>;

    /// State returned from [`serialize_struct_variant`](Self::serialize_struct_variant).
    type SerializeStructVariant: SerializeStructVariant<Ok = Self::Ok, Error = Self::Error>;

    /// Serialize a `bool` value.
    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i8` value.
    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i16` value.
    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i32` value.
    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i64` value.
    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i128` value.
    fn serialize_i128(self, v: i128) -> Result<Self::Ok, Self::Error> {
        let _ = v;
        Err(Error::custom("i128 is not supported"))
    }

    /// Serialize a `u8` value.
    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u16` value.
    fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u32` value.
    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u64` value.
    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u128` value.
    fn serialize_u128(self, v: u128) -> Result<Self::Ok, Self::Error> {
        let _ = v;
        Err(Error::custom("u128 is not supported"))
    }

    /// Serialize an `f32` value.
    fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `f64` value.
    fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error>;

    /// Serialize a character.
    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `&str`.
    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error>;

    /// Serialize a chunk of raw byte data.
    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `None` value.
    fn serialize_none(self) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `Some(T)` value.
    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a `()` value.
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error>;

    /// Serialize a unit struct like `struct Unit`.
    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error>;

    /// Serialize a unit variant like `E::A` in `enum E { A, B }`.
    fn serialize_unit_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error>;

    /// Serialize a newtype struct like `struct Millimeters(u8)`.
    fn serialize_newtype_struct<T>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a newtype variant like `E::N` in `enum E { N(u8) }`.
    fn serialize_newtype_variant<T>(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize;

    /// Begin to serialize a variably sized sequence.
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error>;

    /// Begin to serialize a statically sized sequence.
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error>;

    /// Begin to serialize a tuple struct like `struct Rgb(u8, u8, u8)`.
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error>;

    /// Begin to serialize a tuple variant like `E::T` in `enum E { T(u8, u8) }`.
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error>;

    /// Begin to serialize a map.
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error>;

    /// Begin to serialize a struct like `struct Rgb { r: u8, g: u8, b: u8 }`.
    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error>;

    /// Begin to serialize a struct variant like `E::S` in `enum E { S { r: u8, g: u8, b: u8 } }`.
    fn serialize_struct_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error>;

    /// Collect an iterator as a sequence.
    fn collect_seq<I>(self, iter: I) -> Result<Self::Ok, Self::Error>
    where
        I: IntoIterator,
        <I as IntoIterator>::Item: Serialize,
    {
        let mut iter = iter.into_iter();
        let mut serializer = tri!(self.serialize_seq(iterator_len_hint(&iter)));
        tri!(iter.try_for_each(|item| serializer.serialize_element(&item)));
        serializer.end()
    }

    /// Collect an iterator as a map.
    fn collect_map<K, V, I>(self, iter: I) -> Result<Self::Ok, Self::Error>
    where
        K: Serialize,
        V: Serialize,
        I: IntoIterator<Item = (K, V)>,
    {
        let mut iter = iter.into_iter();
        let mut serializer = tri!(self.serialize_map(iterator_len_hint(&iter)));
        tri!(iter.try_for_each(|(key, value)| serializer.serialize_entry(&key, &value)));
        serializer.end()
    }

    /// Serialize a string produced by an implementation of `Display`.
    fn collect_str<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Display,
    {
        self.serialize_str(&value.to_string())
    }

    /// Determine whether `Serialize` implementations should serialize in human-readable form.
    #[inline]
    fn is_human_readable(&self) -> bool {
        true
    }
}

/// Returned from [`Serializer::serialize_seq`].
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeSeq` is not satisfied"
)]
pub trait SerializeSeq {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a sequence element.
    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Finish serializing a sequence.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from [`Serializer::serialize_tuple`].
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeTuple` is not satisfied"
)]
pub trait SerializeTuple {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a tuple element.
    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Finish serializing a tuple.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from [`Serializer::serialize_tuple_struct`].
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeTupleStruct` is not satisfied"
)]
pub trait SerializeTupleStruct {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a tuple struct field.
    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Finish serializing a tuple struct.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from [`Serializer::serialize_tuple_variant`].
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeTupleVariant` is not satisfied"
)]
pub trait SerializeTupleVariant {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a tuple variant field.
    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Finish serializing a tuple variant.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from [`Serializer::serialize_map`].
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeMap` is not satisfied"
)]
pub trait SerializeMap {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a map key.
    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a map value. Must be preceded by [`serialize_key`](Self::serialize_key).
    /// # Panics
    ///
    /// Calling `serialize_value` before `serialize_key` is incorrect and is
    /// allowed to panic or produce bogus results.
    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a map entry consisting of a key and a value.
    fn serialize_entry<K, V>(&mut self, key: &K, value: &V) -> Result<(), Self::Error>
    where
        K: ?Sized + Serialize,
        V: ?Sized + Serialize,
    {
        tri!(self.serialize_key(key));
        self.serialize_value(value)
    }

    /// Serialize a struct field that reaches the map through `#[serde(flatten)]` so formats
    /// without a null representation (such as TOML) can omit `None` fields while rejecting `None` map values.
    fn serialize_field_entry<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.serialize_entry(key, value)
    }

    /// Finish serializing a map.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from [`Serializer::serialize_struct`].
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeStruct` is not satisfied"
)]
pub trait SerializeStruct {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a struct field.
    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Indicate that a struct field has been skipped.
    #[inline]
    fn skip_field(&mut self, key: &'static str) -> Result<(), Self::Error> {
        let _ = key;
        Ok(())
    }

    /// Finish serializing a struct.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from [`Serializer::serialize_struct_variant`].
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeStructVariant` is not satisfied"
)]
pub trait SerializeStructVariant {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a struct variant field.
    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Indicate that a struct variant field has been skipped.
    #[inline]
    fn skip_field(&mut self, key: &'static str) -> Result<(), Self::Error> {
        let _ = key;
        Ok(())
    }

    /// Finish serializing a struct variant.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

fn iterator_len_hint<I>(iter: &I) -> Option<usize>
where
    I: Iterator,
{
    match iter.size_hint() {
        (lo, Some(hi)) if lo == hi => Some(lo),
        _ => None,
    }
}
