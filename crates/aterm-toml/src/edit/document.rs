// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The document: a root table plus whatever trailed the last entry.

use core::ops::{Index, IndexMut};

use super::{Item, RawString, Table};

/// Editable TOML document preserving formatting and comments.
#[derive(Debug, Clone)]
pub struct DocumentMut {
    pub(crate) root: Item,
    /// Whitespace and comments after the last entry, which belong to no node.
    pub(crate) trailing: RawString,
    /// The file opened with a UTF-8 byte-order mark. It is the file's, not any
    /// line's, so it prints first whatever an edit removes or adds.
    pub(crate) bom: bool,
}

impl Default for DocumentMut {
    fn default() -> Self {
        Self {
            root: Item::Table(Table::new()),
            trailing: RawString::default(),
            bom: false,
        }
    }
}

impl DocumentMut {
    /// An empty document.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The root table.
    #[inline]
    #[must_use]
    pub const fn as_table(&self) -> &Table {
        self.root
            .as_table()
            .expect("document root is always a table")
    }

    /// The root table, mutably.
    #[inline]
    pub const fn as_table_mut(&mut self) -> &mut Table {
        self.root
            .as_table_mut()
            .expect("document root is always a table")
    }

    /// The root as an [`Item`], for code that walks documents and sub-tables
    /// with the same loop.
    #[inline]
    #[must_use]
    pub const fn as_item(&self) -> &Item {
        &self.root
    }

    /// The root as a mutable [`Item`].
    #[inline]
    pub const fn as_item_mut(&mut self) -> &mut Item {
        &mut self.root
    }

    /// Top-level entries in authored order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Item)> {
        self.as_table().iter()
    }

    /// The top-level entry for `key`.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Item> {
        self.as_table().get(key)
    }

    /// Remove a top-level entry while preserving leading comments above key-value lines.
    pub fn remove(&mut self, key: &str) -> Option<Item> {
        self.remove_path(&[key])
    }

    /// Remove the entry at `path`, preserving leading comments above key-value lines.
    pub fn remove_path(&mut self, path: &[&str]) -> Option<Item> {
        let (leaf, parents) = path.split_last()?;
        let Some(depth) = line_owner_depth(self.as_table(), parents) else {
            let mut node = self.as_item_mut();
            for part in parents {
                node = node.as_table_like_mut()?.get_mut(part)?;
            }
            return node.as_table_like_mut()?.remove(leaf);
        };
        let owner = table_at_mut(self.as_table_mut(), &parents[..depth])?;
        let (key, item) = table_at_mut(owner, &parents[depth..])?
            .items
            .remove_entry(leaf)?;
        owner.keep_lines_above(&key);
        Some(item)
    }

    /// Remove the entry at `path` and prune any ancestor tables left empty and uncommented.
    pub fn remove_path_pruning(&mut self, path: &[&str]) -> Option<Item> {
        let removed = self.remove_path(path)?;
        for depth in (1..path.len()).rev() {
            let parent = &path[..depth];
            if self.item_at(parent).is_some_and(is_bare_table) {
                self.remove_path(parent);
                continue;
            }
            if let Some(table) = table_at_mut(self.as_table_mut(), parent)
                && is_bare_header(table)
            {
                table.implicit = true;
            }
            break;
        }
        Some(removed)
    }

    fn item_at(&self, path: &[&str]) -> Option<&Item> {
        let mut node = self.as_item();
        for part in path {
            node = node.as_table_like()?.get(part)?;
        }
        Some(node)
    }
}

fn is_bare_table(item: &Item) -> bool {
    match item {
        Item::Table(table) => table.items.len() == 0 && !table_has_comment(table),
        Item::Value(super::Value::InlineTable(table)) => {
            table.is_empty() && !commented(table.decor.prefix()) && !commented(table.decor.suffix())
        }
        _ => false,
    }
}

fn is_bare_header(table: &Table) -> bool {
    !table.implicit && !table.dotted && table.get_values().is_empty() && !table_has_comment(table)
}

fn table_has_comment(table: &Table) -> bool {
    commented(table.decor.prefix())
        || commented(table.decor.suffix())
        || table.trailing.as_str().contains('#')
}

fn commented(raw: Option<&RawString>) -> bool {
    raw.is_some_and(|raw| raw.as_str().contains('#'))
}

fn line_owner_depth(root: &Table, path: &[&str]) -> Option<usize> {
    let mut depth = 0;
    let mut table = root;
    for (step, part) in path.iter().enumerate() {
        table = table.get(part)?.as_table()?;
        if !table.dotted {
            depth = step + 1;
        }
    }
    Some(depth)
}

fn table_at_mut<'t>(mut table: &'t mut Table, path: &[&str]) -> Option<&'t mut Table> {
    for part in path {
        table = table.get_mut(part)?.as_table_mut()?;
    }
    Some(table)
}

impl Index<&str> for DocumentMut {
    type Output = Item;

    fn index(&self, key: &str) -> &Item {
        const VACANT: &Item = &Item::None;
        self.get(key).unwrap_or(VACANT)
    }
}

impl IndexMut<&str> for DocumentMut {
    fn index_mut(&mut self, key: &str) -> &mut Item {
        let table = self.as_table_mut();
        if !table.contains_key(key) {
            table.insert(key, Item::None);
        }
        table.get_mut(key).expect("just ensured present")
    }
}
