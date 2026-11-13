// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table at a past point, and what changed between two points (docs/PLAN.md LT-05).
//!
//! A point is an operation digest: the causal cut it names is that operation and everything it
//! observed through its signed parents and its writer's chain (`Log::as_of`), projected by the
//! one read every surface uses ([`View::read`]). An authored time never names a point: it is a
//! writer's clock claim and never orders the log. A comparison runs forward, from a point to one
//! that observed it (or to the head), so every cell that differs was made by operations of the
//! later cut, and each difference names them.

use super::View;
use crate::{
    Result,
    database::{Candidate, Cell},
    typed::{
        Kind, Table, Ty,
        cell::{CellKey, CellValue},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_json::Value;
use std::collections::BTreeSet;

/// A point of a table's history: the head (everything this Mac retains), or the causal cut an
/// operation names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    Head,
    At(Digest),
}

impl Point {
    /// `head`, or an operation's 64 lower-case hex digits as `aterm db table history` prints it.
    ///
    /// # Errors
    /// The word is neither.
    pub fn parse(word: &str) -> Result<Self> {
        if word == "head" {
            return Ok(Self::Head);
        }
        crate::database::decode_hex::<32>(word).map(Self::At)
    }
}

/// How a cell differs between two points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Delta {
    /// It held no value at the first point and holds one at the second.
    Added,
    /// It held a value at the first point and holds none at the second: cleared (and no
    /// concurrent value beside the clear), a set's member taken out, or a revert back to absence.
    Retired,
    /// One value at each point, and they differ.
    Changed,
    /// Concurrent values at either point (a clear beside a value among them), and the values
    /// differ.
    ConflictChanged,
}

impl Delta {
    /// `added`, `retired`, `changed`, `conflict-changed`.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Retired => "retired",
            Self::Changed => "changed",
            Self::ConflictChanged => "conflict-changed",
        }
    }
}

/// The operation that made a difference, its writer and the writer's Commons machine (from the
/// binding in the roster the record names, as history resolves it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Maker {
    pub operation: Digest,
    pub actor: Actor,
    pub machine: Option<[u8; 16]>,
}

/// One cell, or one owned row, that differs between two points.
#[derive(Clone, Debug, PartialEq)]
pub struct CellDiff {
    pub collection: String,
    /// The stored cell key (`<pk>.<column>`, `<pk>.<column>.<member>` for a set's member), or an
    /// owned collection's row key.
    pub key: String,
    pub delta: Delta,
    /// The candidates at the first point, and at the second, as stored.
    pub before: Vec<Candidate>,
    pub after: Vec<Candidate>,
    /// The operations of the later point's candidates that the first point did not hold: the
    /// writes that made the difference. Empty only where the later point holds no candidate.
    pub by: Vec<Maker>,
}

/// What changed between two points.
#[derive(Clone, Debug, PartialEq)]
pub struct Diff {
    pub from: Digest,
    pub to: Point,
    pub cells: Vec<CellDiff>,
}

/// A candidate's value as its column reads it: a typed cell's `x`, a stored null (a revert back
/// to absence) as null.
fn x_of(value: &Value) -> Value {
    match CellValue::decode(value) {
        Ok(cell) => cell.x,
        Err(_) => value.clone(),
    }
}

/// The distinct values `candidates` hold, in one order, as the grid reads them
/// (`typed::cell::field`): a clear (`x: null`) beside another value is a side of a conflict, and
/// only clears alone, or a revert back to absence, hold nothing. A set's member is add-wins, so
/// a member taken out (`x: false`) holds nothing whatever stands beside it.
fn values(member: bool, candidates: &[Candidate]) -> Vec<Value> {
    let mut found: Vec<(String, Value)> = candidates
        .iter()
        .map(|c| x_of(&c.value))
        .filter(|x| !(member && *x == Value::Bool(false)))
        .map(|x| (aterm_json::to_string(&x).unwrap_or_default(), x))
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);
    if found.iter().all(|(_, x)| x.is_null()) {
        return Vec::new();
    }
    found.into_iter().map(|(_, x)| x).collect()
}

/// How `before` became `after`, or `None` where they hold the same values.
fn delta(member: bool, before: &[Candidate], after: &[Candidate]) -> Option<Delta> {
    let (was, is) = (values(member, before), values(member, after));
    if was == is {
        return None;
    }
    Some(if was.len() > 1 || is.len() > 1 {
        Delta::ConflictChanged
    } else if was.is_empty() {
        Delta::Added
    } else if is.is_empty() {
        Delta::Retired
    } else {
        Delta::Changed
    })
}

/// Whether the stored cell key `key` of `collection` is a member of a set column.
fn is_member(table: &Table, collection: &str, key: &str) -> bool {
    CellKey::parse(key).is_ok_and(|key| {
        key.elem.is_some()
            && table
                .collection(collection)
                .and_then(|c| c.column(key.col))
                .is_some_and(|column| matches!(column.ty, Ty::Set(_)))
    })
}

/// Every cell and owned row of `table` that differs from `before` to `after`, in collection and
/// key order, each with the operations that made it, as `maker` resolves them.
pub(crate) fn between(
    table: &Table,
    before: &View,
    after: &View,
    maker: impl Fn(&Digest) -> Option<Maker>,
) -> Vec<CellDiff> {
    let owned = |name: &str| {
        table
            .collection(name)
            .is_some_and(|c| c.kind == Kind::Owned)
    };
    let empty = Cell::default();
    let keys: BTreeSet<&(String, String)> = before
        .cells
        .keys()
        .chain(after.cells.keys())
        .filter(|(collection, _)| !owned(collection))
        .collect();
    let mut found = Vec::new();
    let mut push = |collection: &str, key: &str, before: Vec<Candidate>, after: Vec<Candidate>| {
        let member = is_member(table, collection, key);
        let Some(delta) = delta(member, &before, &after) else {
            return;
        };
        let held: BTreeSet<Digest> = before.iter().map(|c| c.operation).collect();
        let mut seen = BTreeSet::new();
        let by = after
            .iter()
            .map(|c| c.operation)
            .filter(|id| !held.contains(id) && seen.insert(*id))
            .filter_map(|id| maker(&id))
            .collect();
        found.push(CellDiff {
            collection: collection.to_owned(),
            key: key.to_owned(),
            delta,
            before,
            after,
            by,
        });
    };
    for key in keys {
        let was = before.cells.get(key).unwrap_or(&empty);
        let is = after.cells.get(key).unwrap_or(&empty);
        push(
            &key.0,
            &key.1,
            was.candidates.clone(),
            is.candidates.clone(),
        );
    }
    for collection in table.collections.iter().filter(|c| c.kind == Kind::Owned) {
        let rows = |view: &View| {
            view.owned
                .get(collection.name)
                .map(|read| {
                    read.rows
                        .iter()
                        .map(|(pk, row)| {
                            (
                                pk.clone(),
                                Candidate {
                                    operation: row.operation,
                                    value: row.value.clone(),
                                },
                            )
                        })
                        .collect::<std::collections::BTreeMap<_, _>>()
                })
                .unwrap_or_default()
        };
        let (was, is) = (rows(before), rows(after));
        let pks: BTreeSet<&String> = was.keys().chain(is.keys()).collect();
        for pk in pks {
            push(
                collection.name,
                pk,
                was.get(pk).cloned().into_iter().collect(),
                is.get(pk).cloned().into_iter().collect(),
            );
        }
    }
    found.sort_by(|a, b| (&a.collection, &a.key).cmp(&(&b.collection, &b.key)));
    found
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
