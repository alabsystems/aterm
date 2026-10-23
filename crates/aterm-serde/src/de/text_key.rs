// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// aterm's own; upstream serde has no counterpart.

//! Map keys a text format wrote as strings, read back as what they were.

use crate::lib::*;

use crate::de::{Deserializer, Visitor};
use crate::private::Content;

/// A deserializer that may hold a map key written as text.
pub trait KeyText {
    /// The key's text, or `None` when this key is not text.
    fn key_text(&self) -> Option<&str>;
}

/// Reads a map key back from the text a string-keyed format wrote it as.
pub struct TextKeyDeserializer<D> {
    inner: D,
}

impl<D> TextKeyDeserializer<D> {
    /// Wrap `inner`, the format's deserializer for one map key.
    #[inline]
    pub const fn new(inner: D) -> Self {
        TextKeyDeserializer { inner }
    }
}

/// An integer as `Display` spells one, at the narrowest of the four widths a
/// visitor takes that holds it.
enum Integer {
    U64(u64),
    I64(i64),
    U128(u128),
    I128(i128),
}

impl Integer {
    /// `text` as an integer, if `Display` writes that integer as `text`.
    #[inline]
    fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('-').unwrap_or(text);
        let canonical = match digits.as_bytes() {
            [b'0'] => digits.len() == text.len(),
            [first, rest @ ..] => {
                matches!(first, b'1'..=b'9') && rest.iter().all(u8::is_ascii_digit)
            }
            [] => false,
        };
        if !canonical {
            return None;
        }
        text.parse()
            .map(Integer::U64)
            .or_else(|_| text.parse().map(Integer::I64))
            .or_else(|_| text.parse().map(Integer::U128))
            .or_else(|_| text.parse().map(Integer::I128))
            .ok()
    }

    #[inline]
    fn visit<'de, V, E>(self, visitor: V) -> Result<V::Value, E>
    where
        V: Visitor<'de>,
        E: crate::de::Error,
    {
        match self {
            Integer::U64(v) => visitor.visit_u64(v),
            Integer::I64(v) => visitor.visit_i64(v),
            Integer::U128(v) => visitor.visit_u128(v),
            Integer::I128(v) => visitor.visit_i128(v),
        }
    }
}

macro_rules! integer_keys {
    ($($method:ident)*) => {$(
        fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.inner.key_text().and_then(Integer::parse) {
                Some(integer) => integer.visit(visitor),
                None => self.inner.$method(visitor),
            }
        }
    )*};
}

macro_rules! forward_to_inner {
    ($($method:ident($($arg:ident: $ty:ty),*))*) => {$(
        fn $method<V>(self, $($arg: $ty,)* visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.inner.$method($($arg,)* visitor)
        }
    )*};
}

impl<'de, D> Deserializer<'de> for TextKeyDeserializer<D>
where
    D: Deserializer<'de> + KeyText,
{
    type Error = D::Error;

    integer_keys! {
        deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64 deserialize_i128
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.inner.key_text() {
            Some("true") => visitor.visit_bool(true),
            Some("false") => visitor.visit_bool(false),
            _ => self.inner.deserialize_bool(visitor),
        }
    }

    /// A key written as text is a present value; a `None` key has no text.
    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.inner.key_text().is_some() {
            visitor.visit_some(self)
        } else {
            self.inner.deserialize_option(visitor)
        }
    }

    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.inner.key_text().is_some() {
            visitor.visit_newtype_struct(self)
        } else {
            self.inner.deserialize_newtype_struct(name, visitor)
        }
    }

    forward_to_inner! {
        deserialize_any()
        deserialize_f32()
        deserialize_f64()
        deserialize_char()
        deserialize_str()
        deserialize_string()
        deserialize_bytes()
        deserialize_byte_buf()
        deserialize_unit()
        deserialize_unit_struct(name: &'static str)
        deserialize_seq()
        deserialize_tuple(len: usize)
        deserialize_tuple_struct(name: &'static str, len: usize)
        deserialize_map()
        deserialize_struct(name: &'static str, fields: &'static [&'static str])
        deserialize_enum(name: &'static str, variants: &'static [&'static str])
        deserialize_identifier()
        deserialize_ignored_any()
    }

    #[inline]
    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }

    #[inline]
    fn __deserialize_content_v1<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de, Value = Content<'de>>,
    {
        self.inner.__deserialize_content_v1(visitor)
    }
}

impl<D> Debug for TextKeyDeserializer<D>
where
    D: Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter
            .debug_struct("TextKeyDeserializer")
            .field("inner", &self.inner)
            .finish()
    }
}
