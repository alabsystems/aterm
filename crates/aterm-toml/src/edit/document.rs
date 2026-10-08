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

    /// Delete a top-level entry: [`DocumentMut::remove_path`] with a
    /// one-segment path, so a key-value line leaves the comment lines written
    /// above it, and a table takes its own along.
    pub fn remove(&mut self, key: &str) -> Option<Item> {
        self.remove_path(&[key])
    }

    /// Delete the entry at a key path — `["net", "listen"]` for `net.listen` —
    /// through either table spelling. `None`, with nothing changed, when a
    /// step is missing or is not a table; the tables along the path stay, even
    /// emptied.
    ///
    /// A removed key-value line goes with its own text, its same-line comment
    /// included, and nothing else. The comment and blank lines written ABOVE it
    /// are the person's, but the parser files them in the line's head, so
    /// [`Table::remove`] takes them along: resetting a `theme` took the list of
    /// commented-out themes above it. Here they stay where they were written:
    /// above the next line of the table that printed the removed one, or after
    /// that table's last line when it was the last. For a dotted key that is
    /// the table its LINE sits in, not the one its key does.
    ///
    /// That is for a key-value line. A TABLE goes whole, `[header]`-spelled or
    /// dotted: every line it printed, with the comment lines above and among
    /// them.
    pub fn remove_path(&mut self, path: &[&str]) -> Option<Item> {
        let (leaf, parents) = path.split_last()?;
        let Some(depth) = line_owner_depth(self.as_table(), parents) else {
            // Past `{ … }` there are no lines, only entries on one line, so
            // nothing is written above an entry.
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
}

/// How many of `path`'s steps lead to the table that prints the lines of the
/// table `path` names: the last non-dotted one (or the root), since a dotted
/// table's lines print among its parent's. `None` when a step is not a table
/// at all — an inline table, an array of tables, a plain value — or is missing.
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
