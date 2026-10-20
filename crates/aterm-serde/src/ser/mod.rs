// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/ser/mod.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Generic data structure serialization framework.
//!
//! The two most important traits in this module are [`Serialize`] and
//! [`Serializer`].
//!
//!  - **A type that implements `Serialize` is a data structure** that can be
//!    serialized to any data format supported by Serde, and conversely
//!  - **A type that implements `Serializer` is a data format** that can
//!    serialize any data structure supported by Serde.
//!
//! # The Serialize trait
//!
//! Serde provides [`Serialize`] implementations for many Rust primitive and
//! standard library types. The complete list is below. All of these can be
//! serialized using Serde out of the box.
//!
//! Additionally, Serde provides a procedural macro called [`serde_derive`] to
//! automatically generate [`Serialize`] implementations for structs and enums
//! in your program. See the [derive section of the manual] for how to use this.
//!
//! In rare cases it may be necessary to implement [`Serialize`] manually for
//! some type in your program. See the [Implementing `Serialize`] section of the
//! manual for more about this.
//!
//! Third-party crates may provide [`Serialize`] implementations for types that
//! they expose. For example the [`linked-hash-map`] crate provides a
//! [`LinkedHashMap<K, V>`] type that is serializable by Serde because the crate
//! provides an implementation of [`Serialize`] for it.
//!
//! # The Serializer trait
//!
//! [`Serializer`] implementations are provided by third-party crates, for
//! example [`serde_json`], [`serde_yaml`] and [`postcard`].
//!
//! A partial list of well-maintained formats is given on the [Serde
//! website][data formats].
//!
//! # Implementations of Serialize provided here
//!
//! The std types aterm's models carry (docs/AUDIT-third-party-2026-10-15.md,
//! SP.4); upstream's other impls were left behind with nothing to call them.
//!
//!  - **Primitive types**:
//!    - bool
//!    - i8, i16, i32, i64, i128, isize
//!    - u8, u16, u32, u64, u128, usize
//!    - f32, f64
//!    - char
//!    - str
//!    - &T and &mut T
//!  - **Compound types**:
//!    - \[T\]
//!    - \[T; 0\] through \[T; 32\]
//!    - tuples up to size 16
//!  - **Common standard library types**:
//!    - ()
//!    - String
//!    - Option\<T\>
//!    - Box\<T\>
//!    - Vec\<T\>
//!    - BTreeMap\<K, V\>
//!    - HashMap\<K, V, H\>
//!    - Path, PathBuf
//!
//! [Implementing `Serialize`]: https://serde.rs/impl-serialize.html
//! [`LinkedHashMap<K, V>`]: https://docs.rs/linked-hash-map/*/linked_hash_map/struct.LinkedHashMap.html
//! [`Serialize`]: crate::Serialize
//! [`Serializer`]: crate::Serializer
//! [`postcard`]: https://github.com/jamesmunns/postcard
//! [`linked-hash-map`]: https://crates.io/crates/linked-hash-map
//! [`serde_derive`]: https://crates.io/crates/serde_derive
//! [`serde_json`]: https://github.com/serde-rs/json
//! [`serde_yaml`]: https://github.com/dtolnay/serde-yaml
//! [derive section of the manual]: https://serde.rs/derive.html
//! [data formats]: https://serde.rs/#data-formats

use crate::lib::*;

mod impls;
mod impossible;

pub use self::impossible::Impossible;

#[doc(no_inline)]
pub use std::error::Error as StdError;

////////////////////////////////////////////////////////////////////////////////

macro_rules! declare_error_trait {
    (Error: Sized $(+ $($supertrait:ident)::+)*) => {
        /// Trait used by `Serialize` implementations to generically construct
        /// errors belonging to the `Serializer` against which they are
        /// currently running.
        ///
        /// # Example implementation
        ///
        /// The [example data format] presented on the website shows an error
        /// type appropriate for a basic JSON data format.
        ///
        /// [example data format]: https://serde.rs/data-format.html
        #[diagnostic::on_unimplemented(
            message = "the trait bound `{Self}: serde::ser::Error` is not satisfied",
        )]
        pub trait Error: Sized $(+ $($supertrait)::+)* {
            /// Used when a [`Serialize`] implementation encounters any error
            /// while serializing a type.
            ///
            /// The message should not be capitalized and should not end with a
            /// period.
            ///
            /// For example, a filesystem [`Path`] may refuse to serialize
            /// itself if it contains invalid UTF-8 data.
            ///
            /// [`Path`]: std::path::Path
            /// [`Serialize`]: crate::Serialize
            fn custom<T>(msg: T) -> Self
            where
                T: Display;
        }
    }
}

declare_error_trait!(Error: Sized + StdError);

////////////////////////////////////////////////////////////////////////////////

/// A **data structure** that can be serialized into any data format supported
/// by Serde.
///
/// Serde provides `Serialize` implementations for many Rust primitive and
/// standard library types. The complete list is [here][crate::ser]. All of
/// these can be serialized using Serde out of the box.
///
/// Additionally, Serde provides a procedural macro called [`serde_derive`] to
/// automatically generate `Serialize` implementations for structs and enums in
/// your program. See the [derive section of the manual] for how to use this.
///
/// In rare cases it may be necessary to implement `Serialize` manually for some
/// type in your program. See the [Implementing `Serialize`] section of the
/// manual for more about this.
///
/// Third-party crates may provide `Serialize` implementations for types that
/// they expose. For example the [`linked-hash-map`] crate provides a
/// [`LinkedHashMap<K, V>`] type that is serializable by Serde because the crate
/// provides an implementation of `Serialize` for it.
///
/// [Implementing `Serialize`]: https://serde.rs/impl-serialize.html
/// [`LinkedHashMap<K, V>`]: https://docs.rs/linked-hash-map/*/linked_hash_map/struct.LinkedHashMap.html
/// [`linked-hash-map`]: https://crates.io/crates/linked-hash-map
/// [`serde_derive`]: https://crates.io/crates/serde_derive
/// [derive section of the manual]: https://serde.rs/derive.html
// Names the trait as dependents spell it: `serde::`, the workspace
// row's rename, not `aterm_serde::ser::Serialize`.
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::Serialize` is not satisfied",
    note = "for local types consider adding `#[derive(serde::Serialize)]` to your `{Self}` type",
    note = "for types from other crates check whether the crate offers a `serde` feature flag"
)]
pub trait Serialize {
    /// Serialize this value into the given Serde serializer.
    ///
    /// See the [Implementing `Serialize`] section of the manual for more
    /// information about how to implement this method.
    ///
    /// [Implementing `Serialize`]: https://serde.rs/impl-serialize.html
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer;
}

////////////////////////////////////////////////////////////////////////////////

/// A **data format** that can serialize any data structure supported by Serde.
///
/// The role of this trait is to define the serialization half of the [Serde
/// data model], which is a way to categorize every Rust data structure into one
/// of 29 possible types. Each method of the `Serializer` trait corresponds to
/// one of the types of the data model.
///
/// Implementations of `Serialize` map themselves into this data model by
/// invoking exactly one of the `Serializer` methods.
///
/// The types that make up the Serde data model are:
///
///  - **14 primitive types**
///    - bool
///    - i8, i16, i32, i64, i128
///    - u8, u16, u32, u64, u128
///    - f32, f64
///    - char
///  - **string**
///    - UTF-8 bytes with a length and no null terminator.
///    - When serializing, all strings are handled equally. When deserializing,
///      there are three flavors of strings: transient, owned, and borrowed.
///  - **byte array** - \[u8\]
///    - Similar to strings, during deserialization byte arrays can be
///      transient, owned, or borrowed.
///  - **option**
///    - Either none or some value.
///  - **unit**
///    - The type of `()` in Rust. It represents an anonymous value containing
///      no data.
///  - **unit_struct**
///    - For example `struct Unit` or `PhantomData<T>`. It represents a named
///      value containing no data.
///  - **unit_variant**
///    - For example the `E::A` and `E::B` in `enum E { A, B }`.
///  - **newtype_struct**
///    - For example `struct Millimeters(u8)`.
///  - **newtype_variant**
///    - For example the `E::N` in `enum E { N(u8) }`.
///  - **seq**
///    - A variably sized heterogeneous sequence of values, for example
///      `Vec<T>` or `HashSet<T>`. When serializing, the length may or may not
///      be known before iterating through all the data. When deserializing,
///      the length is determined by looking at the serialized data.
///  - **tuple**
///    - A statically sized heterogeneous sequence of values for which the
///      length will be known at deserialization time without looking at the
///      serialized data, for example `(u8,)` or `(String, u64, Vec<T>)` or
///      `[u64; 10]`.
///  - **tuple_struct**
///    - A named tuple, for example `struct Rgb(u8, u8, u8)`.
///  - **tuple_variant**
///    - For example the `E::T` in `enum E { T(u8, u8) }`.
///  - **map**
///    - A heterogeneous key-value pairing, for example `BTreeMap<K, V>`.
///  - **struct**
///    - A heterogeneous key-value pairing in which the keys are strings and
///      will be known at deserialization time without looking at the
///      serialized data, for example `struct S { r: u8, g: u8, b: u8 }`.
///  - **struct_variant**
///    - For example the `E::S` in `enum E { S { r: u8, g: u8, b: u8 } }`.
///
/// Many Serde serializers produce text or binary data as output, for example
/// JSON or Postcard. This is not a requirement of the `Serializer` trait, and
/// there are serializers that do not produce text or binary output. One example
/// is the `serde_json::value::Serializer` (distinct from the main `serde_json`
/// serializer) that produces a `serde_json::Value` data structure in memory as
/// output.
///
/// [Serde data model]: https://serde.rs/data-model.html
///
/// # Example implementation
///
/// The [example data format] presented on the website contains example code for
/// a basic JSON `Serializer`.
///
/// [example data format]: https://serde.rs/data-format.html
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::Serializer` is not satisfied"
)]
pub trait Serializer: Sized {
    /// The output type produced by this `Serializer` during successful
    /// serialization. Most serializers that produce text or binary output
    /// should set `Ok = ()` and serialize into an [`io::Write`] or buffer
    /// contained within the `Serializer` instance. Serializers that build
    /// in-memory data structures may be simplified by using `Ok` to propagate
    /// the data structure around.
    ///
    /// [`io::Write`]: std::io::Write
    type Ok;

    /// The error type when some error occurs during serialization.
    type Error: Error;

    /// Type returned from [`serialize_seq`] for serializing the content of the
    /// sequence.
    ///
    /// [`serialize_seq`]: #tymethod.serialize_seq
    type SerializeSeq: SerializeSeq<Ok = Self::Ok, Error = Self::Error>;

    /// Type returned from [`serialize_tuple`] for serializing the content of
    /// the tuple.
    ///
    /// [`serialize_tuple`]: #tymethod.serialize_tuple
    type SerializeTuple: SerializeTuple<Ok = Self::Ok, Error = Self::Error>;

    /// Type returned from [`serialize_tuple_struct`] for serializing the
    /// content of the tuple struct.
    ///
    /// [`serialize_tuple_struct`]: #tymethod.serialize_tuple_struct
    type SerializeTupleStruct: SerializeTupleStruct<Ok = Self::Ok, Error = Self::Error>;

    /// Type returned from [`serialize_tuple_variant`] for serializing the
    /// content of the tuple variant.
    ///
    /// [`serialize_tuple_variant`]: #tymethod.serialize_tuple_variant
    type SerializeTupleVariant: SerializeTupleVariant<Ok = Self::Ok, Error = Self::Error>;

    /// Type returned from [`serialize_map`] for serializing the content of the
    /// map.
    ///
    /// [`serialize_map`]: #tymethod.serialize_map
    type SerializeMap: SerializeMap<Ok = Self::Ok, Error = Self::Error>;

    /// Type returned from [`serialize_struct`] for serializing the content of
    /// the struct.
    ///
    /// [`serialize_struct`]: #tymethod.serialize_struct
    type SerializeStruct: SerializeStruct<Ok = Self::Ok, Error = Self::Error>;

    /// Type returned from [`serialize_struct_variant`] for serializing the
    /// content of the struct variant.
    ///
    /// [`serialize_struct_variant`]: #tymethod.serialize_struct_variant
    type SerializeStructVariant: SerializeStructVariant<Ok = Self::Ok, Error = Self::Error>;

    /// Serialize a `bool` value.
    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i8` value.
    ///
    /// If the format does not differentiate between `i8` and `i64`, a
    /// reasonable implementation would be to cast the value to `i64` and
    /// forward to `serialize_i64`.
    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i16` value.
    ///
    /// If the format does not differentiate between `i16` and `i64`, a
    /// reasonable implementation would be to cast the value to `i64` and
    /// forward to `serialize_i64`.
    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i32` value.
    ///
    /// If the format does not differentiate between `i32` and `i64`, a
    /// reasonable implementation would be to cast the value to `i64` and
    /// forward to `serialize_i64`.
    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i64` value.
    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `i128` value.
    ///
    /// The default behavior unconditionally returns an error.
    fn serialize_i128(self, v: i128) -> Result<Self::Ok, Self::Error> {
        let _ = v;
        Err(Error::custom("i128 is not supported"))
    }

    /// Serialize a `u8` value.
    ///
    /// If the format does not differentiate between `u8` and `u64`, a
    /// reasonable implementation would be to cast the value to `u64` and
    /// forward to `serialize_u64`.
    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u16` value.
    ///
    /// If the format does not differentiate between `u16` and `u64`, a
    /// reasonable implementation would be to cast the value to `u64` and
    /// forward to `serialize_u64`.
    fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u32` value.
    ///
    /// If the format does not differentiate between `u32` and `u64`, a
    /// reasonable implementation would be to cast the value to `u64` and
    /// forward to `serialize_u64`.
    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u64` value.
    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `u128` value.
    ///
    /// The default behavior unconditionally returns an error.
    fn serialize_u128(self, v: u128) -> Result<Self::Ok, Self::Error> {
        let _ = v;
        Err(Error::custom("u128 is not supported"))
    }

    /// Serialize an `f32` value.
    ///
    /// If the format does not differentiate between `f32` and `f64`, a
    /// reasonable implementation would be to cast the value to `f64` and
    /// forward to `serialize_f64`.
    fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error>;

    /// Serialize an `f64` value.
    fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error>;

    /// Serialize a character.
    ///
    /// If the format does not support characters, it is reasonable to serialize
    /// it as a single element `str` or a `u32`.
    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error>;

    /// Serialize a `&str`.
    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error>;

    /// Serialize a chunk of raw byte data.
    ///
    /// Enables serializers to serialize byte slices more compactly or more
    /// efficiently than other types of slices. If no efficient implementation
    /// is available, a reasonable implementation would be to forward to
    /// `serialize_seq`. If forwarded, the implementation looks usually just
    /// like this:
    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error>;

    /// Serialize a [`None`] value.
    ///
    /// [`None`]: core::option::Option::None
    fn serialize_none(self) -> Result<Self::Ok, Self::Error>;

    /// Serialize a [`Some(T)`] value.
    ///
    /// [`Some(T)`]: core::option::Option::Some
    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a `()` value.
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error>;

    /// Serialize a unit struct like `struct Unit` or `PhantomData<T>`.
    ///
    /// A reasonable implementation would be to forward to `serialize_unit`.
    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error>;

    /// Serialize a unit variant like `E::A` in `enum E { A, B }`.
    ///
    /// The `name` is the name of the enum, the `variant_index` is the index of
    /// this variant within the enum, and the `variant` is the name of the
    /// variant.
    fn serialize_unit_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error>;

    /// Serialize a newtype struct like `struct Millimeters(u8)`.
    ///
    /// Serializers are encouraged to treat newtype structs as insignificant
    /// wrappers around the data they contain. A reasonable implementation would
    /// be to forward to `value.serialize(self)`.
    fn serialize_newtype_struct<T>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a newtype variant like `E::N` in `enum E { N(u8) }`.
    ///
    /// The `name` is the name of the enum, the `variant_index` is the index of
    /// this variant within the enum, and the `variant` is the name of the
    /// variant. The `value` is the data contained within this newtype variant.
    fn serialize_newtype_variant<T>(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize;

    /// Begin to serialize a variably sized sequence. This call must be
    /// followed by zero or more calls to `serialize_element`, then a call to
    /// `end`.
    ///
    /// The argument is the number of elements in the sequence, which may or may
    /// not be computable before the sequence is iterated. Some serializers only
    /// support sequences whose length is known up front.
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error>;

    /// Begin to serialize a statically sized sequence whose length will be
    /// known at deserialization time without looking at the serialized data.
    /// This call must be followed by zero or more calls to `serialize_element`,
    /// then a call to `end`.
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error>;

    /// Begin to serialize a tuple struct like `struct Rgb(u8, u8, u8)`. This
    /// call must be followed by zero or more calls to `serialize_field`, then a
    /// call to `end`.
    ///
    /// The `name` is the name of the tuple struct and the `len` is the number
    /// of data fields that will be serialized.
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error>;

    /// Begin to serialize a tuple variant like `E::T` in `enum E { T(u8, u8)
    /// }`. This call must be followed by zero or more calls to
    /// `serialize_field`, then a call to `end`.
    ///
    /// The `name` is the name of the enum, the `variant_index` is the index of
    /// this variant within the enum, the `variant` is the name of the variant,
    /// and the `len` is the number of data fields that will be serialized.
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error>;

    /// Begin to serialize a map. This call must be followed by zero or more
    /// calls to `serialize_key` and `serialize_value`, then a call to `end`.
    ///
    /// The argument is the number of elements in the map, which may or may not
    /// be computable before the map is iterated. Some serializers only support
    /// maps whose length is known up front.
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error>;

    /// Begin to serialize a struct like `struct Rgb { r: u8, g: u8, b: u8 }`.
    /// This call must be followed by zero or more calls to `serialize_field`,
    /// then a call to `end`.
    ///
    /// The `name` is the name of the struct and the `len` is the number of
    /// data fields that will be serialized. `len` does not include fields
    /// which are skipped with [`SerializeStruct::skip_field`].
    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error>;

    /// Begin to serialize a struct variant like `E::S` in `enum E { S { r: u8,
    /// g: u8, b: u8 } }`. This call must be followed by zero or more calls to
    /// `serialize_field`, then a call to `end`.
    ///
    /// The `name` is the name of the enum, the `variant_index` is the index of
    /// this variant within the enum, the `variant` is the name of the variant,
    /// and the `len` is the number of data fields that will be serialized.
    /// `len` does not include fields which are skipped with
    /// [`SerializeStructVariant::skip_field`].
    fn serialize_struct_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error>;

    /// Collect an iterator as a sequence.
    ///
    /// The default implementation serializes each item yielded by the iterator
    /// using [`serialize_seq`]. Implementors should not need to override this
    /// method.
    ///
    /// [`serialize_seq`]: #tymethod.serialize_seq
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
    ///
    /// The default implementation serializes each pair yielded by the iterator
    /// using [`serialize_map`]. Implementors should not need to override this
    /// method.
    ///
    /// [`serialize_map`]: #tymethod.serialize_map
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
    ///
    /// The default implementation builds a heap-allocated [`String`] and
    /// delegates to [`serialize_str`]. Serializers are encouraged to provide a
    /// more efficient implementation if possible.
    ///
    /// [`serialize_str`]: Self::serialize_str
    fn collect_str<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Display,
    {
        self.serialize_str(&value.to_string())
    }

    /// Determine whether `Serialize` implementations should serialize in
    /// human-readable form.
    ///
    /// Some types have a human-readable form that may be somewhat expensive to
    /// construct, as well as a binary form that is compact and efficient.
    /// Generally text-based formats like JSON and YAML will prefer to use the
    /// human-readable one and binary formats like Postcard will prefer the
    /// compact one.
    ///
    /// The default implementation of this method returns `true`. Data formats
    /// may override this to `false` to request a compact form for types that
    /// support one. Note that modifying this method to change a format from
    /// human-readable to compact or vice versa should be regarded as a breaking
    /// change, as a value serialized in human-readable mode is not required to
    /// deserialize from the same data in compact mode.
    #[inline]
    fn is_human_readable(&self) -> bool {
        true
    }
}

/// Returned from `Serializer::serialize_seq`.
///
/// # Example use
///
/// # Example implementation
///
/// The [example data format] presented on the website demonstrates an
/// implementation of `SerializeSeq` for a basic JSON data format.
///
/// [example data format]: https://serde.rs/data-format.html
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

/// Returned from `Serializer::serialize_tuple`.
///
/// # Example use
///
/// # Example implementation
///
/// The [example data format] presented on the website demonstrates an
/// implementation of `SerializeTuple` for a basic JSON data format.
///
/// [example data format]: https://serde.rs/data-format.html
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

/// Returned from `Serializer::serialize_tuple_struct`.
///
/// # Example use
///
/// # Example implementation
///
/// The [example data format] presented on the website demonstrates an
/// implementation of `SerializeTupleStruct` for a basic JSON data format.
///
/// [example data format]: https://serde.rs/data-format.html
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

/// Returned from `Serializer::serialize_tuple_variant`.
///
/// # Example use
///
/// # Example implementation
///
/// The [example data format] presented on the website demonstrates an
/// implementation of `SerializeTupleVariant` for a basic JSON data format.
///
/// [example data format]: https://serde.rs/data-format.html
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

/// Returned from `Serializer::serialize_map`.
///
/// # Example use
///
/// # Example implementation
///
/// The [example data format] presented on the website demonstrates an
/// implementation of `SerializeMap` for a basic JSON data format.
///
/// [example data format]: https://serde.rs/data-format.html
#[diagnostic::on_unimplemented(
    message = "the trait bound `{Self}: serde::ser::SerializeMap` is not satisfied"
)]
pub trait SerializeMap {
    /// Must match the `Ok` type of our `Serializer`.
    type Ok;

    /// Must match the `Error` type of our `Serializer`.
    type Error: Error;

    /// Serialize a map key.
    ///
    /// If possible, `Serialize` implementations are encouraged to use
    /// `serialize_entry` instead as it may be implemented more efficiently in
    /// some formats compared to a pair of calls to `serialize_key` and
    /// `serialize_value`.
    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a map value.
    ///
    /// # Panics
    ///
    /// Calling `serialize_value` before `serialize_key` is incorrect and is
    /// allowed to panic or produce bogus results.
    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize;

    /// Serialize a map entry consisting of a key and a value.
    ///
    /// Some [`Serialize`] types are not able to hold a key and value in memory
    /// at the same time so `SerializeMap` implementations are required to
    /// support [`serialize_key`] and [`serialize_value`] individually. The
    /// `serialize_entry` method allows serializers to optimize for the case
    /// where key and value are both available. [`Serialize`] implementations
    /// are encouraged to use `serialize_entry` if possible.
    ///
    /// The default implementation delegates to [`serialize_key`] and
    /// [`serialize_value`]. This is appropriate for serializers that do not
    /// care about performance or are not able to optimize `serialize_entry` any
    /// better than this.
    ///
    /// [`Serialize`]: crate::Serialize
    /// [`serialize_key`]: Self::serialize_key
    /// [`serialize_value`]: Self::serialize_value
    fn serialize_entry<K, V>(&mut self, key: &K, value: &V) -> Result<(), Self::Error>
    where
        K: ?Sized + Serialize,
        V: ?Sized + Serialize,
    {
        tri!(self.serialize_key(key));
        self.serialize_value(value)
    }

    /// Serialize a struct's field that reaches the map as an entry. aterm's
    /// own; upstream serde has no counterpart.
    ///
    /// A struct with a `#[serde(flatten)]` field is written as a map, and a
    /// flattened struct writes its fields into the enclosing one, so a field
    /// arrives here rather than at [`SerializeStruct::serialize_field`]. A
    /// format that writes a field and a map entry alike needs nothing more: the
    /// default delegates to [`serialize_entry`]. A format that cannot write
    /// every value tells the two apart here. TOML has no null, so it leaves a
    /// `None` field out (an absent field reads back as `None`) but refuses a
    /// `None` map value, which an absent key would lose.
    ///
    /// [`serialize_entry`]: Self::serialize_entry
    fn serialize_field_entry<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.serialize_entry(key, value)
    }

    /// Finish serializing a map.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from `Serializer::serialize_struct`.
///
/// # Example use
///
/// # Example implementation
///
/// The [example data format] presented on the website demonstrates an
/// implementation of `SerializeStruct` for a basic JSON data format.
///
/// [example data format]: https://serde.rs/data-format.html
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
    ///
    /// The default implementation does nothing.
    #[inline]
    fn skip_field(&mut self, key: &'static str) -> Result<(), Self::Error> {
        let _ = key;
        Ok(())
    }

    /// Finish serializing a struct.
    fn end(self) -> Result<Self::Ok, Self::Error>;
}

/// Returned from `Serializer::serialize_struct_variant`.
///
/// # Example use
///
/// # Example implementation
///
/// The [example data format] presented on the website demonstrates an
/// implementation of `SerializeStructVariant` for a basic JSON data format.
///
/// [example data format]: https://serde.rs/data-format.html
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
    ///
    /// The default implementation does nothing.
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
