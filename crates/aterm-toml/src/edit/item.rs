// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tables and the four things a table entry can be.

use core::ops::Range;
use std::collections::HashMap;

use super::{Decor, Key, Value};

/// One entry in a table: a scalar/array/inline-table, a sub-table, an array of
/// sub-tables, or nothing at all.
///
/// [`Item::None`] is not a TOML value — it is the "vacant" state, so that
/// indexing a document with a key that is not there yields a place to write
/// rather than a panic.
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
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Item::None)
    }

    /// The value, if this entry is one.
    #[must_use]
    pub fn as_value(&self) -> Option<&Value> {
        match self {
            Item::Value(v) => Some(v),
            _ => None,
        }
    }

    /// The value, mutably.
    pub fn as_value_mut(&mut self) -> Option<&mut Value> {
        match self {
            Item::Value(v) => Some(v),
            _ => None,
        }
    }

    /// The `[header]` table, if this entry is one. An inline table is NOT one —
    /// use [`Item::as_table_like`] to span both spellings.
    #[must_use]
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Item::Table(t) => Some(t),
            _ => None,
        }
    }

    /// The `[header]` table, mutably.
    pub fn as_table_mut(&mut self) -> Option<&mut Table> {
        match self {
            Item::Table(t) => Some(t),
            _ => None,
        }
    }

    /// The array of tables, if this entry is one.
    #[must_use]
    pub fn as_array_of_tables(&self) -> Option<&ArrayOfTables> {
        match self {
            Item::ArrayOfTables(a) => Some(a),
            _ => None,
        }
    }

    /// The array of tables, mutably.
    pub fn as_array_of_tables_mut(&mut self) -> Option<&mut ArrayOfTables> {
        match self {
            Item::ArrayOfTables(a) => Some(a),
            _ => None,
        }
    }

    /// Either table spelling — `[header]` or `{ inline }` — behind one trait.
    #[must_use]
    pub fn as_table_like(&self) -> Option<&dyn TableLike> {
        match self {
            Item::Table(t) => Some(t),
            Item::Value(Value::InlineTable(t)) => Some(t),
            _ => None,
        }
    }

    /// Either table spelling, mutably.
    pub fn as_table_like_mut(&mut self) -> Option<&mut dyn TableLike> {
        match self {
            Item::Table(t) => Some(t),
            Item::Value(Value::InlineTable(t)) => Some(t),
            _ => None,
        }
    }

    /// Is this entry a table in either spelling?
    #[must_use]
    pub fn is_table_like(&self) -> bool {
        self.as_table_like().is_some()
    }

    /// Is this entry a `[header]` table?
    #[must_use]
    pub fn is_table(&self) -> bool {
        matches!(self, Item::Table(_))
    }

    /// Is this entry an array of tables?
    #[must_use]
    pub fn is_array_of_tables(&self) -> bool {
        matches!(self, Item::ArrayOfTables(_))
    }

    /// The string, if this entry is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        self.as_value().and_then(Value::as_str)
    }

    /// The integer, if this entry is one.
    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        self.as_value().and_then(Value::as_integer)
    }

    /// The float, if this entry is one.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        self.as_value().and_then(Value::as_float)
    }

    /// The boolean, if this entry is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        self.as_value().and_then(Value::as_bool)
    }

    /// The date-time, if this entry is one.
    #[must_use]
    pub fn as_datetime(&self) -> Option<&crate::Datetime> {
        self.as_value().and_then(Value::as_datetime)
    }

    /// The array, if this entry is one. An array of TABLES is a different
    /// thing — see [`Item::as_array_of_tables`].
    #[must_use]
    pub fn as_array(&self) -> Option<&super::Array> {
        self.as_value().and_then(Value::as_array)
    }

    /// The array, mutably.
    pub fn as_array_mut(&mut self) -> Option<&mut super::Array> {
        self.as_value_mut().and_then(Value::as_array_mut)
    }

    /// Is this entry a value (rather than a table or an array of tables)?
    #[must_use]
    pub fn is_value(&self) -> bool {
        matches!(self, Item::Value(_))
    }

    /// Is this entry a string?
    #[must_use]
    pub fn is_str(&self) -> bool {
        self.as_value().is_some_and(Value::is_str)
    }

    /// Is this entry an integer?
    #[must_use]
    pub fn is_integer(&self) -> bool {
        self.as_value().is_some_and(Value::is_integer)
    }

    /// Is this entry a float?
    #[must_use]
    pub fn is_float(&self) -> bool {
        self.as_value().is_some_and(Value::is_float)
    }

    /// Is this entry a boolean?
    #[must_use]
    pub fn is_bool(&self) -> bool {
        self.as_value().is_some_and(Value::is_bool)
    }

    /// Is this entry a date-time?
    #[must_use]
    pub fn is_datetime(&self) -> bool {
        self.as_value().is_some_and(Value::is_datetime)
    }

    /// Is this entry an array? An array of TABLES is a different thing — see
    /// [`Item::is_array_of_tables`].
    #[must_use]
    pub fn is_array(&self) -> bool {
        self.as_value().is_some_and(Value::is_array)
    }

    /// Look one level down, through either table spelling.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.as_table_like()?.get(key)
    }

    /// Look one level down, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Item> {
        self.as_table_like_mut()?.get_mut(key)
    }

    /// The byte range this entry occupied in the document it was parsed from.
    ///
    /// For a value that is the value token; for a `[header]` table it is the
    /// header line. It is `None` for a synthesized node, for an array of
    /// tables (whose elements each carry their own), and after a mutation that
    /// invalidated it.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        match self {
            Item::None | Item::ArrayOfTables(_) => None,
            Item::Value(v) => v.span(),
            Item::Table(t) => t.span(),
        }
    }

    /// A human-facing name for this entry's kind, for error messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
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

/// An insertion-ordered map from key to entry.
///
/// Order is the whole point: a document model that sorted its keys would
/// reorder the user's file on the first write. Lookup rides a side index so a
/// big generated table (aterm's lexicon is thousands of keys) does not turn
/// every `get` into a scan.
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

    pub(crate) fn get_key_value(&self, key: &str) -> Option<(&Key, &Item)> {
        self.index.get(key).map(|&i| {
            let (k, v) = &self.entries[i];
            (k, v)
        })
    }

    /// Insert, keeping an existing key's POSITION and authored spelling. That
    /// is what makes `doc["font_px"] = new` a replacement of one value rather
    /// than a move of the line to the bottom of the file.
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
        let i = self.index.remove(key)?;
        let (_, item) = self.entries.remove(i);
        for slot in self.index.values_mut() {
            if *slot > i {
                *slot -= 1;
            }
        }
        Some(item)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Key, &Item)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = (&Key, &mut Item)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }
}

/// A TOML table: `[header]`, the document root, or the implied parent of a
/// dotted key.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub(crate) items: KeyMap,
    pub(crate) decor: Decor,
    /// No `[header]` line was authored for this table — it exists only because
    /// a deeper one named it. An implicit table with no direct values prints no
    /// header, so `[a.b]` alone does not grow a stray `[a]`.
    pub(crate) implicit: bool,
    /// This table came from a dotted key (`a.b = 1`), so it is printed as part
    /// of its parent's key-value line, never as a header of its own.
    pub(crate) dotted: bool,
    /// Where this table's header stood among all headers in the document.
    /// Rendering sorts on it, which is how `[a.b]` written before `[a]` comes
    /// back out in that order.
    pub(crate) position: Option<usize>,
    pub(crate) span: Option<Range<usize>>,
    /// The verbatim text BETWEEN the header brackets, so `[ a . b ]` prints
    /// back with its spacing. Anchored on the table rather than on the key
    /// segments for the same reason [`super::Key::path_repr`] is: an implicit
    /// parent's key node is shared with the deeper header that created it, and
    /// two headers must not fight over one node's formatting.
    pub(crate) header_repr: Option<super::RawString>,
    /// Created by a dotted key, so a later `[header]` may not REDEFINE it.
    ///
    /// Only redefinition. A header may still add a sub-table BENEATH it —
    /// `apple.color = "red"` then `[fruit.apple.texture]` is the spec's own
    /// worked example — so the parser reads this flag when a header path ENDS
    /// here, and ignores it when the path merely walks through.
    pub(crate) closed: bool,
}

impl Table {
    /// An empty table with canonical formatting.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Formatting around the header line.
    #[must_use]
    pub fn decor(&self) -> &Decor {
        &self.decor
    }

    /// Mutable formatting around the header line.
    pub fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }

    /// Does this table lack an authored `[header]`?
    #[must_use]
    pub fn is_implicit(&self) -> bool {
        self.implicit
    }

    /// Declare whether this table's header is printed when it has children.
    pub fn set_implicit(&mut self, implicit: bool) {
        self.implicit = implicit;
    }

    /// Did this table come from a dotted key?
    #[must_use]
    pub fn is_dotted(&self) -> bool {
        self.dotted
    }

    /// Declare this table printed inline in its parent's key line.
    pub fn set_dotted(&mut self, dotted: bool) {
        self.dotted = dotted;
    }

    /// This table's position among the document's headers.
    #[must_use]
    pub fn position(&self) -> Option<usize> {
        self.position
    }

    /// Move this table among the document's headers.
    pub fn set_position(&mut self, position: usize) {
        self.position = Some(position);
    }

    /// Byte range of the `[header]` line in the source.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
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
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Item)> {
        self.items.iter().map(|(k, v)| (k.get(), v))
    }

    /// Entries in authored order, values mutable.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut Item)> {
        self.items.iter_mut().map(|(k, v)| (k.get(), v))
    }

    /// Entries in authored order, with the full [`Key`] (spelling and decor).
    pub fn iter_keys(&self) -> impl Iterator<Item = (&Key, &Item)> {
        self.items.iter()
    }

    /// The entry for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.items.get(key)
    }

    /// The entry for `key`, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Item> {
        self.items.get_mut(key)
    }

    /// The entry for `key` together with its authored key spelling.
    #[must_use]
    pub fn get_key_value(&self, key: &str) -> Option<(&Key, &Item)> {
        self.items.get_key_value(key)
    }

    /// Is `key` present?
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

    /// Delete `key`.
    pub fn remove(&mut self, key: &str) -> Option<Item> {
        self.items.remove(key)
    }

    /// Every scalar/array/inline-table leaf reachable WITHOUT crossing a header
    /// boundary, paired with the dotted key path that names it.
    ///
    /// This is what a table's own key-value lines are: the direct values, plus
    /// the leaves of any dotted sub-tables, which are printed on the parent's
    /// lines rather than under headers of their own.
    #[must_use]
    pub fn get_values(&self) -> Vec<(Vec<&Key>, &Value)> {
        let mut out = Vec::new();
        append_values(&self.items, &[], &mut out);
        sort_by_authored_order(&mut out);
        out
    }
}

/// Restore the order the lines were WRITTEN in. The tree groups every dotted
/// key under its shared parent, which silently reorders a file that interleaves
/// `net.listen`, `font_px`, `net.timeout_ms` — legal TOML, and a shape a
/// hand-written config really has. The sort is stable, so keys that were never
/// authored (all `usize::MAX`) keep their insertion order at the end.
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

/// A `[[header]]` array of tables.
#[derive(Debug, Clone, Default)]
pub struct ArrayOfTables {
    pub(crate) values: Vec<Table>,
}

impl ArrayOfTables {
    /// An empty array of tables.
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

    /// Elements in document order.
    pub fn iter(&self) -> impl Iterator<Item = &Table> {
        self.values.iter()
    }

    /// Elements in document order, mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Table> {
        self.values.iter_mut()
    }

    /// The element at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Table> {
        self.values.get(index)
    }

    /// The element at `index`, mutably.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Table> {
        self.values.get_mut(index)
    }

    /// Append an element.
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

/// The operations shared by the two table spellings, `[header]` and
/// `{ inline }`.
///
/// Object-safe on purpose: callers walk a document without caring which
/// spelling the author chose, holding a `&dyn TableLike`.
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
    /// Delete `key`.
    fn remove(&mut self, key: &str) -> Option<Item>;
    /// Number of direct entries.
    fn len(&self) -> usize;
    /// Has this table no direct entries?
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Did this table come from a dotted key?
    fn is_dotted(&self) -> bool;
    /// Declare this table printed inline in its parent's key line.
    fn set_dotted(&mut self, dotted: bool);
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
    fn as_table(&self) -> Option<&Table> {
        Some(self)
    }
    fn as_inline_table(&self) -> Option<&super::InlineTable> {
        None
    }
}
