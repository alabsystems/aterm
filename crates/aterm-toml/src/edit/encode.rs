// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rendering a document back to text.
//!
//! For a document that came from a parse and was not modified, this is exactly
//! `concat` over the stored source fragments — every byte of the input landed
//! in some node's repr, decor, preamble, or trailing, so putting them back in
//! order reproduces the file. That is the property `tests/roundtrip.rs` asserts
//! over every `.toml` file in the repository.
//!
//! Nodes that were BUILT rather than parsed have no stored text, and fall back
//! to the canonical spacing named in the `DEFAULT_*` constants below.

use core::fmt::{self, Write as _};

use super::item::{ArrayOfTables, Item, Table};
use super::value::{Array, InlineTable, Value};
use super::{DocumentMut, Key};

/// Around a key on its own line: nothing before, one space before the `=`.
const DEFAULT_KEY_DECOR: (&str, &str) = ("", " ");
/// Around an intermediate segment of a dotted key: nothing on either side.
const DEFAULT_PATH_DECOR: (&str, &str) = ("", "");
/// Around a value on a key-value line: one space after `=`, then the newline.
const DEFAULT_VALUE_DECOR: (&str, &str) = (" ", "\n");
/// Around a table header that is not the first thing in the document: a blank
/// line before it, the newline after it.
const DEFAULT_TABLE_DECOR: (&str, &str) = ("\n", "\n");

impl fmt::Display for DocumentMut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        encode_document(&mut out, self.as_table(), self.trailing.as_str());
        f.write_str(&out)
    }
}

impl fmt::Display for Table {
    /// Renders the table's CONTENTS — its key-value lines and the sub-tables
    /// beneath it — without a header of its own, because a `Table` on its own
    /// does not know the path it would be headed by.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        encode_document(&mut out, self, "");
        f.write_str(&out)
    }
}

impl fmt::Display for ArrayOfTables {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        for table in &self.values {
            encode_document(&mut out, table, "");
        }
        f.write_str(&out)
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::None => Ok(()),
            Item::Value(v) => fmt::Display::fmt(v, f),
            Item::Table(t) => fmt::Display::fmt(t, f),
            Item::ArrayOfTables(a) => fmt::Display::fmt(a, f),
        }
    }
}

impl fmt::Display for Value {
    /// The value ALONE — its own decor (which, for a top-level assignment,
    /// carries the line's trailing comment and newline) is not part of it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        encode_value_body(&mut out, self);
        f.write_str(&out)
    }
}

impl fmt::Display for Array {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        encode_array(&mut out, self);
        f.write_str(&out)
    }
}

impl fmt::Display for InlineTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        encode_inline_table(&mut out, self);
        f.write_str(&out)
    }
}

/// Render a table as a whole document: its own lines, then every table beneath
/// it, in the order their headers were authored.
fn encode_document(out: &mut String, root: &Table, trailing: &str) {
    let mut tables: Vec<(Option<usize>, &Table, Vec<&Key>, bool)> = Vec::new();
    let mut last_position = None;
    let mut path: Vec<&Key> = Vec::new();
    visit_nested_tables(root, &mut path, false, &mut |table, path, is_array| {
        if let Some(position) = table.position {
            last_position = Some(position);
        }
        tables.push((last_position, table, path.to_vec(), is_array));
    });
    // Stable, so tables sharing an inherited position keep their document
    // order; the sort is what lets `[a.b]` appear before `[a]` in the source
    // and come back out that way.
    tables.sort_by_key(|(position, ..)| *position);

    for (_, table, table_path, is_array) in tables {
        encode_table(out, table, &table_path, is_array);
    }
    out.push_str(trailing);
}

/// Depth-first walk over the header-bearing tables under `table`.
///
/// A dotted table is not VISITED — it is printed on its parent's key-value
/// lines, not under a header of its own — but it is still DESCENDED INTO. The
/// spec lets a `[header]` add a sub-table beneath a dotted key
/// (`apple.color = "red"` then `[fruit.apple.texture]`, the spec's own worked
/// example), so a header-bearing table can hang under a dotted parent. Skipping
/// the subtree dropped those tables from the rendered document silently, which
/// is a round-trip that loses data rather than one that reorders it.
fn visit_nested_tables<'t>(
    table: &'t Table,
    path: &mut Vec<&'t Key>,
    is_array: bool,
    visit: &mut impl FnMut(&'t Table, &[&'t Key], bool),
) {
    if !table.dotted {
        visit(table, path, is_array);
    }
    for (key, item) in table.items.iter() {
        match item {
            Item::Table(child) => {
                path.push(key);
                visit_nested_tables(child, path, false, visit);
                path.pop();
            }
            Item::ArrayOfTables(array) => {
                path.push(key);
                for child in &array.values {
                    visit_nested_tables(child, path, true, visit);
                }
                path.pop();
            }
            _ => {}
        }
    }
}

fn encode_table(out: &mut String, table: &Table, path: &[&Key], is_array: bool) {
    let children = table.get_values();
    if !path.is_empty() && (is_array || !(table.implicit && children.is_empty())) {
        // The first header in a document gets no leading blank line; every
        // later one does. Authored decor overrides both.
        let default_prefix = if out.is_empty() {
            ""
        } else {
            DEFAULT_TABLE_DECOR.0
        };
        out.push_str(&table.decor.prefix_or(default_prefix));
        out.push_str(if is_array { "[[" } else { "[" });
        match &table.header_repr {
            Some(repr) => out.push_str(repr.as_str()),
            None => encode_path(out, path),
        }
        out.push_str(if is_array { "]]" } else { "]" });
        out.push_str(&table.decor.suffix_or(DEFAULT_TABLE_DECOR.1));
    }
    for (key_path, value) in children {
        encode_key_value(
            out,
            &key_path,
            value,
            DEFAULT_KEY_DECOR,
            DEFAULT_VALUE_DECOR,
        );
    }
}

fn encode_path(out: &mut String, path: &[&Key]) {
    for (index, key) in path.iter().enumerate() {
        if index > 0 {
            out.push('.');
        }
        out.push_str(&key.display_repr());
    }
}

fn encode_key_value(
    out: &mut String,
    path: &[&Key],
    value: &Value,
    key_decor: (&str, &str),
    value_decor: (&str, &str),
) {
    let leaf = path.last().expect("a key path has at least one segment");
    match &leaf.path_repr {
        // Authored: the head is literally the source bytes from the start of
        // the line to this key, so nothing about the line can be lost.
        Some(repr) => {
            out.push_str(repr.head.as_str());
            out.push_str(&leaf.display_repr());
            out.push_str(repr.tail.as_str());
        }
        None => {
            for (index, key) in path.iter().enumerate() {
                let last = index + 1 == path.len();
                let (default_prefix, default_suffix) = match (index, last) {
                    (0, true) => key_decor,
                    (0, false) => (key_decor.0, DEFAULT_PATH_DECOR.1),
                    (_, true) => (DEFAULT_PATH_DECOR.0, key_decor.1),
                    _ => DEFAULT_PATH_DECOR,
                };
                if index > 0 {
                    out.push('.');
                }
                out.push_str(&key.decor.prefix_or(default_prefix));
                out.push_str(&key.display_repr());
                out.push_str(&key.decor.suffix_or(default_suffix));
            }
        }
    }
    out.push('=');
    out.push_str(&value.decor().prefix_or(value_decor.0));
    encode_value_body(out, value);
    out.push_str(&value.decor().suffix_or(value_decor.1));
}

fn encode_value_body(out: &mut String, value: &Value) {
    match value {
        Value::String(v) => out.push_str(&v.display_repr()),
        Value::Integer(v) => out.push_str(&v.display_repr()),
        Value::Float(v) => out.push_str(&v.display_repr()),
        Value::Boolean(v) => out.push_str(&v.display_repr()),
        Value::Datetime(v) => out.push_str(&v.display_repr()),
        Value::Array(a) => encode_array(out, a),
        Value::InlineTable(t) => encode_inline_table(out, t),
    }
}

fn encode_array(out: &mut String, array: &Array) {
    out.push('[');
    for (index, value) in array.values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let default_prefix = if index == 0 { "" } else { " " };
        out.push_str(&value.decor().prefix_or(default_prefix));
        encode_value_body(out, value);
        out.push_str(&value.decor().suffix_or(""));
    }
    if array.trailing_comma {
        out.push(',');
    }
    out.push_str(array.trailing.as_str());
    out.push(']');
}

fn encode_inline_table(out: &mut String, table: &InlineTable) {
    out.push('{');
    out.push_str(table.preamble.as_str());
    let entries = table.get_values();
    let count = entries.len();
    for (index, (path, value)) in entries.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let value_suffix = if index + 1 == count { " " } else { "" };
        encode_key_value(out, &path, value, (" ", " "), (" ", value_suffix));
    }
    out.push('}');
}

/// The canonical spelling of a key: bare when the spec allows it, a basic
/// string otherwise.
pub(crate) fn encode_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    if bare {
        key.to_owned()
    } else {
        encode_basic_string(key)
    }
}

/// The canonical spelling of a string: a basic string with everything the spec
/// requires escaped, and nothing it does not.
pub(crate) fn encode_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            // `basic-unescaped` stops at %x7E, so DEL is escaped too.
            c if c < ' ' || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The canonical spelling of a float. Rust's `{}` is shortest-round-trip, but
/// it prints `1` for `1.0`, which TOML would read back as an integer — so a
/// bare mantissa gains a `.0`.
pub(crate) fn encode_float(value: f64) -> String {
    if value.is_nan() {
        return if value.is_sign_negative() {
            "-nan".to_owned()
        } else {
            "nan".to_owned()
        };
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf".to_owned()
        } else {
            "inf".to_owned()
        };
    }
    let text = format!("{value}");
    if text.contains(['.', 'e', 'E']) {
        text
    } else {
        text + ".0"
    }
}
