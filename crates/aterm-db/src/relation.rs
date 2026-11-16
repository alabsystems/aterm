// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A computed relation: a typed column list ([`Schema`]) and the rows a product computes for
//! it ([`Row`]), each a list of [`Cell`]s in the schema's order. One schema drives every face
//! of a relation: a command prints by it, and a sheet reads its columns, types and words from
//! it. The fleet relations (`fleet::rows`) are relations of this kind.
//!
//! A column is a `typed::Column` (the ATT/1 value types, so a value is checked as a registry
//! cell is) and the words that say what it holds.
//!
//! A cell that was not read is [`Cell::Unread`], which shows `?` and is JSON `null`, never a
//! zero; a cell that is known to hold nothing is [`Cell::Empty`]. Every face draws a cell
//! through [`Cell::shown`] (text) or [`Cell::json`] (JSON) and filters rows with [`kept`], so
//! one cell reads the same on each.

use crate::typed::{Column, Ty, cell::admissible, grid, secret};
use crate::{Error, Result};
use aterm_json::Value;

/// One column: its typed definition and what it holds.
#[derive(Clone, Copy, Debug)]
pub struct Col {
    pub column: Column,
    pub doc: &'static str,
}

/// A column every row holds a value in.
pub const fn col(name: &'static str, ty: Ty, doc: &'static str) -> Col {
    Col {
        column: Column::new(name, ty),
        doc,
    }
}

/// A column a row may hold no value in.
pub const fn opt(name: &'static str, ty: Ty, doc: &'static str) -> Col {
    Col {
        column: Column::new(name, ty).null(),
        doc,
    }
}

/// One relation: its name, its key, its order and its columns.
#[derive(Clone, Copy, Debug)]
pub struct Schema {
    pub name: &'static str,
    pub doc: &'static str,
    /// The columns that name a row.
    pub key: &'static [&'static str],
    /// The order rows come in, in words.
    pub order: &'static str,
    pub columns: &'static [Col],
}

impl Schema {
    /// The position of the column `name`.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.column.name == name)
    }
}

/// One cell of a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// A value of the column's type.
    Value(Value),
    /// Nobody read it: `?`, JSON `null`. Never a zero.
    Unread,
    /// It is known to hold nothing: an empty cell, JSON `null`.
    Empty,
}

impl Cell {
    /// A read value, else unread.
    pub fn read<T: Into<Value>>(value: Option<T>) -> Self {
        value.map_or(Self::Unread, |v| Self::Value(v.into()))
    }

    /// A value, else known empty.
    pub fn known<T: Into<Value>>(value: Option<T>) -> Self {
        value.map_or(Self::Empty, |v| Self::Value(v.into()))
    }

    /// A value.
    pub fn of<T: Into<Value>>(value: T) -> Self {
        Self::Value(value.into())
    }

    /// The cell, in a column of type `ty`, as a text face shows it: `?` for a cell nobody read,
    /// nothing for one known empty, and a value as the registry's grid shows a value of its type
    /// ([`grid::show`]).
    pub fn shown(&self, ty: &Ty) -> String {
        match self {
            Self::Unread => "?".to_owned(),
            Self::Empty => String::new(),
            Self::Value(value) => grid::show(ty, value),
        }
    }

    /// The cell, in a column of type `ty`, as JSON: `null` for a cell with no value, and a value
    /// of its column's type as it is. A sealed column's value is [`secret::MASK`], and a value
    /// that fails its type or the guards (another writer's build stored it) is the grid's raw
    /// value ([`grid::raw_shown`]), so the JSON shows nothing [`Self::shown`] withholds.
    pub fn json(&self, ty: &Ty) -> Value {
        match self {
            Self::Unread | Self::Empty => Value::Null,
            Self::Value(Value::Null) => Value::Null,
            Self::Value(_) if matches!(ty, Ty::Secret) => Value::String(secret::MASK.to_owned()),
            Self::Value(value) if !admissible(ty, value) => grid::raw_shown(value),
            Self::Value(value) => value.clone(),
        }
    }

    /// Whether the cell, in a column of type `ty`, reads as `text`: its shown text, or for a list
    /// cell any item's (for a list of records, any member of any item).
    pub fn matches(&self, ty: &Ty, text: &str) -> bool {
        let text = text.trim();
        if self.shown(ty) == text {
            return true;
        }
        let (Ty::List(inner, _), Self::Value(Value::Array(items))) = (ty, self) else {
            return false;
        };
        items.iter().any(|item| match item {
            Value::Object(members) => members.values().any(|member| match member {
                Value::String(word) => word == text,
                other => other.to_string() == text,
            }),
            other => grid::show(inner, other) == text,
        })
    }
}

/// A row of a relation.
pub trait Row {
    /// Its relation.
    const SCHEMA: &'static Schema;
    /// Its cells, one per column of [`Self::SCHEMA`], in order.
    fn cells(&self) -> Vec<Cell>;
}

/// The rows of `rows` every filter keeps, as cells: a filter `col=val` keeps a row whose cell
/// in `col` [`Cell::matches`] `val`.
///
/// # Errors
/// A filter names no column of the relation.
pub fn kept<R: Row>(rows: &[R], filters: &[grid::Where]) -> Result<Vec<Vec<Cell>>> {
    let schema = R::SCHEMA;
    let filters: Vec<(usize, &str)> = filters
        .iter()
        .map(|filter| {
            schema
                .position(&filter.column)
                .map(|at| (at, filter.text.as_str()))
                .ok_or_else(|| Error::new(&format!("{}: no such column to filter by", schema.name)))
        })
        .collect::<Result<_>>()?;
    Ok(rows
        .iter()
        .map(Row::cells)
        .filter(|cells| {
            filters
                .iter()
                .all(|(at, text)| cells[*at].matches(&schema.columns[*at].column.ty, text))
        })
        .collect())
}

#[cfg(all(test, unix))]
#[path = "relation_tests.rs"]
mod tests;
