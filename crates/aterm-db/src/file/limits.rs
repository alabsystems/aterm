// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ai/limits`, the first derived file (docs/DESIGN-shared-files-2026-11-11.md §2, "Views across
//! Macs"; §10 file 6): every Mac's `ai/usage` rows folded into one row per account or
//! sub-account. A fleet reading is a fold, never a stored winner, so nobody writes this file
//! and no Mac's choice of a winner travels.
//!
//! **No clock orders two readings.** A window instance is (the window, the provider's reset):
//! within one instance the largest percent, across instances the largest reset, and a wall is
//! the largest `until`. A reading's `read_at` is its Mac's clock and is shown, never compared:
//! the fold is a function of the set of readings, so two Macs that hold the same rows fold the
//! same row whichever read last ([`fold`]; the `SharedFile` model's `LimitsByReset`).
//!
//! **One instance, two spellings.** Two Macs state one reset a little apart: a statusline's
//! instant to the second, a `/usage` screen's "Resets 1pm" to the hour, a Codex reading
//! computed from a delay on the reading Mac's clock. Readings of one window whose resets lie
//! within [`SAME_RESET_MS`] of the latest are that instance.
//!
//! **A Mac with two seats.** After a writer succession a Mac has rows under its fenced
//! predecessor's uuid and its successor's (§2, "Mine tables"). Where the registry binds both
//! uuids to one machine, the fold takes the current member's reading of a key and a
//! predecessor's only where the current one has none, so one Mac is one reader.

use aterm_file::Row;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The view's columns, in face order (all computed).
pub const COLUMNS: &[&str] = &[
    "account",
    "account_slug",
    "account_name",
    "w5h",
    "w5h_reset",
    "w7d",
    "w7d_reset",
    "w7dm",
    "w7dm_reset",
    "wall",
    "wall_until",
    "readers",
    "read_at",
];

/// The limit windows a `usage` row states: its percent column, then its reset column.
const WINDOWS: [(&str, &str); 3] = [
    ("w5h", "w5h_reset"),
    ("w7d", "w7d_reset"),
    ("w7dm", "w7dm_reset"),
];

/// How far apart two Macs' resets of one window may be and still name one instance: an hour.
/// The harness states a reset to the second (a statusline, a probe, a Codex rollout) or to the
/// hour (a `/usage` screen's "Resets 1pm"), and the journal's own fold of the same readings
/// takes an hour-resolution reset to agree within an hour (`alab::journal::seen`'s tolerance).
/// Every window is five hours or longer, so an hour never joins two instances of one window.
/// SF-08b measures the agreement across the fleet's readings and sets this from it.
pub const SAME_RESET_MS: u64 = 3_600_000;

/// One Mac's reading of one account or sub-account, as its `ai/usage` row states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The key it is about: the row key's account or sub-account part (`ac-…`, `sa-…`).
    pub pk: String,
    /// The reading Mac's Commons uuid, 32 hex: the row key's suffix.
    pub uuid: String,
    /// The account a sub-account's row names, or the account's own.
    pub account: Option<String>,
    /// Each window's tenths of a percent used and the provider's reset, Unix milliseconds.
    pub windows: [(Option<u64>, Option<u64>); 3],
    /// The wall the Mac met (`none` where it met none).
    pub wall: Option<String>,
    pub wall_until: Option<u64>,
    /// The reading Mac's clock when it read: shown, never compared.
    pub read_at: Option<u64>,
}

impl Reading {
    /// The reading an `ai/usage` row of the snapshot states; `None` for a row whose key is not
    /// `<pk>.<uuid32>`.
    #[must_use]
    pub fn of_row(row: &Row) -> Option<Self> {
        let (pk, uuid) = row.pk.rsplit_once('.')?;
        crate::typed::keys::parse_uuid32(uuid)?;
        let uint = |column: &str| row.values.get(column).and_then(Value::as_u64);
        let text = |column: &str| {
            row.values
                .get(column)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Some(Self {
            pk: pk.to_owned(),
            uuid: uuid.to_owned(),
            account: text("account"),
            windows: WINDOWS.map(|(percent, reset)| (uint(percent), uint(reset))),
            wall: text("wall"),
            wall_until: uint("wall_until"),
            read_at: uint("read_at"),
        })
    }
}

/// One account's or sub-account's fold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Limit {
    pub account: Option<String>,
    /// Each window's percent and reset: the latest instance's largest percent, at the latest
    /// reset.
    pub windows: [(Option<u64>, Option<u64>); 3],
    /// The wall whose `until` is the largest, and that `until`.
    pub wall: Option<String>,
    pub wall_until: Option<u64>,
    /// How many Macs' readings were folded.
    pub readers: u64,
    /// The newest `read_at` of those readings, for display.
    pub read_at: Option<u64>,
}

/// Which Mac each Commons uuid is, where the registry binds it, and which uuids are the lab's
/// current members: a predecessor's reading gives way to its successor's.
#[derive(Clone, Debug, Default)]
pub struct Seats {
    /// uuid (32 hex) to the machine its facts row binds.
    pub bound: BTreeMap<String, String>,
    /// The current members' uuids.
    pub current: BTreeSet<String>,
}

impl Seats {
    /// Whether `reading` is a predecessor's that a current member's reading of the same key on
    /// the same machine replaces.
    fn superseded(&self, reading: &Reading, all: &[&Reading]) -> bool {
        if self.current.contains(&reading.uuid) {
            return false;
        }
        let Some(machine) = self.bound.get(&reading.uuid) else {
            return false;
        };
        all.iter().any(|other| {
            other.pk == reading.pk
                && self.current.contains(&other.uuid)
                && self.bound.get(&other.uuid) == Some(machine)
        })
    }
}

/// The fold of one window over `readings`: the latest reset among those that state a percent,
/// and the largest percent of the readings within [`SAME_RESET_MS`] of it; where none states a
/// reset, the largest percent.
fn window(readings: &[(Option<u64>, Option<u64>, Option<u64>)]) -> (Option<u64>, Option<u64>) {
    #[cfg(test)]
    if seam::by_clock() {
        // The negative control: the reading whose Mac's clock read last wins.
        return readings
            .iter()
            .filter(|(percent, _, _)| percent.is_some())
            .max_by_key(|(_, _, read_at)| *read_at)
            .map_or((None, None), |(percent, reset, _)| (*percent, *reset));
    }
    let stated = || readings.iter().filter(|(percent, _, _)| percent.is_some());
    let Some(latest) = stated().filter_map(|(_, reset, _)| *reset).max() else {
        return (stated().filter_map(|(percent, _, _)| *percent).max(), None);
    };
    let floor = latest.saturating_sub(SAME_RESET_MS);
    let percent = stated()
        .filter(|(_, reset, _)| reset.is_some_and(|reset| reset >= floor))
        .filter_map(|(percent, _, _)| *percent)
        .max();
    (percent, Some(latest))
}

/// Every account's or sub-account's fold of `readings`, by key. It depends only on the set of
/// readings, never on their order or on any Mac's clock.
///
/// The model's `Fold` is this function; a materializer pass runs it too, inside the pass's
/// `Materialize` step, where it projects `ai/limits` from the `ai/usage` rows it projected.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Materialize",
        project = "aterm_db::file::model_tests::read_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Fold",
        project = "aterm_db::file::model_tests::rotation_transition"
    )
)]
#[must_use]
pub fn fold(readings: &[Reading], seats: &Seats) -> BTreeMap<String, Limit> {
    let mut by_pk: BTreeMap<&str, Vec<&Reading>> = BTreeMap::new();
    for reading in readings {
        by_pk.entry(&reading.pk).or_default().push(reading);
    }
    let mut out = BTreeMap::new();
    for (pk, all) in by_pk {
        let folded: Vec<&Reading> = all
            .iter()
            .copied()
            .filter(|reading| !seats.superseded(reading, &all))
            .collect();
        let mut limit = Limit {
            account: folded.iter().filter_map(|r| r.account.clone()).min(),
            readers: folded
                .iter()
                .map(|r| r.uuid.as_str())
                .collect::<BTreeSet<_>>()
                .len() as u64,
            read_at: folded.iter().filter_map(|r| r.read_at).max(),
            ..Limit::default()
        };
        for (i, slot) in limit.windows.iter_mut().enumerate() {
            let column: Vec<_> = folded
                .iter()
                .map(|r| (r.windows[i].0, r.windows[i].1, r.read_at))
                .collect();
            *slot = window(&column);
        }
        // A wall is the largest `until`; of walls that state none, the smallest word, so a set
        // of readings names one wall whatever order it came in.
        let walls = || {
            folded
                .iter()
                .filter(|r| r.wall.as_deref().is_some_and(|wall| wall != "none"))
        };
        let wall = walls()
            .filter(|r| r.wall_until.is_some())
            .max_by(|a, b| {
                a.wall_until
                    .cmp(&b.wall_until)
                    .then_with(|| b.wall.cmp(&a.wall))
            })
            .or_else(|| walls().min_by(|a, b| a.wall.cmp(&b.wall)));
        if let Some(wall) = wall {
            limit.wall.clone_from(&wall.wall);
            limit.wall_until = wall.wall_until;
        } else if folded.iter().any(|r| r.wall.is_some()) {
            limit.wall = Some("none".to_owned());
        }
        out.insert(pk.to_owned(), limit);
    }
    out
}

impl Limit {
    /// The view's row for `pk`, `slug` and `name` the account's where the registry names it.
    #[must_use]
    pub fn row(&self, pk: &str, slug: Option<&str>, name: Option<&str>) -> Row {
        let mut row = Row {
            pk: pk.to_owned(),
            live: true,
            ..Row::default()
        };
        let mut put = |column: &str, value: Value| {
            let shown = match &value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            row.cells.insert(column.to_owned(), shown);
            row.values.insert(column.to_owned(), value);
        };
        if let Some(account) = &self.account {
            put("account", Value::from(account.as_str()));
        }
        if let Some(slug) = slug {
            put("account_slug", Value::from(slug));
        }
        if let Some(name) = name {
            put("account_name", Value::from(name));
        }
        for ((percent, reset), (percent_column, reset_column)) in self.windows.iter().zip(WINDOWS) {
            if let Some(percent) = percent {
                put(percent_column, Value::from(*percent));
            }
            if let Some(reset) = reset {
                put(reset_column, Value::from(*reset));
            }
        }
        if let Some(wall) = &self.wall {
            put("wall", Value::from(wall.as_str()));
        }
        if let Some(until) = self.wall_until {
            put("wall_until", Value::from(until));
        }
        put("readers", Value::from(self.readers));
        if let Some(read_at) = self.read_at {
            put("read_at", Value::from(read_at));
        }
        row
    }
}

/// The negative control of the conformance and of the exit test: a fold that takes the reading
/// whose Mac's clock read last (the model's `ClockOrdersReadings`).
#[cfg(test)]
#[path = "limits_seam_tests.rs"]
pub(crate) mod seam;
