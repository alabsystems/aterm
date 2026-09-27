// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The document: a root table plus whatever trailed the last entry.

use core::ops::{Index, IndexMut};

use super::{Item, RawString, Table};

/// A whole TOML document, editable in place, printing back byte-for-byte what
/// was parsed until something is changed.
#[derive(Debug, Clone)]
pub struct DocumentMut {
    pub(crate) root: Item,
    /// Whitespace and comments after the last entry. They belong to no node, so
    /// the document holds them; without this, a file's closing comment block
    /// would vanish on the first save.
    pub(crate) trailing: RawString,
}

impl Default for DocumentMut {
    fn default() -> Self {
        Self {
            root: Item::Table(Table::new()),
            trailing: RawString::default(),
        }
    }
}

impl DocumentMut {
    /// An empty document.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The root table.
    #[must_use]
    pub fn as_table(&self) -> &Table {
        self.root
            .as_table()
            .expect("document root is always a table")
    }

    /// The root table, mutably.
    pub fn as_table_mut(&mut self) -> &mut Table {
        self.root
            .as_table_mut()
            .expect("document root is always a table")
    }

    /// The root as an [`Item`], for code that walks documents and sub-tables
    /// with the same loop.
    #[must_use]
    pub fn as_item(&self) -> &Item {
        &self.root
    }

    /// The root as a mutable [`Item`].
    pub fn as_item_mut(&mut self) -> &mut Item {
        &mut self.root
    }

    /// Top-level entries in authored order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Item)> {
        self.as_table().iter()
    }

    /// The top-level entry for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.as_table().get(key)
    }

    /// Delete a top-level entry.
    pub fn remove(&mut self, key: &str) -> Option<Item> {
        self.as_table_mut().remove(key)
    }
}

impl Index<&str> for DocumentMut {
    type Output = Item;

    fn index(&self, key: &str) -> &Item {
        const VACANT: &Item = &Item::None;
        self.get(key).unwrap_or(VACANT)
    }
}

impl IndexMut<&str> for DocumentMut {
    /// Indexing for WRITE vivifies the key, so `doc["font_px"] = item` works on
    /// a key the file never had — the assignment path the Preferences window
    /// takes for a setting the user is enabling for the first time.
    fn index_mut(&mut self, key: &str) -> &mut Item {
        let table = self.as_table_mut();
        if !table.contains_key(key) {
            table.insert(key, Item::None);
        }
        table.get_mut(key).expect("just ensured present")
    }
}
