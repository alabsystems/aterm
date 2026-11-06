// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owned and log collections, the schema collection and the per-Mac quota (design §3.1, §3.5,
//! §3.7), read from a table's eligible operations rather than its cells, since a whole-row
//! replacement in cells would let one writer's record hide another's.
//!
//! An owned row lives at its writer's own Commons machine uuid (or `<pk>.<uuid>`); a row at
//! any other key is inert and counted, as for the claims rows. The writer's machine is
//! resolved through the binding in **the roster the record names** (`Session::
//! historical_binding`), never through current membership alone, so a removed Mac's rows stay
//! attributable. Each key's latest row ranks by (period, n, sequence).

use super::{Collection, Kind, SCHEMA, Table, Writers, keys};
use crate::database::Change;
use astream_wire::sync::{Actor, Digest};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One eligible operation, as the typed readers need it.
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub id: Digest,
    pub actor: Actor,
    pub sequence: u64,
    /// The roster the record was sealed under, which resolves its writer's machine.
    pub roster: Digest,
    /// The period table it was read from (a ring's period; 0 for a catalog table).
    pub period: u64,
    /// A `Revert`: the owner's compensation of one earlier operation, never automatic.
    pub revert: bool,
    pub changes: Vec<Change>,
}

/// One key's latest owned row.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedRow {
    pub machine: [u8; 16],
    pub actor: Actor,
    pub value: Value,
    /// (period, n, sequence).
    pub rank: (u64, u64, u64),
    pub operation: Digest,
}

/// An owned collection read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnedRead {
    pub rows: BTreeMap<String, OwnedRow>,
    /// Rows whose key is not their signer's machine, or whose body is not a bounded object
    /// with a counter: never projected.
    pub inert: usize,
}

/// The machine uuid a key of `collection` names, when it is well formed.
fn owner_of(collection: &Collection, key: &str) -> Option<[u8; 16]> {
    match collection.writers {
        Writers::SelfKey => keys::parse_uuid32(key),
        Writers::SelfSuffix => {
            let (pk, uuid) = key.rsplit_once('.')?;
            super::pk_grammar(pk).then_some(())?;
            keys::parse_uuid32(uuid)
        }
        Writers::Owner => None,
    }
}

fn bounded(collection: &Collection, value: &Value) -> bool {
    value.is_object()
        && aterm_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= collection.max_row)
}

/// Each key's latest row of an owned collection, keeping only rows whose key names the
/// machine `resolve` finds for the record's (roster, actor).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "OwnA",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "OwnB",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "ForgeB",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
pub fn read_owned(
    collection: &Collection,
    ops: &[Op],
    resolve: impl Fn(&Digest, &Actor) -> Option<[u8; 16]>,
) -> OwnedRead {
    let mut read = OwnedRead::default();
    if collection.kind != Kind::Owned {
        return read;
    }
    for op in ops {
        let mut signer = None;
        for change in op
            .changes
            .iter()
            .filter(|c| c.collection == collection.name)
        {
            let Some(value) = &change.value else {
                read.inert += 1;
                continue;
            };
            let named = owner_of(collection, &change.key);
            let signer = *signer.get_or_insert_with(|| resolve(&op.roster, &op.actor));
            let n = value.get("n").and_then(Value::as_u64);
            let (Some(machine), Some(n)) = (named.filter(|m| Some(*m) == signer), n) else {
                read.inert += 1;
                continue;
            };
            if !bounded(collection, value) {
                read.inert += 1;
                continue;
            }
            let rank = (op.period, n, op.sequence);
            if read
                .rows
                .get(&change.key)
                .is_none_or(|held| held.rank < rank)
            {
                read.rows.insert(
                    change.key.clone(),
                    OwnedRow {
                        machine,
                        actor: op.actor,
                        value: value.clone(),
                        rank,
                        operation: op.id,
                    },
                );
            }
        }
    }
    read
}

/// One entry of a log collection.
#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub machine: [u8; 16],
    pub actor: Actor,
    /// The writer's batch counter, from the key `<uuid32>.<n>`.
    pub n: u64,
    pub sequence: u64,
    pub period: u64,
    pub value: Value,
    pub operation: Digest,
}

/// A log collection read: every writer's entries, in (machine, n) order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogRead {
    pub entries: Vec<LogEntry>,
    pub inert: usize,
}

/// Read a log collection: append-only per writer, `<uuid32>.<n>` keys, each inert unless its
/// uuid is its signer's machine. A counter a writer reused keeps its first entry, by period and
/// then by sequence within the period's table.
pub fn read_log(
    collection: &Collection,
    ops: &[Op],
    resolve: impl Fn(&Digest, &Actor) -> Option<[u8; 16]>,
) -> LogRead {
    let mut read = LogRead::default();
    if collection.kind != Kind::Log {
        return read;
    }
    let mut seen = BTreeMap::<([u8; 16], u64), usize>::new();
    for op in ops {
        let mut signer = None;
        for change in op
            .changes
            .iter()
            .filter(|c| c.collection == collection.name)
        {
            let parsed = change.key.split_once('.').and_then(|(uuid, text)| {
                if text.is_empty()
                    || (text.len() > 1 && text.starts_with('0'))
                    || !text.bytes().all(|b| b.is_ascii_digit())
                {
                    return None;
                }
                let n = text.parse::<u64>().ok()?;
                Some((keys::parse_uuid32(uuid)?, n))
            });
            let signer = *signer.get_or_insert_with(|| resolve(&op.roster, &op.actor));
            let (Some((machine, n)), Some(value)) = (
                parsed.filter(|(machine, _)| Some(*machine) == signer),
                change.value.as_ref().filter(|v| bounded(collection, v)),
            ) else {
                read.inert += 1;
                continue;
            };
            let entry = LogEntry {
                machine,
                actor: op.actor,
                n,
                sequence: op.sequence,
                period: op.period,
                value: value.clone(),
                operation: op.id,
            };
            // A writer's sequence starts again in each period table, so the period ranks first.
            match seen.get(&(machine, n)).copied() {
                Some(at)
                    if (read.entries[at].period, read.entries[at].sequence)
                        <= (op.period, op.sequence) =>
                {
                    read.inert += 1;
                }
                Some(at) => {
                    read.entries[at] = entry;
                    read.inert += 1;
                }
                None => {
                    seen.insert((machine, n), read.entries.len());
                    read.entries.push(entry);
                }
            }
        }
    }
    read.entries.sort_unstable_by_key(|e| (e.machine, e.n));
    read
}

/// A collection's stored schema row, as generic tools see it.
#[derive(Clone, Debug, PartialEq)]
pub enum SchemaState {
    /// No schema row is visible.
    Absent,
    /// The highest visible version and its row.
    One { version: u32, row: Value },
    /// Two different rows claim the highest version: generic editing of the collection stops
    /// until the owner resolves it; compiled code is unaffected.
    Split { version: u32 },
}

/// The schema row of `collection`: **the maximum version over every eligible operation**, not
/// the current cell, since under per-cell replacement an older build's write after a newer row
/// would otherwise replace it (design §3.5).
pub fn schema_state(ops: &[Op], collection: &str) -> SchemaState {
    let mut best: Option<(u32, Vec<Value>)> = None;
    for op in ops {
        for change in &op.changes {
            if change.collection != SCHEMA || change.key != collection {
                continue;
            }
            let Some((version, row)) = change.value.as_ref().and_then(|value| {
                let version = u32::try_from(value.get("r")?.as_u64()?).ok()?;
                Some((version, value.clone()))
            }) else {
                continue;
            };
            match best.as_mut() {
                Some((held, rows)) if *held == version => {
                    if !rows.contains(&row) {
                        rows.push(row);
                    }
                }
                Some((held, _)) if *held > version => {}
                _ => best = Some((version, vec![row])),
            }
        }
    }
    match best {
        None => SchemaState::Absent,
        Some((version, mut rows)) if rows.len() == 1 => SchemaState::One {
            version,
            row: rows.remove(0),
        },
        Some((version, _)) => SchemaState::Split { version },
    }
}

/// Whether a build whose compiled collection is at `ours` may publish its schema row: never
/// below a version already visible.
pub fn may_publish_schema(state: &SchemaState, ours: u32) -> bool {
    match state {
        SchemaState::Absent => true,
        SchemaState::One { version, .. } | SchemaState::Split { version } => ours >= *version,
    }
}

/// Whether `op` is an automatic record in `table`: it writes an owned or log collection, or
/// a cell marked automatic (a birth row, a seed). Owner records never are, and a revert is
/// the owner's verb even where it restores a value written automatically, so it never spends
/// the reverting Mac's quota (or lands past it, inert while `revert` reports success).
pub fn automatic(table: &Table, op: &Op) -> bool {
    !op.revert
        && op.changes.iter().any(|change| {
            table
                .collection(&change.collection)
                .is_some_and(|collection| collection.kind != Kind::Cells)
                || change
                    .value
                    .as_ref()
                    .and_then(|value| value.get("a"))
                    .is_some_and(|a| *a == Value::Bool(true))
        })
}

/// Each writer's automatic records and the ones past the quota.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quota {
    /// Records each writer's automatic records cost, the inert ones and the merges each needed
    /// included.
    pub spent: BTreeMap<Actor, usize>,
    /// Automatic records past their writer's quota, in its sequence order: inert to every
    /// reader, so one Mac cannot spend the owner's records (design §3.7).
    pub over: BTreeSet<Digest>,
}

/// Count each writer's automatic records in sequence order against `table.self_quota`. Each
/// costs one record plus the causal-only `merges` (writer, sequence) its writer appended just
/// before it to join a wide frontier, so merges cannot carry a writer's automatic writes past
/// the records the quota leaves the owner (design §3.9); a record whose cost passes the quota
/// is inert.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Merge",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Automatic",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Owner",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Revert",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
pub fn quota(table: &Table, ops: &[Op], merges: &[(Actor, u64)]) -> Quota {
    let mut business = BTreeMap::<Actor, BTreeSet<u64>>::new();
    for op in ops {
        business.entry(op.actor).or_default().insert(op.sequence);
    }
    let mut joins = BTreeMap::<Actor, BTreeSet<u64>>::new();
    for (actor, sequence) in merges {
        joins.entry(*actor).or_default().insert(*sequence);
    }
    let mut by_writer = BTreeMap::<Actor, Vec<(u64, Digest)>>::new();
    for op in ops.iter().filter(|op| automatic(table, op)) {
        by_writer
            .entry(op.actor)
            .or_default()
            .push((op.sequence, op.id));
    }
    let mut quota = Quota::default();
    for (actor, mut records) in by_writer {
        records.sort_unstable();
        let mut spent = 0;
        for (sequence, id) in records {
            // The merges between this writer's previous business record and this one.
            let after = business[&actor]
                .range(..sequence)
                .next_back()
                .map_or(0, |previous| previous + 1);
            let merged = joins
                .get(&actor)
                .map_or(0, |joins| joins.range(after..sequence).count());
            spent += 1 + merged;
            if spent > table.self_quota {
                quota.over.insert(id);
            }
        }
        quota.spent.insert(actor, spent);
    }
    quota
}

/// How many of `entries`, taken in priority order (the caller sorts them, most important
/// first), fit in `cap` bytes once `wrap` encodes them as a row: the longest prefix that fits.
/// The rest are dropped by that priority, never at random (design §4.17).
///
/// # Errors
/// Not even an empty row fits.
pub fn fit(
    entries: &[Value],
    cap: usize,
    wrap: impl Fn(&[Value]) -> Value,
) -> crate::Result<usize> {
    let size = |n: usize| aterm_json::to_vec(&wrap(&entries[..n])).map_or(usize::MAX, |b| b.len());
    if entries.is_empty() {
        if size(0) > cap {
            return Err(crate::Error::new("an empty row passes its cap"));
        }
        return Ok(0);
    }
    if size(entries.len()) <= cap {
        return Ok(entries.len());
    }
    if size(0) > cap {
        return Err(crate::Error::new("an empty row passes its cap"));
    }
    let (mut fits, mut fails) = (0, entries.len());
    // Encoded size grows with the prefix, so the boundary is found by bisection.
    while fails - fits > 1 {
        let mid = fits + (fails - fits) / 2;
        if size(mid) <= cap {
            fits = mid;
        } else {
            fails = mid;
        }
    }
    Ok(fits)
}

/// Row caps of the journal ring's collections, in canonical bytes (design §4.17).
pub mod caps {
    /// An `ev` batch.
    pub const EV_BATCH: usize = 24 * 1024;
    /// One event inside a batch.
    pub const EV_EVENT: usize = 192;
    /// A per-account `use` row.
    pub const USE_ROW: usize = 1536;
    /// A presence `sum` row.
    pub const SUM_ROW: usize = 2048;
    /// A `relay` row for one bucket of accounts.
    pub const RELAY_ROW: usize = 12 * 1024;
    /// An allocator `place` row.
    pub const PLACE_ROW: usize = 512;
    /// One flush record's payload.
    pub const RECORD: usize = 60 * 1024;
    /// Buckets accounts fall into for relaying.
    pub const RELAY_BUCKETS: u8 = 16;
}

/// The relay bucket of an account or sub-account key: the first nibble of its SHA-256.
pub fn relay_bucket(pk: &str) -> u8 {
    aterm_digest::Sha256::digest(pk.as_bytes())[0] >> 4
}

/// The two buckets a Mac relays by default: `h(uuid) mod 16` and the one eight past it. It also
/// relays any bucket no other Mac covers in its window tables (step 8).
pub fn relay_duty(machine: &[u8; 16]) -> [u8; 2] {
    let first = aterm_digest::Sha256::digest(machine)[0] % caps::RELAY_BUCKETS;
    [first, (first + 8) % caps::RELAY_BUCKETS]
}

#[cfg(unix)]
pub(crate) mod commons {
    use super::Op;
    use crate::{database::Command, documents::log::Operation, table::Session};
    use astream_wire::sync::{Actor, Digest};

    impl Op {
        /// The typed reader's view of one eligible operation read from a table of `period`.
        pub(crate) fn from_eligible(
            period: u64,
            operation: &Operation,
            command: &Command,
        ) -> Option<Self> {
            Some(Self {
                id: operation.header.id,
                actor: operation.header.actor,
                sequence: operation.header.sequence,
                roster: operation.roster,
                period,
                revert: matches!(command, Command::Revert { .. }),
                changes: command.changes()?.to_vec(),
            })
        }

        /// [`Self::from_eligible`] moving `changes` out of `command`.
        pub(crate) fn from_eligible_owned(
            period: u64,
            operation: Operation,
            command: Command,
        ) -> Option<Self> {
            let revert = matches!(command, Command::Revert { .. });
            Some(Self {
                id: operation.header.id,
                actor: operation.header.actor,
                sequence: operation.header.sequence,
                roster: operation.roster,
                period,
                revert,
                changes: command.into_changes()?,
            })
        }
    }

    /// The production resolver: a record's writer machine from the binding in the roster the
    /// record names, so a Mac removed since still owns its rows.
    pub(crate) fn historical(
        session: &Session<'_>,
    ) -> impl Fn(&Digest, &Actor) -> Option<[u8; 16]> {
        let cache = std::cell::RefCell::new(std::collections::BTreeMap::new());
        move |roster, actor| {
            let key = (*roster, *actor);
            if let Some(&cached) = cache.borrow().get(&key) {
                return cached;
            }
            let resolved = session
                .historical_binding(roster, actor)
                .map(|binding| *binding.machine());
            cache.borrow_mut().insert(key, resolved);
            resolved
        }
    }
}
