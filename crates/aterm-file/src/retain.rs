// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a snapshot keeps of rows its store stops holding (docs/DESIGN-shared-files-2026-11-11.md
//! §2, Bounds: "Latest mine row kept 120 days from when this Mac last saw it change; log rows
//! 30 days; 4 MiB per collection").
//!
//! Some stores forget on a schedule of their own: the lab journal's week ring deletes a week's
//! table once it leaves the window, so a Mac's row that its writer has not restated since is in
//! no table any more. A projection of such a store carries the rows it last showed past that
//! point, for a span of this Mac's own time measured from when it last saw each row change, and
//! within a byte budget. Nothing here drops a record: a store's history is the store's (the
//! lab tracker's TK-D2, "a checkpoint compacts what a read walks, never what history holds");
//! this is only which rows a projection still shows.
//!
//! The rule counts no records: a row ages out or the budget's bytes are spent, never a count
//! (the owner, 2026-11-14, on record caps). It names no store, so any projection of a store
//! that forgets (the journal ring today, the per-Mac logs that replace it, the vault log's head)
//! can keep it.

use crate::Row;
use aterm_json::Value;

/// The derived column a kept row carries: this Mac's clock, Unix milliseconds, when it last saw
/// the row change. It is this Mac's own reading, never compared with another Mac's, and only
/// ages the row.
pub const SEEN: &str = "seen";

/// How long, and within how many bytes, a projection keeps rows its store no longer holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keep {
    /// How long after this Mac last saw a row change it is still shown, milliseconds.
    pub age_ms: u64,
    /// The most bytes the kept rows take in the snapshot ([`Row::encoded_len`]).
    pub bytes: usize,
}

const DAY_MS: u64 = 86_400_000;

/// A Mac's latest row of a table it states (a `mine` row): 120 days, 4 MiB.
pub const MINE: Keep = Keep {
    age_ms: 120 * DAY_MS,
    bytes: 4 * 1024 * 1024,
};

/// An entry of a log: 30 days, 4 MiB.
pub const LOG: Keep = Keep {
    age_ms: 30 * DAY_MS,
    bytes: 4 * 1024 * 1024,
};

/// When this Mac last saw `row` change, where the row says.
#[must_use]
pub fn seen(row: &Row) -> Option<u64> {
    row.values.get(SEEN).and_then(Value::as_u64)
}

/// Whether two rows say the same, what this Mac saw of them when left out.
fn same(a: &Row, b: &Row) -> bool {
    let cells = |row: &Row| {
        row.cells
            .iter()
            .filter(|(name, _)| *name != SEEN)
            .map(|(name, text)| (name.clone(), text.clone()))
            .collect::<Vec<_>>()
    };
    let values = |row: &Row| {
        row.values
            .iter()
            .filter(|(name, _)| *name != SEEN)
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>()
    };
    a.live == b.live && cells(a) == cells(b) && values(a) == values(b)
}

fn stamp(row: &mut Row, at_ms: u64) {
    row.values.insert(SEEN.to_owned(), Value::from(at_ms));
    row.cells.insert(SEEN.to_owned(), at_ms.to_string());
}

/// The rows a projection shows: every row `read` from the store now, each stamped with when this
/// Mac last saw it change (the time `previous` gave it where it says the same, else `now_ms`),
/// and each row of `previous` the store no longer holds while `keep.age_ms` has not passed
/// since then; then, while they take more than `keep.bytes`, the rows this Mac saw change least
/// recently are left out first (ties by key). The rows are in key order.
///
/// `previous` must be a projection of the same store's history: a projection of another replica
/// is no earlier reading of this one, and carrying its rows would show what this one never held.
/// A previous row that says no time is taken as seen now, so a snapshot an earlier projection
/// wrote ages from the first pass that reads it.
#[must_use]
pub fn carry(previous: &[Row], read: Vec<Row>, keep: Keep, now_ms: u64) -> Vec<Row> {
    let mut rows: std::collections::BTreeMap<String, Row> = std::collections::BTreeMap::new();
    for mut row in read {
        let at = previous
            .iter()
            .find(|held| held.pk == row.pk)
            .filter(|held| same(held, &row))
            .and_then(seen)
            .unwrap_or(now_ms);
        stamp(&mut row, at);
        rows.insert(row.pk.clone(), row);
    }
    for held in previous {
        if rows.contains_key(&held.pk) {
            continue;
        }
        let at = seen(held).unwrap_or(now_ms);
        if now_ms.saturating_sub(at) > keep.age_ms {
            continue;
        }
        let mut row = held.clone();
        stamp(&mut row, at);
        rows.insert(row.pk.clone(), row);
    }
    let mut total: usize = rows.values().map(Row::encoded_len).sum();
    if total > keep.bytes {
        let mut by_age: Vec<(u64, String)> = rows
            .values()
            .map(|row| (seen(row).unwrap_or(now_ms), row.pk.clone()))
            .collect();
        by_age.sort_unstable();
        for (_, pk) in by_age {
            if total <= keep.bytes {
                break;
            }
            if let Some(row) = rows.remove(&pk) {
                total -= row.encoded_len();
            }
        }
    }
    rows.into_values().collect()
}

#[cfg(test)]
#[path = "retain_tests.rs"]
mod tests;
