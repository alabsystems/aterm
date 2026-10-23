// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Values: scalars, arrays and inline tables, each keeping the text it was written as.

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
    #[inline]
    #[must_use]
    pub const fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.value().as_str()),
            _ => None,
        }
    }

    /// Is this a string?
    #[inline]
    #[must_use]
    pub const fn is_str(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// The integer, if this is one.
    #[inline]
    #[must_use]
    pub const fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v.value()),
            _ => None,
        }
    }

    /// Is this an integer?
    #[inline]
    #[must_use]
    pub const fn is_integer(&self) -> bool {
        matches!(self, Value::Integer(_))
    }

    /// The float, if this is one.
    #[inline]
    #[must_use]
    pub const fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v.value()),
            _ => None,
        }
    }

    /// Is this a float?
    #[inline]
    #[must_use]
    pub const fn is_float(&self) -> bool {
        matches!(self, Value::Float(_))
    }

    /// The boolean, if this is one.
    #[inline]
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(v) => Some(*v.value()),
            _ => None,
        }
    }

    /// Is this a boolean?
    #[inline]
    #[must_use]
    pub const fn is_bool(&self) -> bool {
        matches!(self, Value::Boolean(_))
    }

    /// Is this a date-time?
    #[inline]
    #[must_use]
    pub const fn is_datetime(&self) -> bool {
        matches!(self, Value::Datetime(_))
    }

    /// The array, if this is one.
    #[inline]
    #[must_use]
    pub const fn as_array(&self) -> Option<&Array> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Is this an array?
    #[inline]
    #[must_use]
    pub const fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// The inline table, if this is one.
    #[inline]
    #[must_use]
    pub const fn as_inline_table(&self) -> Option<&InlineTable> {
        match self {
            Value::InlineTable(t) => Some(t),
            _ => None,
        }
    }

    /// Formatting around the value.
    #[inline]
    #[must_use]
    pub const fn decor(&self) -> &Decor {
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
    #[inline]
    pub const fn decor_mut(&mut self) -> &mut Decor {
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
    #[inline]
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
    #[inline]
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
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
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of elements.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.values.len()
    }

    /// Are there no elements?
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Elements in order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.values.iter()
    }

    /// The element at `index`.
    #[inline]
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    /// Append an element.
    #[inline]
    pub fn push(&mut self, value: impl Into<Value>) {
        self.values.push(value.into());
    }

    /// Byte range of `[` through `]` in the source, when it was parsed.
    #[inline]
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
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
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of direct entries.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Has this table no direct entries?
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.len() == 0
    }

    /// The entry for `key`.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.get(key)?.as_value()
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
        self.items.insert(Key::new(key), inline_item(item))
    }
    fn remove(&mut self, key: &str) -> Option<Item> {
        self.items.remove(key)
    }
    fn len(&self) -> usize {
        self.items.len()
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

fn inline_item(item: Item) -> Item {
    match item {
        Item::Table(table) => Item::Value(Value::InlineTable(table_to_inline(table))),
        Item::ArrayOfTables(array) => Item::Value(Value::Array(
            array
                .values
                .into_iter()
                .map(|table| Value::InlineTable(table_to_inline(table)))
                .collect(),
        )),
        other => other,
    }
}

fn table_to_inline(table: super::Table) -> InlineTable {
    let mut out = InlineTable::new();
    out.dotted = table.dotted;
    for (key, item) in table.items.into_entries() {
        out.items.insert(key, inline_item(item));
    }
    out
}

impl core::str::FromStr for Value {
    type Err = crate::Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        super::parse::parse_value(s)
    }
}

/// Build a parsed scalar node.
pub(crate) fn parsed<T: super::ValueRepr>(value: T, raw: &str, span: Range<usize>) -> Formatted<T> {
    Formatted::with_repr(value, Repr::new_unchecked(raw), span)
}
