// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The untyped value tree — what a TOML file MEANS, with the formatting gone.
//!
//! This is the shape callers reach for when the document's schema is not known
//! at compile time (aterm's art assets, a status file being spot-checked in a
//! test). When the schema IS known, `#[derive(Deserialize)]` and
//! [`crate::from_str`] skip this representation entirely and build the target
//! type straight off the parse tree.

use core::fmt;
use core::str::FromStr;
use std::collections::BTreeMap;
use std::collections::btree_map;

use crate::Datetime;

/// A TOML table: keys to values.
///
/// Ordered by key rather than by appearance, deliberately: this is the
/// SEMANTIC view, two files that differ only in key order mean the same thing,
/// and a caller who needs the authored order wants [`crate::edit`] instead.
pub type Table = Map<String, Value>;

/// A sorted string-keyed map.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Map<K: Ord, V> {
    inner: BTreeMap<K, V>,
}

impl<K: Ord, V> Map<K, V> {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Are there no entries?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Insert, returning any previous value.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.inner.insert(key, value)
    }

    /// Entries in key order.
    pub fn iter(&self) -> btree_map::Iter<'_, K, V> {
        self.inner.iter()
    }

    /// Entries in key order, values mutable.
    pub fn iter_mut(&mut self) -> btree_map::IterMut<'_, K, V> {
        self.inner.iter_mut()
    }

    /// Keys in order.
    pub fn keys(&self) -> btree_map::Keys<'_, K, V> {
        self.inner.keys()
    }

    /// Values in key order.
    pub fn values(&self) -> btree_map::Values<'_, K, V> {
        self.inner.values()
    }
}

impl<K: Ord + core::borrow::Borrow<str>, V> Map<K, V> {
    /// The value for `key`.
    pub fn get(&self, key: &str) -> Option<&V> {
        self.inner.get(key)
    }

    /// The value for `key`, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.inner.get_mut(key)
    }

    /// Is `key` present?
    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    /// Delete `key`.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.inner.remove(key)
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for Map<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            inner: iter.into_iter().collect(),
        }
    }
}

impl<K: Ord, V> IntoIterator for Map<K, V> {
    type Item = (K, V);
    type IntoIter = btree_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_iter()
    }
}

impl<'a, K: Ord, V> IntoIterator for &'a Map<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = btree_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter()
    }
}

impl core::ops::Index<&str> for Map<String, Value> {
    type Output = Value;

    fn index(&self, key: &str) -> &Value {
        self.get(key).expect("no such key in table")
    }
}

/// A TOML value with its formatting discarded.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A string.
    String(String),
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Boolean(bool),
    /// A date-time in one of the four spec shapes.
    Datetime(Datetime),
    /// An array. TOML 1.0 does not require the elements to share a type.
    Array(Vec<Value>),
    /// A table, in either spelling — the value tree does not record which.
    Table(Table),
}

/// Something that can address into a [`Value`]: a key for tables, an index for
/// arrays.
pub trait Index: private::Sealed {
    /// Resolve against `value`, or `None` if the shapes do not match.
    fn index_into<'v>(&self, value: &'v Value) -> Option<&'v Value>;
}

mod private {
    pub trait Sealed {}
    impl Sealed for str {}
    impl Sealed for String {}
    impl Sealed for usize {}
    impl<T: ?Sized + Sealed> Sealed for &T {}
}

impl Index for str {
    fn index_into<'v>(&self, value: &'v Value) -> Option<&'v Value> {
        match value {
            Value::Table(t) => t.get(self),
            _ => None,
        }
    }
}

impl Index for String {
    fn index_into<'v>(&self, value: &'v Value) -> Option<&'v Value> {
        self.as_str().index_into(value)
    }
}

impl Index for usize {
    fn index_into<'v>(&self, value: &'v Value) -> Option<&'v Value> {
        match value {
            Value::Array(a) => a.get(*self),
            _ => None,
        }
    }
}

impl<T: ?Sized + Index> Index for &T {
    fn index_into<'v>(&self, value: &'v Value) -> Option<&'v Value> {
        (**self).index_into(value)
    }
}

impl Value {
    /// Look up a table key or an array index.
    pub fn get<I: Index>(&self, index: I) -> Option<&Value> {
        index.index_into(self)
    }

    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, if this is one.
    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v),
            _ => None,
        }
    }

    /// The float, if this is one.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// The boolean, if this is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(v) => Some(*v),
            _ => None,
        }
    }

    /// The date-time, if this is one.
    #[must_use]
    pub fn as_datetime(&self) -> Option<&Datetime> {
        match self {
            Value::Datetime(v) => Some(v),
            _ => None,
        }
    }

    /// The array, if this is one.
    #[must_use]
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The array, mutably.
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The table, if this is one.
    #[must_use]
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// The table, mutably.
    pub fn as_table_mut(&mut self) -> Option<&mut Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Is this a string?
    #[must_use]
    pub fn is_str(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// Is this an integer?
    #[must_use]
    pub fn is_integer(&self) -> bool {
        matches!(self, Value::Integer(_))
    }

    /// Is this a float?
    #[must_use]
    pub fn is_float(&self) -> bool {
        matches!(self, Value::Float(_))
    }

    /// Is this a boolean?
    #[must_use]
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Boolean(_))
    }

    /// Is this a date-time?
    #[must_use]
    pub fn is_datetime(&self) -> bool {
        matches!(self, Value::Datetime(_))
    }

    /// Is this an array?
    #[must_use]
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// Is this a table?
    #[must_use]
    pub fn is_table(&self) -> bool {
        matches!(self, Value::Table(_))
    }

    /// A human-facing name for this value's kind.
    #[must_use]
    pub fn type_str(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::Boolean(_) => "boolean",
            Value::Datetime(_) => "datetime",
            Value::Array(_) => "array",
            Value::Table(_) => "table",
        }
    }

    /// Deserialize any `T` out of this value.
    ///
    /// # Errors
    /// If the value does not match `T`'s shape.
    pub fn try_into<T: serde::de::DeserializeOwned>(self) -> crate::Result<T> {
        T::deserialize(self)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::String(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::String(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Integer(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Float(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Boolean(value)
    }
}

impl From<Datetime> for Value {
    fn from(value: Datetime) -> Self {
        Value::Datetime(value)
    }
}

impl From<Table> for Value {
    fn from(value: Table) -> Self {
        Value::Table(value)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(value: Vec<T>) -> Self {
        Value::Array(value.into_iter().map(Into::into).collect())
    }
}

impl FromStr for Value {
    type Err = crate::Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        crate::from_str(s)
    }
}

impl FromStr for Table {
    type Err = crate::Error;

    /// Parse a whole document as a table. A TOML file always IS one, so this is
    /// [`Value::from_str`] with the top-level shape already known.
    fn from_str(s: &str) -> crate::Result<Self> {
        crate::from_str(s)
    }
}

/// `value["key"]` and `value[0]`, panicking when the shape or the key is wrong.
///
/// Use [`Value::get`] where a miss is expected. This exists because indexing is
/// how asset and fixture code reads a document it has already decided is
/// well-formed, and there a panic is the right answer.
impl<I: Index> core::ops::Index<I> for Value {
    type Output = Value;

    fn index(&self, index: I) -> &Value {
        self.get(index).expect("index not found")
    }
}

impl fmt::Display for Value {
    /// Prints the value as TOML: a table as a whole document, a scalar or array
    /// as the value alone. Both are useful — the first for dumping a parsed
    /// file, the second for putting a value in an error message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let item = crate::ser::value_to_item(self).map_err(|_| fmt::Error)?;
        fmt::Display::fmt(&item, f)
    }
}
