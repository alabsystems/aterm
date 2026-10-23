// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tables and the four things a table entry can be.

use core::ops::Range;
use std::collections::HashMap;

use super::{Decor, Key, Value};

/// One entry in a table: a value, a sub-table, an array of sub-tables, or vacant ([`Item::None`]).
#[derive(Debug, Clone, Default)]
pub enum Item {
    /// No entry.
    #[default]
    None,
    /// A value: scalar, array, or inline table.
    Value(Value),
    /// A `[header]` table, or a table implied by a dotted key.
    Table(Table),
    /// A `[[header]]` array of tables.
    ArrayOfTables(ArrayOfTables),
}

impl Item {
    /// Is this the vacant entry?
    #[inline]
    #[must_use]
    pub const fn is_none(&self) -> bool {
        matches!(self, Item::None)
    }

    /// The value, if this entry is one.
    #[inline]
    #[must_use]
    pub const fn as_value(&self) -> Option<&Value> {
        match self {
            Item::Value(v) => Some(v),
            _ => None,
        }
    }

    /// The value, mutably.
    #[inline]
    pub const fn as_value_mut(&mut self) -> Option<&mut Value> {
        match self {
            Item::Value(v) => Some(v),
            _ => None,
        }
    }

    /// The `[header]` table, if this entry is one.
    #[inline]
    #[must_use]
    pub const fn as_table(&self) -> Option<&Table> {
        match self {
            Item::Table(t) => Some(t),
            _ => None,
        }
    }

    /// The `[header]` table, mutably.
    #[inline]
    pub const fn as_table_mut(&mut self) -> Option<&mut Table> {
        match self {
            Item::Table(t) => Some(t),
            _ => None,
        }
    }

    /// The array of tables, if this entry is one.
    #[inline]
    #[must_use]
    pub const fn as_array_of_tables(&self) -> Option<&ArrayOfTables> {
        match self {
            Item::ArrayOfTables(a) => Some(a),
            _ => None,
        }
    }

    /// Either table spelling — `[header]` or `{ inline }` — behind one trait.
    #[inline]
    #[must_use]
    pub fn as_table_like(&self) -> Option<&dyn TableLike> {
        match self {
            Item::Table(t) => Some(t),
            Item::Value(Value::InlineTable(t)) => Some(t),
            _ => None,
        }
    }

    /// Either table spelling, mutably.
    #[inline]
    pub fn as_table_like_mut(&mut self) -> Option<&mut dyn TableLike> {
        match self {
            Item::Table(t) => Some(t),
            Item::Value(Value::InlineTable(t)) => Some(t),
            _ => None,
        }
    }

    /// Is this entry a table in either spelling?
    #[inline]
    #[must_use]
    pub fn is_table_like(&self) -> bool {
        self.as_table_like().is_some()
    }

    /// Is this entry a `[header]` table?
    #[inline]
    #[must_use]
    pub const fn is_table(&self) -> bool {
        matches!(self, Item::Table(_))
    }

    /// Is this entry an array of tables?
    #[inline]
    #[must_use]
    pub const fn is_array_of_tables(&self) -> bool {
        matches!(self, Item::ArrayOfTables(_))
    }

    /// The string, if this entry is one.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        self.as_value().and_then(Value::as_str)
    }

    /// The integer, if this entry is one.
    #[inline]
    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        self.as_value().and_then(Value::as_integer)
    }

    /// The float, if this entry is one.
    #[inline]
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        self.as_value().and_then(Value::as_float)
    }

    /// The boolean, if this entry is one.
    #[inline]
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        self.as_value().and_then(Value::as_bool)
    }

    /// The array, if this entry is one.
    #[inline]
    #[must_use]
    pub fn as_array(&self) -> Option<&super::Array> {
        self.as_value().and_then(Value::as_array)
    }

    /// Is this entry a value (rather than a table or an array of tables)?
    #[inline]
    #[must_use]
    pub const fn is_value(&self) -> bool {
        matches!(self, Item::Value(_))
    }

    /// Is this entry a string?
    #[inline]
    #[must_use]
    pub fn is_str(&self) -> bool {
        self.as_value().is_some_and(Value::is_str)
    }

    /// Is this entry an integer?
    #[inline]
    #[must_use]
    pub fn is_integer(&self) -> bool {
        self.as_value().is_some_and(Value::is_integer)
    }

    /// Is this entry a float?
    #[inline]
    #[must_use]
    pub fn is_float(&self) -> bool {
        self.as_value().is_some_and(Value::is_float)
    }

    /// Is this entry a boolean?
    #[inline]
    #[must_use]
    pub fn is_bool(&self) -> bool {
        self.as_value().is_some_and(Value::is_bool)
    }

    /// Is this entry a date-time?
    #[inline]
    #[must_use]
    pub fn is_datetime(&self) -> bool {
        self.as_value().is_some_and(Value::is_datetime)
    }

    /// Is this entry an array? An array of tables is [`Item::is_array_of_tables`].
    #[inline]
    #[must_use]
    pub fn is_array(&self) -> bool {
        self.as_value().is_some_and(Value::is_array)
    }

    /// Look one level down, through either table spelling.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.as_table_like()?.get(key)
    }

    /// Byte range this entry occupied in the parsed source (`None` if synthesized, invalidated, or an array of tables).
    #[inline]
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        match self {
            Item::None | Item::ArrayOfTables(_) => None,
            Item::Value(v) => v.span(),
            Item::Table(t) => t.span(),
        }
    }

    /// A human-facing name for this entry's kind, for error messages.
    #[inline]
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Item::None => "none",
            Item::Value(v) => v.type_name(),
            Item::Table(_) => "table",
            Item::ArrayOfTables(_) => "array of tables",
        }
    }
}

impl From<Value> for Item {
    fn from(value: Value) -> Self {
        Item::Value(value)
    }
}

/// Insertion-ordered map from key to entry with a hash index for O(1) lookup.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeyMap {
    entries: Vec<(Key, Item)>,
    index: HashMap<String, usize>,
}

impl KeyMap {
    pub(crate) fn get(&self, key: &str) -> Option<&Item> {
        self.index.get(key).map(|&i| &self.entries[i].1)
    }

    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut Item> {
        let i = *self.index.get(key)?;
        Some(&mut self.entries[i].1)
    }

    /// Index of `key` in `entries` (valid until an entry is removed).
    pub(crate) fn position(&self, key: &str) -> Option<usize> {
        self.index.get(key).copied()
    }

    /// The entry at a position [`Self::position`] returned.
    pub(crate) fn get_at_mut(&mut self, position: usize) -> Option<&mut Item> {
        self.entries.get_mut(position).map(|(_, item)| item)
    }

    pub(crate) fn get_key_value(&self, key: &str) -> Option<(&Key, &Item)> {
        self.index.get(key).map(|&i| {
            let (k, v) = &self.entries[i];
            (k, v)
        })
    }

    /// Inserts `value`, preserving an existing key's position and authored spelling.
    pub(crate) fn insert(&mut self, key: Key, value: Item) -> Option<Item> {
        match self.index.get(key.get()) {
            Some(&i) => Some(core::mem::replace(&mut self.entries[i].1, value)),
            None => {
                self.index.insert(key.get().to_owned(), self.entries.len());
                self.entries.push((key, value));
                None
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<Item> {
        self.remove_entry(key).map(|(_, item)| item)
    }

    /// Delete `key`, handing back the key too: its authored formatting.
    pub(crate) fn remove_entry(&mut self, key: &str) -> Option<(Key, Item)> {
        let i = self.index.remove(key)?;
        let entry = self.entries.remove(i);
        for slot in self.index.values_mut() {
            if *slot > i {
                *slot -= 1;
            }
        }
        Some(entry)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Key, &Item)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// The entries by value, in order.
    pub(crate) fn into_entries(self) -> impl Iterator<Item = (Key, Item)> {
        self.entries.into_iter()
    }

    #[inline]
    pub(crate) const fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }
}

/// A TOML table: `[header]`, the document root, or the implied parent of a dotted key.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub(crate) items: KeyMap,
    pub(crate) decor: Decor,
    /// True when no `[header]` was authored for this table (created implicitly by a deeper header).
    pub(crate) implicit: bool,
    /// True when created from a dotted key (`a.b = 1`) and printed inline on its parent's key-value line.
    pub(crate) dotted: bool,
    /// Header ordinal in the document for preserving authored header order.
    pub(crate) position: Option<usize>,
    pub(crate) span: Option<Range<usize>>,
    /// Verbatim text between header brackets (`[ a . b ]`).
    pub(crate) header_repr: Option<super::RawString>,
    /// True when created by a dotted key so a later `[header]` cannot redefine it.
    pub(crate) closed: bool,
    /// Comment and blank lines printed after this table's key-value lines.
    pub(crate) trailing: super::RawString,
}

impl Table {
    /// An empty table with canonical formatting.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Does this table lack an authored `[header]`?
    #[inline]
    #[must_use]
    pub const fn is_implicit(&self) -> bool {
        self.implicit
    }

    /// Declare whether this table's header is printed when it has children.
    #[inline]
    pub const fn set_implicit(&mut self, implicit: bool) {
        self.implicit = implicit;
    }

    /// Byte range of the `[header]` line in the source.
    #[inline]
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }

    /// Entries in authored order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Item)> {
        self.items.iter().map(|(k, v)| (k.get(), v))
    }

    /// The entry for `key`.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.items.get(key)
    }

    /// The entry for `key`, mutably.
    #[inline]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Item> {
        self.items.get_mut(key)
    }

    /// Is `key` present?
    #[inline]
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.items.contains_key(key)
    }

    /// Set `key`, keeping an existing entry's line position.
    pub fn insert(&mut self, key: &str, item: Item) -> Option<Item> {
        self.items.insert(Key::new(key), item)
    }

    /// Set `key` with an explicit key spelling and decor.
    pub fn insert_formatted(&mut self, key: Key, item: Item) -> Option<Item> {
        self.items.insert(key, item)
    }

    /// Delete `key` with everything its line holds, the comment lines above it included.
    pub fn remove(&mut self, key: &str) -> Option<Item> {
        self.items.remove(key)
    }

    /// Preserves leading comment and blank lines from `removed` on the next printed line or `self.trailing`.
    pub(crate) fn keep_lines_above(&mut self, removed: &Key) {
        let Some(repr) = &removed.path_repr else {
            return;
        };
        let Some(end) = repr.head.as_str().rfind('\n') else {
            return;
        };
        let lines = &repr.head.as_str()[..=end];
        let mut keys = Vec::new();
        line_keys_mut(&mut self.items, &mut keys);
        let next = keys
            .into_iter()
            .filter(|key| key.order > removed.order)
            .min_by_key(|key| key.order);
        match next.and_then(|key| key.path_repr.as_mut()) {
            Some(next) => next.head = format!("{lines}{}", next.head).into(),
            None => self.trailing = format!("{lines}{}", self.trailing).into(),
        }
    }

    /// Direct and dotted-subtable key-value pairs in authored order.
    #[must_use]
    pub fn get_values(&self) -> Vec<(Vec<&Key>, &Value)> {
        let mut out = Vec::new();
        append_values(&self.items, &[], &mut out);
        sort_by_authored_order(&mut out);
        out
    }
}

/// Sorts key-value pairs by authored line order (`Key::order`).
pub(crate) fn sort_by_authored_order(values: &mut [(Vec<&Key>, &Value)]) {
    values.sort_by_key(|(path, _)| path.last().map_or(usize::MAX, |key| key.order));
}

pub(crate) fn append_values<'a>(
    items: &'a KeyMap,
    parent: &[&'a Key],
    out: &mut Vec<(Vec<&'a Key>, &'a Value)>,
) {
    for (key, item) in items.iter() {
        let mut path = parent.to_vec();
        path.push(key);
        match item {
            Item::Table(t) if t.dotted => append_values(&t.items, &path, out),
            Item::Value(Value::InlineTable(t)) if t.dotted => append_values(&t.items, &path, out),
            Item::Value(v) => out.push((path, v)),
            _ => {}
        }
    }
}

fn line_keys_mut<'a>(items: &'a mut KeyMap, out: &mut Vec<&'a mut Key>) {
    for (key, item) in &mut items.entries {
        match item {
            Item::Table(t) if t.dotted => line_keys_mut(&mut t.items, out),
            Item::Value(Value::InlineTable(t)) if t.dotted => line_keys_mut(&mut t.items, out),
            Item::Value(_) => out.push(key),
            _ => {}
        }
    }
}

/// A `[[header]]` array of tables.
#[derive(Debug, Clone, Default)]
pub struct ArrayOfTables {
    pub(crate) values: Vec<Table>,
}

impl ArrayOfTables {
    /// An empty array of tables.
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

    /// Elements in document order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Table> {
        self.values.iter()
    }

    /// Append an element.
    #[inline]
    pub fn push(&mut self, table: Table) {
        self.values.push(table);
    }
}

impl<'a> IntoIterator for &'a ArrayOfTables {
    type Item = &'a Table;
    type IntoIter = core::slice::Iter<'a, Table>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

/// Object-safe operations shared by `[header]` and `{ inline }` tables.
pub trait TableLike {
    /// Entries in authored order.
    fn iter(&self) -> Box<dyn Iterator<Item = (&str, &Item)> + '_>;
    /// The entry for `key`.
    fn get(&self, key: &str) -> Option<&Item>;
    /// The entry for `key`, mutably.
    fn get_mut(&mut self, key: &str) -> Option<&mut Item>;
    /// The entry for `key` together with its authored key spelling.
    fn get_key_value(&self, key: &str) -> Option<(&Key, &Item)>;
    /// Is `key` present?
    fn contains_key(&self, key: &str) -> bool;
    /// Set `key`, keeping an existing entry's position.
    fn insert(&mut self, key: &str, item: Item) -> Option<Item>;
    /// Delete `key` with everything its line holds, the comment lines above it included.
    fn remove(&mut self, key: &str) -> Option<Item>;
    /// Number of direct entries.
    fn len(&self) -> usize;
    /// Has this table no direct entries?
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Formatting around the table.
    fn decor(&self) -> &Decor;
    /// Mutable formatting around the table.
    fn decor_mut(&mut self) -> &mut Decor;
    /// The `[header]` view, when that is what this is.
    fn as_table(&self) -> Option<&Table>;
    /// The `{ inline }` view, when that is what this is.
    fn as_inline_table(&self) -> Option<&super::InlineTable>;
}

impl TableLike for Table {
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
    fn decor(&self) -> &Decor {
        &self.decor
    }
    fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }
    fn as_table(&self) -> Option<&Table> {
        Some(self)
    }
    fn as_inline_table(&self) -> Option<&super::InlineTable> {
        None
    }
}
