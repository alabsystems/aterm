// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Values: the six scalar kinds, arrays, and inline tables — each keeping the
//! text it was written as.

use core::ops::Range;

use super::item::{KeyMap, TableLike, append_values, sort_by_authored_order};
use super::{Decor, Formatted, Item, Key, RawString, Repr};
use crate::Datetime;

/// A TOML value.
#[derive(Debug, Clone)]
pub enum Value {
    /// A string, in any of the four spellings.
    String(Formatted<String>),
    /// A 64-bit signed integer.
    Integer(Formatted<i64>),
    /// A 64-bit float.
    Float(Formatted<f64>),
    /// `true` or `false`.
    Boolean(Formatted<bool>),
    /// One of the four date-time shapes.
    Datetime(Formatted<Datetime>),
    /// `[ … ]`.
    Array(Array),
    /// `{ … }`.
    InlineTable(InlineTable),
}

impl Value {
    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.value().as_str()),
            _ => None,
        }
    }

    /// Is this a string?
    #[must_use]
    pub fn is_str(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// The integer, if this is one.
    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v.value()),
            _ => None,
        }
    }

    /// Is this an integer?
    #[must_use]
    pub fn is_integer(&self) -> bool {
        matches!(self, Value::Integer(_))
    }

    /// The float, if this is one.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v.value()),
            _ => None,
        }
    }

    /// Is this a float?
    #[must_use]
    pub fn is_float(&self) -> bool {
        matches!(self, Value::Float(_))
    }

    /// The boolean, if this is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(v) => Some(*v.value()),
            _ => None,
        }
    }

    /// Is this a boolean?
    #[must_use]
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Boolean(_))
    }

    /// The date-time, if this is one.
    #[must_use]
    pub fn as_datetime(&self) -> Option<&Datetime> {
        match self {
            Value::Datetime(v) => Some(v.value()),
            _ => None,
        }
    }

    /// Is this a date-time?
    #[must_use]
    pub fn is_datetime(&self) -> bool {
        matches!(self, Value::Datetime(_))
    }

    /// The array, if this is one.
    #[must_use]
    pub fn as_array(&self) -> Option<&Array> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The array, mutably.
    pub fn as_array_mut(&mut self) -> Option<&mut Array> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Is this an array?
    #[must_use]
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// The inline table, if this is one.
    #[must_use]
    pub fn as_inline_table(&self) -> Option<&InlineTable> {
        match self {
            Value::InlineTable(t) => Some(t),
            _ => None,
        }
    }

    /// The inline table, mutably.
    pub fn as_inline_table_mut(&mut self) -> Option<&mut InlineTable> {
        match self {
            Value::InlineTable(t) => Some(t),
            _ => None,
        }
    }

    /// Is this an inline table?
    #[must_use]
    pub fn is_inline_table(&self) -> bool {
        matches!(self, Value::InlineTable(_))
    }

    /// Formatting around the value.
    #[must_use]
    pub fn decor(&self) -> &Decor {
        match self {
            Value::String(v) => v.decor(),
            Value::Integer(v) => v.decor(),
            Value::Float(v) => v.decor(),
            Value::Boolean(v) => v.decor(),
            Value::Datetime(v) => v.decor(),
            Value::Array(a) => &a.decor,
            Value::InlineTable(t) => &t.decor,
        }
    }

    /// Mutable formatting around the value.
    pub fn decor_mut(&mut self) -> &mut Decor {
        match self {
            Value::String(v) => v.decor_mut(),
            Value::Integer(v) => v.decor_mut(),
            Value::Float(v) => v.decor_mut(),
            Value::Boolean(v) => v.decor_mut(),
            Value::Datetime(v) => v.decor_mut(),
            Value::Array(a) => &mut a.decor,
            Value::InlineTable(t) => &mut t.decor,
        }
    }

    /// Byte range of the value token in the source, when it was parsed.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        match self {
            Value::String(v) => v.span(),
            Value::Integer(v) => v.span(),
            Value::Float(v) => v.span(),
            Value::Boolean(v) => v.span(),
            Value::Datetime(v) => v.span(),
            Value::Array(a) => a.span.clone(),
            Value::InlineTable(t) => t.span.clone(),
        }
    }

    /// A human-facing name for this value's kind, for error messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::Boolean(_) => "boolean",
            Value::Datetime(_) => "datetime",
            Value::Array(_) => "array",
            Value::InlineTable(_) => "inline table",
        }
    }

    /// Set the leading whitespace, returning the value — the builder spelling
    /// used when assembling a document by hand.
    #[must_use]
    pub fn decorated(mut self, prefix: impl Into<RawString>, suffix: impl Into<RawString>) -> Self {
        let decor = self.decor_mut();
        decor.set_prefix(prefix);
        decor.set_suffix(suffix);
        self
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::String(Formatted::new(value.to_owned()))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::String(Formatted::new(value))
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Integer(Formatted::new(value))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Float(Formatted::new(value))
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Boolean(Formatted::new(value))
    }
}

impl From<Datetime> for Value {
    fn from(value: Datetime) -> Self {
        Value::Datetime(Formatted::new(value))
    }
}

impl From<Array> for Value {
    fn from(value: Array) -> Self {
        Value::Array(value)
    }
}

impl From<InlineTable> for Value {
    fn from(value: InlineTable) -> Self {
        Value::InlineTable(value)
    }
}

/// A `[ … ]` array.
///
/// `trailing` is the run of whitespace and comments between the last element
/// (or its comma) and the closing bracket — the only part of an array's
/// formatting that has no element to hang off.
#[derive(Debug, Clone, Default)]
pub struct Array {
    pub(crate) values: Vec<Value>,
    pub(crate) trailing: RawString,
    pub(crate) trailing_comma: bool,
    pub(crate) decor: Decor,
    pub(crate) span: Option<Range<usize>>,
}

impl Array {
    /// An empty array.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Are there no elements?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Elements in order.
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.values.iter()
    }

    /// Elements in order, mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.values.iter_mut()
    }

    /// The element at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    /// Append an element.
    pub fn push(&mut self, value: impl Into<Value>) {
        self.values.push(value.into());
    }

    /// Formatting around the brackets.
    #[must_use]
    pub fn decor(&self) -> &Decor {
        &self.decor
    }

    /// Mutable formatting around the brackets.
    pub fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }

    /// Byte range of `[` through `]` in the source, when it was parsed.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }

    /// Does this array print a comma after its last element?
    #[must_use]
    pub fn trailing_comma(&self) -> bool {
        self.trailing_comma
    }

    /// Declare whether a trailing comma is printed.
    pub fn set_trailing_comma(&mut self, yes: bool) {
        self.trailing_comma = yes;
    }
}

impl<'a> IntoIterator for &'a Array {
    type Item = &'a Value;
    type IntoIter = core::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

impl<V: Into<Value>> FromIterator<V> for Array {
    fn from_iter<I: IntoIterator<Item = V>>(iter: I) -> Self {
        let mut array = Array::new();
        array.values = iter.into_iter().map(Into::into).collect();
        array
    }
}

/// A `{ … }` inline table.
///
/// `preamble` is the whitespace immediately after `{`; every other run belongs
/// to a key or a value, so `{ }` and `{}` both survive a round-trip.
#[derive(Debug, Clone, Default)]
pub struct InlineTable {
    pub(crate) items: KeyMap,
    pub(crate) preamble: RawString,
    pub(crate) decor: Decor,
    pub(crate) dotted: bool,
    pub(crate) span: Option<Range<usize>>,
}

impl InlineTable {
    /// An empty inline table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of direct entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Has this table no direct entries?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.len() == 0
    }

    /// Entries in authored order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.items
            .iter()
            .filter_map(|(k, v)| Some((k.get(), v.as_value()?)))
    }

    /// The entry for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.get(key)?.as_value()
    }

    /// The entry for `key`, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.items.get_mut(key)?.as_value_mut()
    }

    /// Set `key`.
    pub fn insert(&mut self, key: &str, value: Value) -> Option<Value> {
        self.items
            .insert(Key::new(key), Item::Value(value))
            .and_then(|i| match i {
                Item::Value(v) => Some(v),
                _ => None,
            })
    }

    /// Delete `key`.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.items.remove(key).and_then(|i| match i {
            Item::Value(v) => Some(v),
            _ => None,
        })
    }

    /// Formatting around the braces.
    #[must_use]
    pub fn decor(&self) -> &Decor {
        &self.decor
    }

    /// Mutable formatting around the braces.
    pub fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }

    /// Byte range of `{` through `}` in the source, when it was parsed.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }

    /// Did this table come from a dotted key inside another inline table?
    #[must_use]
    pub fn is_dotted(&self) -> bool {
        self.dotted
    }

    /// Every leaf reachable through dotted children, with its full key path.
    #[must_use]
    pub fn get_values(&self) -> Vec<(Vec<&Key>, &Value)> {
        let mut out = Vec::new();
        append_values(&self.items, &[], &mut out);
        sort_by_authored_order(&mut out);
        out
    }
}

impl TableLike for InlineTable {
    fn iter(&self) -> Box<dyn Iterator<Item = (&str, &Item)> + '_> {
        Box::new(self.items.iter().map(|(k, v)| (k.get(), v)))
    }
    fn get(&self, key: &str) -> Option<&Item> {
        self.items.get(key)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut Item> {
        self.items.get_mut(key)
    }
    fn get_key_value(&self, key: &str) -> Option<(&Key, &Item)> {
        self.items.get_key_value(key)
    }
    fn contains_key(&self, key: &str) -> bool {
        self.items.contains_key(key)
    }
    fn insert(&mut self, key: &str, item: Item) -> Option<Item> {
        self.items.insert(Key::new(key), item)
    }
    fn remove(&mut self, key: &str) -> Option<Item> {
        self.items.remove(key)
    }
    fn len(&self) -> usize {
        self.items.len()
    }
    fn is_dotted(&self) -> bool {
        self.dotted
    }
    fn set_dotted(&mut self, dotted: bool) {
        self.dotted = dotted;
    }
    fn decor(&self) -> &Decor {
        &self.decor
    }
    fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }
    fn as_table(&self) -> Option<&super::Table> {
        None
    }
    fn as_inline_table(&self) -> Option<&InlineTable> {
        Some(self)
    }
}

impl core::str::FromStr for Value {
    type Err = crate::Error;

    /// Parse ONE value, and nothing else.
    ///
    /// The input must be exactly the value: no surrounding whitespace, no
    /// comment, no trailing newline. That is the oracle's contract, measured
    /// rather than assumed — see `parse::parse_value`.
    fn from_str(s: &str) -> crate::Result<Self> {
        super::parse::parse_value(s)
    }
}

/// Build a parsed scalar node. Kept here so the parser does not have to know
/// the shape of [`Formatted`].
pub(crate) fn parsed<T: super::ValueRepr>(value: T, raw: &str, span: Range<usize>) -> Formatted<T> {
    Formatted::with_repr(value, Repr::new_unchecked(raw), span)
}
