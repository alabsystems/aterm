// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The untyped document model: [`Value`], [`Number`] and [`Map`].
//!
//! Roughly half the JSON in this tree is read WITHOUT a model — an LLM
//! response whose shape is the provider's business, a recording index, a
//! metrics reply an assertion pokes at — so the untyped half is not an
//! afterthought. It is deliberately the same shape as `serde_json`'s, down to
//! the two decisions that are visible in output:
//!
//! * [`Map`] is a `BTreeMap`, so an object serializes with its keys in SORTED
//!   order. That is what `serde_json` does without its `preserve_order`
//!   feature, and it is why a request body built with [`json!`](crate::json)
//!   is byte-identical between the two.
//! * [`Number`] keeps integers as integers. A `u64` that survived the parse
//!   round-trips exactly; it is not silently widened to `f64` and back.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A JSON object: keys in sorted order, matching `serde_json`'s default.
pub type Map = BTreeMap<String, Value>;

/// Any JSON value.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    /// `null`.
    #[default]
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(Number),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object.
    Object(Map),
}

/// A JSON number: an integer that fits 64 bits stays one, everything else is a
/// double.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Number(N);

#[derive(Debug, Clone, Copy, PartialEq)]
enum N {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
}

impl Number {
    /// The value as `u64`, if it is a non-negative integer that fits.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self.0 {
            N::Unsigned(v) => Some(v),
            N::Signed(_) | N::Float(_) => None,
        }
    }

    /// The value as `i64`, if it is an integer that fits.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self.0 {
            N::Signed(v) => Some(v),
            N::Unsigned(v) => i64::try_from(v).ok(),
            N::Float(_) => None,
        }
    }

    /// The value as `f64`. Always succeeds; integers are widened, which may
    /// lose precision past 2^53 exactly as it would in any JSON reader that
    /// only has doubles.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self.0 {
            N::Unsigned(v) => v as f64,
            N::Signed(v) => v as f64,
            N::Float(v) => v,
        })
    }

    /// Whether this number is held as a non-negative integer.
    #[must_use]
    pub fn is_u64(&self) -> bool {
        matches!(self.0, N::Unsigned(_))
    }

    /// Whether this number is held as an integer of either sign.
    #[must_use]
    pub fn is_i64(&self) -> bool {
        self.as_i64().is_some()
    }

    /// Whether this number is held as a double.
    #[must_use]
    pub fn is_f64(&self) -> bool {
        matches!(self.0, N::Float(_))
    }
}

impl From<u64> for Number {
    fn from(v: u64) -> Self {
        Self(N::Unsigned(v))
    }
}

impl From<i64> for Number {
    fn from(v: i64) -> Self {
        // Normalised the way `serde_json` normalises: a non-negative `i64` is
        // held as unsigned, so `-0` and `0` are the same number.
        if v < 0 {
            Self(N::Signed(v))
        } else {
            Self(N::Unsigned(v.unsigned_abs()))
        }
    }
}

impl From<f64> for Number {
    fn from(v: f64) -> Self {
        Self(N::Float(v))
    }
}

impl Value {
    /// The value under `key`, if this is an object that has one.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// The element at `index`, if this is an array long enough.
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<&Value> {
        match self {
            Self::Array(items) => items.get(index),
            _ => None,
        }
    }

    /// Resolve an RFC 6901 JSON Pointer against this value.
    ///
    /// `""` is the whole document; otherwise the pointer must start with `/`
    /// and each `/`-separated token selects an object key or an array index.
    /// `~1` in a token means `/` and `~0` means `~`, so a key containing a
    /// slash is still addressable.
    ///
    /// An array index must be a plain decimal with no `+` and no leading zero,
    /// so `/0` is the first element and `/00` resolves to nothing — the same
    /// rule `serde_json` applies, and the reason a token that is not an index
    /// cannot accidentally select one.
    #[must_use]
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        if pointer.is_empty() {
            return Some(self);
        }
        if !pointer.starts_with('/') {
            return None;
        }
        let mut target = self;
        for raw in pointer.split('/').skip(1) {
            // Unescape in this order: `~1` first, then `~0`, so a literal `~01`
            // means `~1` rather than `/`.
            let token = raw.replace("~1", "/").replace("~0", "~");
            target = match target {
                Self::Object(map) => map.get(&token)?,
                Self::Array(items) => items.get(array_index(&token)?)?,
                _ => return None,
            };
        }
        Some(target)
    }

    /// The string contents, if this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    /// The value as `u64`, if this is a non-negative integer that fits.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(n) => n.as_u64(),
            _ => None,
        }
    }

    /// The value as `i64`, if this is an integer that fits.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(n) => n.as_i64(),
            _ => None,
        }
    }

    /// The value as `f64`, if this is a number.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => n.as_f64(),
            _ => None,
        }
    }

    /// The boolean, if this is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The elements, if this is an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The entries, if this is an object.
    #[must_use]
    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    /// The entries for mutation, if this is an object.
    #[must_use]
    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    /// The elements for mutation, if this is an array.
    #[must_use]
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Whether this is an array.
    #[must_use]
    pub fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }

    /// Whether this is an object.
    #[must_use]
    pub fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// Whether this is a string.
    #[must_use]
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    /// Whether this is `null`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Whether this is a number.
    #[must_use]
    pub fn is_number(&self) -> bool {
        matches!(self, Self::Number(_))
    }

    /// Whether this is a boolean.
    #[must_use]
    pub fn is_boolean(&self) -> bool {
        matches!(self, Self::Bool(_))
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::String(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::String(v.to_string())
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Self::Array(v.into_iter().map(Into::into).collect())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

macro_rules! from_number {
    ($($ty:ty => $via:ty),* $(,)?) => {
        $(
            impl From<$ty> for Value {
                fn from(v: $ty) -> Self {
                    Self::Number(Number::from(<$via>::from(v)))
                }
            }
        )*
    };
}

from_number!(u8 => u64, u16 => u64, u32 => u64, u64 => u64,
             i8 => i64, i16 => i64, i32 => i64, i64 => i64,
             f32 => f64, f64 => f64);

/// `Display` is compact JSON, so `format!("{value}")` is a document — the
/// behaviour every `{data}` interpolation in this tree already relies on.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match crate::to_string(self) {
            Ok(text) => f.write_str(&text),
            Err(_) => Err(fmt::Error),
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(v) => serializer.serialize_bool(*v),
            Self::Number(n) => match n.0 {
                N::Unsigned(v) => serializer.serialize_u64(v),
                N::Signed(v) => serializer.serialize_i64(v),
                N::Float(v) => serializer.serialize_f64(v),
            },
            Self::String(v) => serializer.serialize_str(v),
            Self::Array(items) => serializer.collect_seq(items),
            Self::Object(map) => serializer.collect_map(map),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(v)))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(v)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(v)))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_string()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        // Last key wins, which is `serde_json`'s rule and `BTreeMap::insert`'s.
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            out.insert(key, value);
        }
        Ok(Value::Object(out))
    }
}

/// A JSON Pointer array index: decimal, no sign, and no leading zero unless the
/// token IS `"0"`.
fn array_index(token: &str) -> Option<usize> {
    if token.starts_with('+') || (token.starts_with('0') && token.len() != 1) {
        return None;
    }
    token.parse::<usize>().ok()
}

/// The immutable `value["key"]` / `value[0]` sugar.
///
/// A miss yields `Null` rather than panicking — the same total behaviour
/// `serde_json` gives its `Index` impl, which is what lets
/// `value["rows"].is_null()` be a question rather than a crash.
static NULL: Value = Value::Null;

impl std::ops::Index<&str> for Value {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
}

impl std::ops::Index<usize> for Value {
    type Output = Value;
    fn index(&self, index: usize) -> &Value {
        self.get_index(index).unwrap_or(&NULL)
    }
}
