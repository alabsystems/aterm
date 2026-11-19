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
//! within the wider of their resolutions' tolerances of the latest instance's are that instance
//! ([`SAME_RESET_MS`]): an exact reset within a minute, one to the hour within an hour. A refill
//! that moves a reset by ten minutes is a new instance, which no older reading joins.
//!
//! **The account's walls and each model's.** A row's `wall` is the account's own: every
//! session of the account meets it. A model's own weekly wall walls that model's sessions alone
//! (FL-269), so each is folded apart, by its model's bucket, into `model_walls`; an older build's
//! `wall` of `7d-model`, which names no model, walls neither the account nor any model
//! ([`Reading::of_row`]). A row's `model_walls` is read entry by entry ([`model_walls_of`]), so a
//! newer build's entry costs no other its wall.
//!
//! **A Mac with two seats.** After a writer succession a Mac has rows under its fenced
//! predecessor's uuid and its successor's (§2, "Mine tables"). Where the registry binds both
//! uuids to one machine, the fold takes the current member's reading of a key and a
//! predecessor's only where the current one has none, so one Mac is one reader.

use crate::alab::journal::{ModelBucket, Res, mine::Wall, seen};
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
    "model_walls",
    "readers",
    "read_at",
];

/// The limit windows a `usage` row states: its percent column, then its reset column.
const WINDOWS: [(&str, &str); 3] = [
    ("w5h", "w5h_reset"),
    ("w7d", "w7d_reset"),
    ("w7dm", "w7dm_reset"),
];

/// The column a `usage` row (version 3 on) says how exactly each window's reset is known in, in
/// [`WINDOWS`]' order.
const RESOLUTIONS: [&str; 3] = ["w5h_res", "w7d_res", "w7dm_res"];

/// How far apart two Macs' resets of one window may be and still name one instance where a
/// reading does not say how exactly its reset is known: an hour. A reading states its reset's
/// resolution (`ai/usage` version 3, `w5h_res` and its kin), and two readings are one instance
/// when their resets lie within the wider of their resolutions' tolerances, the journal's own
/// fold's (`alab::journal::seen::tolerance`): a minute for a reset to the second (a statusline,
/// a probe, a Codex rollout, since two Macs round one server time differently), an hour for one
/// to the hour (a `/usage` screen's "Resets 1pm"), a day for one to the day. A row before
/// version 3 names none, and its reading may be a `/usage` screen's: it takes this hour.
///
/// Measured (SF-08c, m2, 2026-11-18, read-only) over the journal events this Mac holds, every
/// Mac's, own and relayed. The keys: each row's `pk` of `aterm file rows ai/accounts --json`
/// (156). The events: `aterm db alab log <key> --days 120` for each key (153 answered), its
/// `events` list. A reading of a reset is an event of kind `quota` with a `reset` and a `window`
/// of `5h`, `7d` or `7d-model`, kept as (reset, its Mac's `by.mc`, `src`, `at.t`), and its
/// resolution is its source's (`statusline` and `rollout` exact, `session-usage` the hour, as the
/// harness notes `rr`); a wall's is an event of kind `wall` with an `until`. A reading heard
/// twice (its Mac's own copy and a relay's) is that tuple once. An instance is one account's
/// (`ac`, `window`) readings sorted by reset and cut where two successive resets lie more than
/// half the window apart; its spread is its largest reset less its smallest, judged where two
/// Macs read it. Resets: 12,211 readings from 47 Macs (statusline 12,204, session-usage 4,
/// probe 3), 316 instances, 10 read by two Macs. Nine agree to the second, one of them an exact
/// reading beside one to the hour (the three readings to the hour that share an instance with
/// an exact one are each 0 s from it); one is 43,200 s apart, a week m65 read under the
/// account's key while it noted `wrong-account` about the account (12 events), which no
/// tolerance shorter than a window can join (docs/KNOWN_ISSUES.md, ATERM-KI-1118-0846). Walls'
/// `until`: 3,080 readings from 46 Macs, 106 instances, 4 read by two Macs, all 0 s apart. Every
/// week's reset fell on the hour (107 distinct), every five-hour window's on a ten-minute mark
/// (194 distinct, 76 on the hour), so a reading to the hour can lie 50 minutes from the exact
/// one: the hour. Of 195 pairs of successive distinct five-hour resets of one account, 192 lie
/// 18,000 s apart or more (139 exactly); the three closer, 600 s twice and 10,800 s once, each
/// begin within 131 s of the account's `reset-used`, a refill. So an exact reading takes a
/// minute, which the 0 s agreement fits and the 600 s refill passes: an hour for every reading
/// would join a refilled account's spent instance to its new one (2 of 195 here) and show it
/// spent until the Mac that read it spent reads it again or the new window resets.
pub const SAME_RESET_MS: u64 = 3_600_000;

/// How far, in milliseconds, a reading's reset stated at `res` may lie from another's of the
/// same instance: its resolution's tolerance ([`seen::tolerance`]), or [`SAME_RESET_MS`] where
/// the row names none.
fn tolerance_ms(res: Option<Res>) -> u64 {
    res.map_or(SAME_RESET_MS, |res| {
        seen::tolerance(Some(res)).saturating_mul(1000)
    })
}

/// The model walls `value` states (an `ai/usage` row's `model_walls`, or an `ai/limits`
/// row's), entry by entry: each object's `bucket` word as this build reads it
/// ([`ModelBucket::read`]: a word it does not know is [`ModelBucket::Other`]'s, a wall for every
/// session) with its `until` where the entry states one as a whole number. An entry with no
/// bucket word is no wall, and a member no build of this one names is passed over, so a newer
/// build's entry, member or bucket costs no other entry its wall.
pub(crate) fn model_walls_of(
    value: Option<&Value>,
) -> impl Iterator<Item = (ModelBucket, Option<u64>)> + '_ {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let bucket = entry.get("bucket")?.as_str()?;
            Some((
                ModelBucket::read(bucket),
                entry.get("until").and_then(Value::as_u64),
            ))
        })
}

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
    /// How exactly each window's reset is known, where the row says (version 3 on).
    pub resolutions: [Option<Res>; 3],
    /// The account-wide wall the Mac met (`none` where it met none).
    pub wall: Option<String>,
    pub wall_until: Option<u64>,
    /// Each model's own weekly wall the Mac met, by its bucket's word ([`ModelBucket`]), with
    /// its end where the row states one.
    pub model_walls: BTreeMap<String, Option<u64>>,
    /// The reading Mac's clock when it read: shown, never compared.
    pub read_at: Option<u64>,
}

impl Reading {
    /// The reading an `ai/usage` row of the snapshot states; `None` for a row whose key is not
    /// `<pk>.<uuid32>`. A `wall` that walls no account (an older build's `7d-model`, which names
    /// no model, [`Wall::walls_account`]) reads as `none` with no end; `model_walls` is read
    /// entry by entry ([`model_walls_of`]), two entries of one bucket as the later end.
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
        let mut wall = text("wall");
        let mut wall_until = uint("wall_until");
        if wall
            .as_deref()
            .and_then(Wall::parse)
            .is_some_and(|word| !word.walls_account())
        {
            wall = Some(Wall::None.word().to_owned());
            wall_until = None;
        }
        let mut model_walls = BTreeMap::new();
        for (bucket, until) in model_walls_of(row.values.get("model_walls")) {
            let held = model_walls.entry(bucket.word().to_owned()).or_insert(until);
            *held = (*held).max(until);
        }
        Some(Self {
            pk: pk.to_owned(),
            uuid: uuid.to_owned(),
            account: text("account"),
            windows: WINDOWS.map(|(percent, reset)| (uint(percent), uint(reset))),
            resolutions: RESOLUTIONS.map(|column| {
                row.values
                    .get(column)
                    .and_then(Value::as_str)
                    .and_then(Res::parse)
            }),
            wall,
            wall_until,
            model_walls,
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
    /// The account-wide wall whose `until` is the largest, and that `until`.
    pub wall: Option<String>,
    pub wall_until: Option<u64>,
    /// Each model's own weekly wall, by its bucket: the largest `until` any reading states of
    /// it, or none where every reading that names it states no end.
    pub model_walls: BTreeMap<String, Option<u64>>,
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

/// One Mac's reading of one window, as the fold of that window takes it.
#[derive(Clone, Copy, Debug)]
struct Stated {
    percent: Option<u64>,
    reset: Option<u64>,
    res: Option<Res>,
    /// The reading Mac's clock, which only the negative control folds by.
    #[cfg(test)]
    read_at: Option<u64>,
}

/// The fold of one window over `readings`: the latest reset among those that state a percent,
/// and the largest percent of the instance it names; where none states a reset, the largest
/// percent. The instance is the readings at the latest reset, and each reading whose reset lies
/// within the wider of its own and an instance member's tolerance ([`tolerance_ms`]) of that
/// member's: a set closed under that rule, so it depends on the set of readings alone.
fn window(readings: &[Stated]) -> (Option<u64>, Option<u64>) {
    #[cfg(test)]
    if seam::by_clock() {
        // The negative control: the reading whose Mac's clock read last wins.
        return readings
            .iter()
            .filter(|stated| stated.percent.is_some())
            .max_by_key(|stated| stated.read_at)
            .map_or((None, None), |stated| (stated.percent, stated.reset));
    }
    let stated: Vec<&Stated> = readings
        .iter()
        .filter(|stated| stated.percent.is_some())
        .collect();
    let Some(latest) = stated.iter().filter_map(|stated| stated.reset).max() else {
        return (
            stated.iter().filter_map(|stated| stated.percent).max(),
            None,
        );
    };
    // Two readings of one instance: their resets within the wider of their tolerances.
    let near = |a: &Stated, b: &Stated| match (a.reset, b.reset) {
        (Some(ours), Some(theirs)) => {
            ours.abs_diff(theirs) <= tolerance_ms(a.res).max(tolerance_ms(b.res))
        }
        _ => false,
    };
    let mut joined: Vec<bool> = stated
        .iter()
        .map(|stated| stated.reset == Some(latest))
        .collect();
    loop {
        let joins: Vec<usize> = (0..stated.len())
            .filter(|&i| {
                !joined[i]
                    && stated
                        .iter()
                        .zip(&joined)
                        .any(|(member, &member_joined)| member_joined && near(stated[i], member))
            })
            .collect();
        if joins.is_empty() {
            break;
        }
        for i in joins {
            joined[i] = true;
        }
    }
    let percent = stated
        .iter()
        .zip(&joined)
        .filter(|(_, joined)| **joined)
        .filter_map(|(stated, _)| stated.percent)
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
            let column: Vec<Stated> = folded
                .iter()
                .map(|r| Stated {
                    percent: r.windows[i].0,
                    reset: r.windows[i].1,
                    res: r.resolutions[i],
                    #[cfg(test)]
                    read_at: r.read_at,
                })
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
        // Each model's wall by the same rule, apart from the account's and from every other
        // model's: the largest `until`, a wall stating none only where no reading states one.
        for reading in &folded {
            for (bucket, until) in &reading.model_walls {
                let held = limit.model_walls.entry(bucket.clone()).or_insert(*until);
                *held = (*held).max(*until);
            }
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
        if !self.model_walls.is_empty() {
            put(
                "model_walls",
                Value::Array(
                    self.model_walls
                        .iter()
                        .map(|(bucket, until)| {
                            let mut entry = aterm_json::Map::new();
                            entry.insert("bucket".to_owned(), Value::from(bucket.as_str()));
                            if let Some(until) = until {
                                entry.insert("until".to_owned(), Value::from(*until));
                            }
                            Value::Object(entry)
                        })
                        .collect(),
                ),
            );
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
