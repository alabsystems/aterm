// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a typed table's read folds from its records, part by part, in a form a fold can start
//! from (docs/DESIGN-table-checkpoint-2026-11-15.md §5, "The state at F serves every registry
//! reader", tracker LT-B10D2): the state a checkpoint at a cut holds beside its claims
//! ([`super::claim::Floors`], format 3).
//!
//! Every part is a fold: [`State::fold`] of the records above a cut, started from the state at
//! the cut, gives the state [`State::fold`] of every record gives, because each part takes its
//! records in an order no arrival order changes. Owned rows and log entries rank by their rules'
//! total orders, ties by the smallest digest (tracker LT-B10D1); the quota charges each writer's
//! records in its own sequence order, and every record of a writer above a cut follows every one
//! at or below it, so a writer's [`Counter`] at the cut is all the quota of the records above it
//! needs; the rest are maxima, first writes and counts. The parts are what a compiled family
//! declares (its owned rows, carried rows and log collections, its quota and schema rows), and
//! what its readers ask of every record (the first write to a key, the records touching a
//! collection, the writers with refused history), never one table's own code.

use super::owned::{self, Counter, LogRead, Op, OwnedRead, SchemaState};
use super::{Collection, Kind, SCHEMA, Table};
use astream_wire::sync::{Actor, Digest};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A typed table's state made by a set of records.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// Each owned collection's rows this generation's records wrote, past-quota records left
    /// out ([`owned::read_owned`]).
    pub owned: BTreeMap<&'static str, OwnedRead>,
    /// Each owned collection's rows of the records a roll carried into the `archived` rows
    /// ([`crate::att`]'s carried records), each carried by a record within its writer's quota.
    /// A row of `owned` outranks the carried row at its key, whatever their ranks.
    pub carried: BTreeMap<&'static str, OwnedRead>,
    /// The carried records the carried rows were read from.
    pub carried_ids: BTreeSet<Digest>,
    /// Each log collection's entries, past-quota records left out ([`owned::read_log`]).
    pub logs: BTreeMap<&'static str, LogRead>,
    /// Each writer's quota, counted through its newest record ([`owned::charge`]). A state at a
    /// cut holds it only for a table that declares a quota ([`Self::settled`]).
    pub quota: BTreeMap<Actor, Counter>,
    /// Each schema row's key (a collection's name) with its highest version written and the
    /// distinct rows written at it, in their canonical bytes' order ([`SchemaState`]).
    pub schema: BTreeMap<String, (u32, Vec<Value>)>,
    /// Whether any record wrote the schema collection.
    pub schema_written: bool,
    /// Each writer's first write within its quota to each key of an owned collection, by
    /// (writer, collection, key): the record, its sequence and the row it wrote.
    pub first: BTreeMap<(Actor, String, String), First>,
    /// How many records each writer wrote to each collection, reverts left out and past-quota
    /// records counted.
    pub touched: BTreeMap<(Actor, String), u64>,
    /// The writers with withheld, unavailable or refused history (`Log::unwritable`).
    pub refused: BTreeSet<Actor>,
}

/// A writer's first write to one owned key ([`State::first`]).
#[derive(Clone, Debug, PartialEq)]
pub struct First {
    /// The record.
    pub operation: Digest,
    /// Its sequence.
    pub sequence: u64,
    /// The row it wrote there.
    pub row: Value,
}

impl State {
    /// `base` (a state at a cut, or none for the empty one) with `ops` folded in: the business
    /// records above the cut (every record for a whole read), with `merges`, the causal-only
    /// merges among them as (writer, sequence), `carried`, every record a roll carried that
    /// authenticates with the records that carried it, `refused`, the writers whose records
    /// among them are refused (`Log::unwritable`), and `resolve`, a record's writer machine
    /// through the roster it names. Each writer's records in `ops` and `merges` follow every one
    /// `base` counted. The records past their writer's quota come back beside the state: a
    /// reader of the records above leaves them out.
    pub fn fold<'a>(
        table: &'static Table,
        base: Option<&Self>,
        ops: &[Op],
        merges: &[(Actor, u64)],
        carried: impl Iterator<Item = (&'a Op, &'a BTreeSet<Digest>)>,
        refused: BTreeSet<Actor>,
        resolve: impl Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    ) -> (Self, BTreeSet<Digest>) {
        let mut state = base.cloned().unwrap_or_default();
        let mut over = BTreeSet::new();
        let events = owned::writer_events(
            ops.iter()
                .map(|op| (op.actor, op.sequence, op.id, owned::automatic(table, op))),
            merges,
        );
        for (actor, events) in events {
            owned::charge(
                table,
                state.quota.entry(actor).or_default(),
                events,
                &mut over,
            );
        }
        let within: Vec<&Op> = ops.iter().filter(|op| !over.contains(&op.id)).collect();
        let owned_collections = || {
            table
                .collections
                .iter()
                .filter(|collection| collection.kind == Kind::Owned)
        };
        for collection in owned_collections() {
            let read = owned::read_owned(collection, within.iter().copied(), &resolve);
            merge_owned(state.owned.entry(collection.name).or_default(), read);
        }
        for collection in table
            .collections
            .iter()
            .filter(|collection| collection.kind == Kind::Log)
        {
            let read = owned::read_log(collection, within.iter().copied(), &resolve);
            merge_log(state.logs.entry(collection.name).or_default(), read);
        }
        // A carried record stands where a record within its writer's quota carried it; one the
        // base holds stands already.
        let carriers: BTreeSet<Digest> = within.iter().map(|op| op.id).collect();
        let fresh: Vec<Op> = carried
            .filter(|(op, by)| {
                !state.carried_ids.contains(&op.id) && by.iter().any(|id| carriers.contains(id))
            })
            .map(|(op, _)| op.clone())
            .collect();
        state.carried_ids.extend(fresh.iter().map(|op| op.id));
        for collection in owned_collections() {
            let read = owned::read_owned(collection, &fresh, &resolve);
            merge_owned(state.carried.entry(collection.name).or_default(), read);
        }
        for op in ops {
            fold_schema(&mut state, op);
            if op.revert.is_none() {
                let collections: BTreeSet<&str> = op
                    .changes
                    .iter()
                    .map(|change| change.collection.as_str())
                    .collect();
                for collection in collections {
                    *state
                        .touched
                        .entry((op.actor, collection.to_owned()))
                        .or_default() += 1;
                }
            }
        }
        for op in &within {
            fold_first(table, &mut state, op);
        }
        state.refused.extend(refused);
        (state, over)
    }

    /// This state as a checkpoint of `table` at a cut holds it: the quota counters only where
    /// the table declares a quota. A table with none refuses no record for its quota, so its
    /// state carries no counters, and a fold from it counts `spent` from the cut.
    #[must_use]
    pub fn settled(mut self, table: &Table) -> Self {
        if table.self_quota == usize::MAX {
            self.quota.clear();
        }
        self
    }

    /// The rows of the owned `collection`: those of this generation's records, then each
    /// carried row at a key none of them wrote.
    #[must_use]
    pub fn owned_read(&self, collection: &str) -> OwnedRead {
        let mut read = self.owned.get(collection).cloned().unwrap_or_default();
        if let Some(carried) = self.carried.get(collection) {
            read.inert += carried.inert;
            for (key, row) in &carried.rows {
                read.rows.entry(key.clone()).or_insert_with(|| row.clone());
            }
        }
        read
    }

    /// The stored schema row of `collection` ([`owned::schema_state`]).
    #[must_use]
    pub fn schema_state(&self, collection: &str) -> SchemaState {
        match self.schema.get(collection) {
            None => SchemaState::Absent,
            Some((version, rows)) if rows.len() == 1 => SchemaState::One {
                version: *version,
                row: rows[0].clone(),
            },
            Some((version, _)) => SchemaState::Split { version: *version },
        }
    }

    /// The quota's records each writer's automatic records cost ([`owned::Quota::spent`]).
    #[must_use]
    pub fn spent(&self) -> BTreeMap<Actor, usize> {
        self.quota
            .iter()
            .filter_map(|(actor, counter)| Some((*actor, counter.spent?)))
            .collect()
    }

    /// Feed this state, in its one encoding, to `hash`: every part in its sorted order, every
    /// length explicit, each value as canonical JSON.
    ///
    /// # Errors
    /// A value does not encode.
    pub fn digest_into(&self, hash: &mut aterm_digest::Sha256) -> crate::Result<()> {
        let Self {
            owned,
            carried,
            carried_ids,
            logs,
            quota,
            schema,
            schema_written,
            first,
            touched,
            refused,
        } = self;
        for rows in [owned, carried] {
            put_len(hash, rows.len());
            for (collection, read) in rows {
                put_text(hash, collection);
                put_len(hash, read.inert);
                put_len(hash, read.rows.len());
                for (key, row) in &read.rows {
                    put_text(hash, key);
                    hash.update(row.machine);
                    hash.update(row.actor);
                    put_value(hash, &row.value)?;
                    for part in [row.rank.0, row.rank.1, row.rank.2] {
                        hash.update(part.to_le_bytes());
                    }
                    hash.update(row.operation);
                }
            }
        }
        put_len(hash, carried_ids.len());
        for id in carried_ids {
            hash.update(id);
        }
        put_len(hash, logs.len());
        for (collection, read) in logs {
            put_text(hash, collection);
            put_len(hash, read.inert);
            put_len(hash, read.entries.len());
            for entry in &read.entries {
                hash.update(entry.machine);
                hash.update(entry.actor);
                for part in [entry.n, entry.sequence, entry.period] {
                    hash.update(part.to_le_bytes());
                }
                put_value(hash, &entry.value)?;
                hash.update(entry.operation);
            }
        }
        put_len(hash, quota.len());
        for (actor, counter) in quota {
            hash.update(actor);
            match counter.spent {
                None => hash.update([0]),
                Some(spent) => {
                    hash.update([1]);
                    put_len(hash, spent);
                }
            }
            hash.update(counter.last_business.to_le_bytes());
            put_len(hash, counter.merges_after);
        }
        put_len(hash, schema.len());
        for (key, (version, rows)) in schema {
            put_text(hash, key);
            hash.update(version.to_le_bytes());
            let mut encoded = rows
                .iter()
                .map(|row| {
                    aterm_json::to_vec(row)
                        .map_err(|_| crate::Error::new("a schema row does not encode"))
                })
                .collect::<crate::Result<Vec<Vec<u8>>>>()?;
            encoded.sort_unstable();
            put_len(hash, encoded.len());
            for row in encoded {
                put_len(hash, row.len());
                hash.update(row);
            }
        }
        hash.update([u8::from(*schema_written)]);
        put_len(hash, first.len());
        for ((actor, collection, key), first) in first {
            hash.update(actor);
            put_text(hash, collection);
            put_text(hash, key);
            hash.update(first.operation);
            hash.update(first.sequence.to_le_bytes());
            put_value(hash, &first.row)?;
        }
        put_len(hash, touched.len());
        for ((actor, collection), count) in touched {
            hash.update(actor);
            put_text(hash, collection);
            hash.update(count.to_le_bytes());
        }
        put_len(hash, refused.len());
        for actor in refused {
            hash.update(actor);
        }
        Ok(())
    }
}

/// `into` with `read`'s rows (of records it did not read) folded in: each key keeps the higher
/// rank, an exact tie the smaller digest, as [`owned::read_owned`] keeps them.
fn merge_owned(into: &mut OwnedRead, read: OwnedRead) {
    into.inert += read.inert;
    for (key, row) in read.rows {
        let wins = into.rows.get(&key).is_none_or(|held| {
            held.rank < row.rank || (held.rank == row.rank && row.operation < held.operation)
        });
        if wins {
            into.rows.insert(key, row);
        }
    }
}

/// `into` with `read`'s entries (of records it did not read) folded in: each (machine, n) keeps
/// its first entry by (period, sequence, digest), the other inert, as [`owned::read_log`] keeps
/// them.
fn merge_log(into: &mut LogRead, read: LogRead) {
    into.inert += read.inert;
    let mut at: BTreeMap<([u8; 16], u64), usize> = into
        .entries
        .iter()
        .enumerate()
        .map(|(at, entry)| ((entry.machine, entry.n), at))
        .collect();
    for entry in read.entries {
        let rank = |e: &owned::LogEntry| (e.period, e.sequence, e.operation);
        match at.get(&(entry.machine, entry.n)).copied() {
            Some(held) => {
                into.inert += 1;
                if rank(&entry) < rank(&into.entries[held]) {
                    into.entries[held] = entry;
                }
            }
            None => {
                at.insert((entry.machine, entry.n), into.entries.len());
                into.entries.push(entry);
            }
        }
    }
    into.entries.sort_unstable_by_key(|e| (e.machine, e.n));
}

/// Fold `op`'s schema rows into `state` ([`owned::schema_state`]).
pub(crate) fn fold_schema(state: &mut State, op: &Op) {
    for change in op.changes.iter().filter(|c| c.collection == SCHEMA) {
        state.schema_written = true;
        let Some((version, row)) = change.value.as_ref().and_then(|value| {
            let version = u32::try_from(value.get("r")?.as_u64()?).ok()?;
            Some((version, value.clone()))
        }) else {
            continue;
        };
        match state.schema.get_mut(&change.key) {
            Some((held, rows)) if *held == version => {
                if !rows.contains(&row) {
                    rows.push(row);
                    // One order whatever order the records came in: their canonical bytes'.
                    rows.sort_by_cached_key(|row| aterm_json::to_vec(row).unwrap_or_default());
                }
            }
            Some((held, _)) if *held > version => {}
            _ => {
                state
                    .schema
                    .insert(change.key.clone(), (version, vec![row]));
            }
        }
    }
}

/// Fold `op`, a record within its writer's quota, into `state`'s first writes: each owned key it
/// writes whose first change in it has a value, unless its writer wrote the key earlier.
fn fold_first(table: &Table, state: &mut State, op: &Op) {
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    for change in &op.changes {
        if !seen.insert((change.collection.as_str(), change.key.as_str())) {
            continue;
        }
        let owned = table
            .collection(&change.collection)
            .is_some_and(|collection: &Collection| collection.kind == Kind::Owned);
        let Some(row) = change.value.as_ref().filter(|_| owned) else {
            continue;
        };
        let key = (op.actor, change.collection.clone(), change.key.clone());
        if state
            .first
            .get(&key)
            .is_none_or(|held| op.sequence < held.sequence)
        {
            state.first.insert(
                key,
                First {
                    operation: op.id,
                    sequence: op.sequence,
                    row: row.clone(),
                },
            );
        }
    }
}

pub(super) fn put_len(hash: &mut aterm_digest::Sha256, len: usize) {
    hash.update((len as u64).to_le_bytes());
}

pub(super) fn put_text(hash: &mut aterm_digest::Sha256, text: &str) {
    put_len(hash, text.len());
    hash.update(text.as_bytes());
}

pub(super) fn put_value(hash: &mut aterm_digest::Sha256, value: &Value) -> crate::Result<()> {
    let bytes = aterm_json::to_vec(value)
        .map_err(|_| crate::Error::new("a state value does not encode"))?;
    put_len(hash, bytes.len());
    hash.update(bytes);
    Ok(())
}
