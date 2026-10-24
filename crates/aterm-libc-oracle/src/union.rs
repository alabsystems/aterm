// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reads and renders ([`Union::render`]) `libc-oracle/union.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_json::Value;

use crate::cells::{CELLS, Cell};

/// Where the list lives, under a checkout.
pub const PATH: &str = "libc-oracle/union.json";

/// The checkout's list file.
#[must_use]
pub fn path(root: &Path) -> PathBuf {
    root.join(PATH)
}

/// Why the list file could not be had ([`Union::read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unread {
    /// The file does not exist.
    Absent(String),
    /// The file is there and cannot be read or parsed: a finding about the tree.
    Unusable(String),
}

impl Unread {
    /// The message, whichever it is.
    #[must_use]
    pub fn into_detail(self) -> String {
        match self {
            Self::Absent(detail) | Self::Unusable(detail) => detail,
        }
    }
}

/// Every cell's item list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Union {
    lists: BTreeMap<&'static str, BTreeSet<String>>,
}

impl Union {
    /// Parse the file's text.
    ///
    /// # Errors
    /// When the text is not one object of string lists keyed by cell.
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value = aterm_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        let object = value.as_object().ok_or("not a JSON object")?;
        let mut lists = BTreeMap::new();
        for (key, names) in object {
            let cell = CELLS
                .iter()
                .find(|cell| cell.key == key)
                .ok_or_else(|| format!("`{key}` is not a cell"))?;
            let names = names
                .as_array()
                .ok_or_else(|| format!("`{key}` is not a list"))?
                .iter()
                .map(|name| {
                    name.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("a non-string name in `{key}`"))
                })
                .collect::<Result<BTreeSet<String>, String>>()?;
            lists.insert(cell.key, names);
        }
        Ok(Self { lists })
    }

    /// Read `<root>/libc-oracle/union.json`.
    ///
    /// # Errors
    /// [`Unread::Absent`] when missing; [`Unread::Unusable`] when unreadable or malformed.
    pub fn read(root: &Path) -> Result<Self, Unread> {
        let path = path(root);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            let said = format!("{}: {e}", path.display());
            if e.kind() == std::io::ErrorKind::NotFound {
                Unread::Absent(said)
            } else {
                Unread::Unusable(said)
            }
        })?;
        Self::parse(&text).map_err(|e| Unread::Unusable(format!("{}: {e}", path.display())))
    }

    /// [`Self::read`], either failure as its message.
    ///
    /// # Errors
    /// When the file is absent, unreadable or malformed; the message names it.
    pub fn load(root: &Path) -> Result<Self, String> {
        Self::read(root).map_err(Unread::into_detail)
    }

    /// The cell's names, sorted.
    ///
    /// # Errors
    /// When the file has no list for the cell.
    pub fn names(&self, cell: &Cell) -> Result<Vec<String>, String> {
        self.lists
            .get(cell.key)
            .map(|names| names.iter().cloned().collect())
            .ok_or_else(|| format!("no `{}` list", cell.key))
    }

    /// Add `names` to the cell's list; the ones it did not hold yet, sorted.
    pub fn widen(&mut self, cell: &Cell, names: impl IntoIterator<Item = String>) -> Vec<String> {
        let list = self.lists.entry(cell.key).or_default();
        let mut added: Vec<String> = names
            .into_iter()
            .filter(|name| list.insert(name.clone()))
            .collect();
        added.sort();
        added
    }

    /// The file's text: one list per cell, in table order, one-space indent.
    #[must_use]
    pub fn render(&self) -> String {
        let quote = |s: &str| aterm_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""));
        let lists: Vec<String> = CELLS
            .iter()
            .filter_map(|cell| Some((cell.key, self.lists.get(cell.key)?)))
            .map(|(key, names)| {
                let names: Vec<String> = names.iter().map(|n| format!("  {}", quote(n))).collect();
                format!(" {}: [\n{}\n ]", quote(key), names.join(",\n"))
            })
            .collect();
        format!("{{\n{}\n}}\n", lists.join(",\n"))
    }

    /// Write the file back.
    ///
    /// # Errors
    /// When the file cannot be written.
    pub fn store(&self, root: &Path) -> Result<(), String> {
        let path = path(root);
        std::fs::write(&path, self.render()).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cells_list_is_read_by_its_key() {
        let union = Union::parse(r#"{"mac":["stat","AF_INET"],"linux":[1]}"#);
        assert_eq!(union, Err("a non-string name in `linux`".into()));
        let union = Union::parse(r#"{"mac":["stat","AF_INET"]}"#).expect("parses");
        assert_eq!(
            union.names(&CELLS[0]),
            Ok(vec!["AF_INET".to_string(), "stat".to_string()])
        );
        assert!(union.names(&CELLS[1]).is_err());
        assert!(Union::parse(r#"{"win":[]}"#).is_err());
        assert!(Union::parse("[]").is_err());
    }

    #[test]
    fn widening_adds_only_new_names_and_renders_in_table_order() {
        let mut union = Union::parse(r#"{"linux":["b"],"mac":["a"]}"#).expect("parses");
        let added = union.widen(&CELLS[0], ["c".to_string(), "a".to_string()]);
        assert_eq!(added, ["c"]);
        let canonical = "{\n \"mac\": [\n  \"a\",\n  \"c\"\n ],\n \"linux\": [\n  \"b\"\n ]\n}\n";
        assert_eq!(union.render(), canonical);
        let reread = Union::parse(canonical).expect("parses");
        assert_eq!(reread.render(), canonical);
    }
}
