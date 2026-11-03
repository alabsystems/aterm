// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rolling the registry to its next generation (docs/DESIGN-alab-lab-model-2026-11-01.md §3.8,
//! §3.9, §11 step 13): `aterm db alab roll [--apply]`, which `check` offers once a writer's quota
//! is spent or the table holds 75 % of the store's records or bytes.
//!
//! A roll is an owner verb, run on one Mac:
//!
//! 1. **Restate.** Every register of the old generation goes into the new one as one carried
//!    value (`typed::cell::carried`), listing each candidate it holds there with the operation
//!    that wrote it, so an open conflict stays open and the provisional value reads the same.
//!    A collection or column this build does not know is carried as it is. The schema rows are
//!    the compiled ones, published into the new generation; the old generation's cover rows are
//!    not carried. Past-quota automatic records stay inert: they never project, so nothing of
//!    them is restated, and the new generation gives every writer its whole quota again.
//! 2. **Archive.** Each owned row a reader projects (machine facts, mailbox observations) is
//!    carried as the signed record that wrote it (`registry::ARCHIVED`), still sealed under the
//!    old table's scope, so its signer and operation digest are its own: a reader authenticates
//!    it and applies the owned rule exactly as before (`att::archived_ops`). A Mac's own row in
//!    the new generation outranks its archived one.
//! 3. **Cover.** One `roll` row per (generation, Mac) says which records of each writer this
//!    restatement carried: for each writer, its last record before the first one this build
//!    cannot read. A record that only projects nothing (a merge, a past-quota record, a cut-off
//!    writer's) counts as carried.
//!    A cover counts only as the Mac its key names wrote it (`covers`).
//! 4. **Seal** the old generation (`catalog::Directory::seal`): readers move to the new one. The
//!    seal waits until every current Writer has declared the new generation in its own name
//!    (`endorse`, `catalog::Directory::undeclared`), which only a build that compiles it does,
//!    since an older build cannot read the registry once the old generation is sealed; the
//!    roll reports the Macs it waits for, and running it again once they have seals.
//! 5. **Release** this Mac's storage of the old generation where the covers carry everything
//!    it holds there (`catalog::release`).
//!
//! Every write is a whole register the old generation stated, written by request id derived
//! from its content, so a re-run writes nothing and a retry returns its first receipt. A record
//! that reached a Mac late (a Mac on the other side of a split edited the old generation before
//! it heard the seal) is carried by the roll run again on that Mac: it writes only registers
//! holding a candidate no roll carried yet (`late`), merged with what the new generation holds
//! now, less the candidates this Mac's old history shows were superseded. An owner's edit made
//! in the new generation meanwhile stays, beside the late value, as a conflict for the owner,
//! since neither saw the other. Until that Mac's roll runs, its storage of the old generation
//! stays.

use super::{
    registry::{APPLICATION, ARCHIVED, ROLL},
    store::Registry,
};
use crate::{
    Error, Result,
    att::{self, Compiled, View, decode, pack},
    catalog::{
        Directory,
        release::{Released, Retired},
    },
    database::{Change, Command},
    documents::{application, log::Log},
    table::{Session, store::Store},
    typed::{
        self,
        cell::{self, CellKey, CellValue, Cells, Projection},
        keys, owned,
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_json::{Value, json};
use aterm_secrets::commons::bootstrap::Bootstrap;
use std::collections::{BTreeMap, BTreeSet};

/// The collections a roll does not restate as registers: the schema rows (the new generation
/// takes the compiled ones) and the old generation's own cover rows.
const NOT_RESTATED: [&str; 2] = [typed::SCHEMA, ROLL.name];

/// Every operation a carried value in `ops` names: what some roll already carried into the
/// table, whether or not a later edit has replaced it since.
pub(crate) fn carried_origins<'a>(ops: impl IntoIterator<Item = &'a Command>) -> BTreeSet<Digest> {
    ops.into_iter()
        .filter_map(Command::changes)
        .flatten()
        .filter_map(|change| change.value.as_ref().and_then(cell::carried))
        .flat_map(|items| items.into_iter().map(|(operation, _)| operation))
        .collect()
}

/// The registers to write into the new generation (`target`, whose rolls already carried
/// `carried`) from the old one's `source` cells, `held` being every operation this Mac holds of
/// the old one. A register is written only where it holds a candidate no roll carried yet; it
/// then holds that candidate and every other the old generation holds, and whatever the new one
/// holds that the old history here does not show superseded. One change each. A collection
/// `table` names as owned or log rows is no register: its rows go as archived records.
///
/// # Errors
/// A register would hold more candidates than a carried value lists, or a key breaks a rule.
pub(crate) fn restate(
    table: &typed::Table,
    source: &Cells,
    held: &BTreeSet<Digest>,
    target: &Cells,
    carried: &BTreeSet<Digest>,
) -> Result<Vec<Vec<Change>>> {
    let mut rows = Vec::new();
    for ((collection, key), cell) in source {
        if NOT_RESTATED.contains(&collection.as_str())
            || cell.stopped
            || table
                .collection(collection)
                .is_some_and(|known| known.kind != typed::Kind::Cells)
        {
            continue;
        }
        let old: BTreeMap<Digest, Value> = cell::origins(cell).into_iter().collect();
        if old.keys().all(|origin| carried.contains(origin)) {
            continue;
        }
        let now: BTreeMap<Digest, Value> = target
            .get(&(collection.clone(), key.clone()))
            .map(cell::origins)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut merged: BTreeMap<Digest, Value> = now
            .iter()
            .filter(|(origin, _)| !held.contains(*origin) || old.contains_key(*origin))
            .map(|(origin, value)| (*origin, value.clone()))
            .collect();
        merged.extend(old);
        if merged == now {
            continue;
        }
        let value = cell::carry(&merged).ok_or_else(|| {
            Error::new(&format!(
                "{collection}: a register holds more candidates than a roll carries; resolve it \
                 first"
            ))
        })?;
        rows.push(vec![Change {
            collection: collection.clone(),
            key: key.clone(),
            value: Some(value),
        }]);
    }
    Ok(rows)
}

/// One operation of a table as the covers count it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) actor: [u8; 48],
    pub(crate) sequence: u64,
    /// Its payload is here and this build cannot read it, so no restatement of this build's
    /// carries it.
    pub(crate) unread: bool,
}

/// The `Held` of each operation this Mac retains of a table.
pub(crate) fn held_of(log: &Log) -> Vec<Held> {
    log.snapshot()
        .operations
        .iter()
        .map(|op| Held {
            actor: op.header.actor,
            sequence: op.header.sequence,
            unread: !op.available || (op.catalog_eligible && op.refusal.is_some()),
        })
        .collect()
}

/// For each writer, the last of its records this Mac holds.
pub(crate) fn holdings(ops: &[Held]) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for op in ops {
        let last = out.entry(keys::actor_key(&op.actor)).or_default();
        *last = (*last).max(op.sequence);
    }
    out
}

/// For each writer, the last of its records a restatement of `ops` carries: the one before its
/// first record this build cannot read, else its last. A writer's records here are a prefix of
/// its chain, since each names its predecessor, so every record up to that one is carried.
pub(crate) fn frontier(ops: &[Held]) -> BTreeMap<String, u64> {
    let mut first_unread: BTreeMap<String, u64> = BTreeMap::new();
    for op in ops.iter().filter(|op| op.unread) {
        let first = first_unread
            .entry(keys::actor_key(&op.actor))
            .or_insert(u64::MAX);
        *first = (*first).min(op.sequence);
    }
    holdings(ops)
        .into_iter()
        .map(|(actor, last)| {
            let covered = first_unread
                .get(&actor)
                .map_or(last, |first| last.min(first.saturating_sub(1)));
            (actor, covered)
        })
        .collect()
}

/// What the cover rows of `view` for generation `from` carry, merged: each writer's highest.
///
/// A cover row counts only as the Mac it is keyed by wrote it (`keys::roll_pk` of `from` and
/// the signer's own machine, `resolve` reading the binding in the roster each record names): a
/// cover is that Mac's statement of what its own restatement carried, and the storage release
/// deletes data on its word, so no other writer may speak for it. The rows are read from every
/// eligible operation rather than the cells' current values, so a later write over a cover
/// cannot hide one either; each Mac's covers only grow, so merging every one it wrote reads
/// the same as its latest. A write that is no cover of its signer's counts for nothing.
pub(crate) fn covers(
    view: &View,
    resolve: impl Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    from: u32,
) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for op in view
        .ops
        .iter()
        .filter(|op| !op.revert && !view.quota.over.contains(&op.id))
    {
        let Some(machine) = resolve(&op.roster, &op.actor) else {
            continue;
        };
        let own = keys::roll_pk(from, &keys::uuid32(&machine));
        let (mut said_from, mut covered) = (None, None);
        for change in op.changes.iter().filter(|c| c.collection == ROLL.name) {
            let Ok(key) = CellKey::parse(&change.key) else {
                continue;
            };
            let Some(value) = change
                .value
                .as_ref()
                .and_then(|value| CellValue::decode(value).ok())
            else {
                continue;
            };
            if key.pk != own || key.elem.is_some() {
                continue;
            }
            match key.col {
                "from" => said_from = value.x.as_u64(),
                "covered" => covered = Some(value.x),
                _ => {}
            }
        }
        if said_from != Some(u64::from(from)) {
            continue;
        }
        for item in covered
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(actor), Some(sequence)) = (
                item.get("a").and_then(Value::as_str),
                item.get("s").and_then(Value::as_u64),
            ) {
                let last = out.entry(actor.to_owned()).or_default();
                *last = (*last).max(sequence);
            }
        }
    }
    out
}

/// Whether every record `held` names is within `covered`.
pub(crate) fn within(held: &BTreeMap<String, u64>, covered: &BTreeMap<String, u64>) -> bool {
    held.iter()
        .all(|(actor, last)| covered.get(actor).is_some_and(|c| c >= last))
}

/// The cover row's cells for this Mac (`uuid`) restating generation `from` with `frontier`, or
/// none where the new generation's row already says it.
fn cover_row(
    target: &Projection,
    from: u32,
    uuid: &str,
    frontier: &BTreeMap<String, u64>,
    ops: usize,
    now_ms: u64,
) -> Result<Vec<Change>> {
    if frontier.len() > super::registry::MAX_COVER_WRITERS {
        return Err(Error::new(
            "the table names more writers than a cover row can; this build cannot roll it",
        ));
    }
    let pk = keys::roll_pk(from, uuid);
    let covered: Vec<Value> = frontier
        .iter()
        .map(|(actor, sequence)| json!({"a": actor, "s": sequence}))
        .collect();
    let said = target
        .rows
        .get(&pk)
        .and_then(|row| row.get("covered"))
        .cloned();
    if said == Some(Value::Array(covered.clone())) {
        return Ok(Vec::new());
    }
    let cell = |col: &str, x: Value| {
        cell::change(
            &ROLL,
            CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &CellValue::owner(ROLL.version, x),
        )
    };
    Ok(vec![
        cell("from", Value::from(u64::from(from)))?,
        cell("covered", Value::Array(covered))?,
        cell("ops", Value::from(ops as u64))?,
        cell("at", Value::from(now_ms))?,
        cell("row", Value::from("live"))?,
    ])
}

/// The archived row carrying the record `wire` of generation `from`, keyed by its operation.
fn archive_row(operation: &Digest, from: u32, wire: &[u8]) -> Result<Vec<Change>> {
    let pk = keys::archived_pk(operation);
    let cell = |col: &str, x: Value| {
        cell::change(
            &ARCHIVED,
            CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &CellValue::owner(ARCHIVED.version, x),
        )
    };
    Ok(vec![
        cell("generation", Value::from(u64::from(from)))?,
        cell(
            "wire",
            Value::String(aterm_secrets::b64::encode_padded(wire)),
        )
        .map_err(|_| Error::new("an owned record is too large for a roll to carry"))?,
        cell("row", Value::from("live"))?,
    ])
}

/// What a roll found or did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rolled {
    /// The generation restated, and the one it went into.
    pub from: u32,
    pub to: u32,
    /// Registers restated, owned records archived, and whether this Mac's cover row changed.
    pub registers: usize,
    pub archived: usize,
    pub cover: bool,
    /// Records appended (none for a plan).
    pub records: usize,
    /// Whether this roll sealed the old generation (an earlier one, or another Mac, may have).
    pub sealed: bool,
    /// The current Writers, by Commons machine, that have not declared the next generation, so
    /// have shown no build that reads it: the seal waits for them (for a plan, would wait).
    pub waiting: Vec<[u8; 16]>,
    /// What the storage release did after it.
    pub released: Released,
    /// Records this Mac holds of the old generation that this build cannot read, so no cover of
    /// its own carries them: they keep the old storage here until a build that reads them rolls.
    pub unread: usize,
}

/// The storage release's `covered` answer for a restated registry generation: whether the next
/// generation's cover rows carry every record this Mac holds of `retired`, in its primary and
/// its recovery storage alike, since the release deletes both. Holding none, it is carried;
/// holding some with no next generation here, it is not. A region whose storage directory is
/// there but cannot be observed (an unfinished enrollment) answers not carried: what it holds
/// is unknown, so it is never released unread.
///
/// # Errors
/// A table's storage cannot be read.
pub(crate) fn covered(
    session: &Session<'_>,
    directory: &Directory,
    retired: &Retired,
) -> Result<bool> {
    let trust = directory.trust();
    let chain = att::REGISTRY.chain();
    let Some(from) = chain
        .iter()
        .find(|c| c.descriptor(trust).is_ok_and(|d| d == retired.descriptor))
    else {
        return Ok(false);
    };
    let Some(to) = from.successor else {
        return Ok(false);
    };
    let scope = retired.descriptor.scope;
    let mut held: BTreeMap<String, u64> = BTreeMap::new();
    for region in [directory.primary(), directory.recovery()] {
        match region.observe_store_if_exists(session, scope)? {
            Some(old) => {
                for (actor, last) in
                    holdings(&held_of(&Log::read_with(session, &old, scope, decode)?))
                {
                    let entry = held.entry(actor).or_default();
                    *entry = (*entry).max(last);
                }
            }
            None if region.path(scope)?.try_exists()? => return Ok(false),
            None => {}
        }
    }
    if held.is_empty() {
        return Ok(true);
    }
    let next = to.descriptor(trust)?.scope;
    let Some(new) = directory.primary().observe_store_if_exists(session, next)? else {
        return Ok(false);
    };
    let view = View::read(
        to.table,
        session,
        &Log::read_with(session, &new, next, decode)?,
    );
    Ok(within(
        &held,
        &covers(&view, owned::commons::historical(session), from.generation),
    ))
}

/// Declare, as this Mac, each later registry generation this build compiles that `snapshot`
/// shows another Mac declared and this one has not (`Directory::endorse`): the evidence a roll
/// waits for before it seals the generation before it. Housekeeping on a claims write and a
/// release pass, so it never fails its caller: a Mac that cannot declare now does at a later
/// write.
pub(crate) fn endorse(
    session: &Session<'_>,
    directory: &Directory,
    snapshot: &crate::catalog::Snapshot,
) {
    let trust = directory.trust();
    for compiled in att::REGISTRY.chain().into_iter().skip(1) {
        let Ok(descriptor) = compiled.descriptor(trust) else {
            continue;
        };
        let waits = snapshot
            .entries
            .get(&descriptor.scope.digest())
            .is_some_and(|entry| {
                !entry
                    .declarations
                    .iter()
                    .any(|d| d.actor == session.actor())
            });
        if waits {
            let _ = directory.endorse(session, &descriptor);
        }
    }
}

impl Registry {
    /// Roll the registry to its next generation (module docs), or with `apply` false, say what a
    /// roll would write and write nothing. Run again on a Mac that holds records of a sealed
    /// generation no cover carries, it restates those. `now_ms` stamps the cover row; the storage
    /// release reads `now_ms` as this Mac's clock.
    ///
    /// # Errors
    /// No Commons writer; this Mac holds no registry; its history of either generation is
    /// incomplete; this build compiles no later generation; a register or record is too large to
    /// carry; or a record cannot be written.
    pub fn roll(&self, apply: bool, now_ms: u64) -> Result<Rolled> {
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(self.paths(), deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        session.writer()?;
        let trust = bootstrap.trust();
        let directory = Directory::new(&paths, trust);
        let snapshot = directory.snapshot(&session)?;
        let chain = att::REGISTRY.chain();
        let current = att::current_in(&snapshot, trust, &chain);
        // The oldest earlier generation this Mac still stores is what is left to carry; else the
        // current one rolls.
        let mut from = current;
        for compiled in &chain {
            if compiled.generation < current.generation
                && directory
                    .path(compiled.descriptor(trust)?.scope)?
                    .try_exists()?
            {
                from = compiled;
                break;
            }
        }
        let to: &'static Compiled = from.successor.ok_or_else(|| {
            Error::new("this build compiles no registry generation after this one")
        })?;
        let fresh = from.generation == current.generation;
        let old_scope = from.descriptor(trust)?.scope;
        let old = directory
            .observe_store_if_exists(&session, old_scope)?
            .ok_or_else(|| Error::new("this Mac holds no registry to roll"))?;
        let old_log = Log::read_with(&session, &old, old_scope, decode)?;
        if !old_log.awaited(session.actor()).is_empty() {
            return Err(Error::new(
                "this Mac's history of the registry is incomplete; a roll waits for a complete scan",
            ));
        }
        let old_view = View::read(from.table, &session, &old_log);
        let held_ops = held_of(&old_log);
        let held: BTreeSet<Digest> = old_log
            .snapshot()
            .operations
            .iter()
            .map(|op| op.header.id)
            .collect();
        let frontier = frontier(&held_ops);
        let to_descriptor = to.descriptor(trust)?;
        let observed = directory.observe_store_if_exists(&session, to_descriptor.scope)?;
        let uuid = session
            .members()
            .iter()
            .find(|binding| binding.actor() == session.actor())
            .map(|binding| keys::uuid32(binding.machine()))
            .ok_or_else(|| Error::new("this Mac is not in its own Commons roster"))?;

        let plan = |new: Option<&Store>| -> Result<(Vec<Vec<Change>>, usize, usize, bool)> {
            let (target, carried, done_archive, rolls) = match new {
                Some(store) => {
                    let log = Log::read_with(&session, store, to_descriptor.scope, decode)?;
                    if !log.awaited(session.actor()).is_empty() {
                        return Err(Error::new(
                            "this Mac's history of the new registry generation is incomplete; a \
                             roll waits for a complete scan",
                        ));
                    }
                    let view = View::read(to.table, &session, &log);
                    let carried =
                        carried_origins(log.eligible().iter().map(|(_, command)| command));
                    // What is carried already is each record that authenticates, wherever
                    // it was written: a cell some other writer put at a record's key first
                    // must not keep that record from being carried.
                    let archived: BTreeSet<Digest> =
                        att::archived_ops(to.table, &session, &view.ops)
                            .into_iter()
                            .map(|op| op.id)
                            .collect();
                    (view.cells.clone(), carried, archived, view.project(&ROLL))
                }
                None => Default::default(),
            };
            let mut rows = restate(from.table, &old_view.cells, &held, &target, &carried)?;
            let registers = rows.len();
            let mut archived = 0;
            let mut operations: BTreeSet<Digest> = BTreeSet::new();
            for read in old_view.owned.values() {
                operations.extend(read.rows.values().map(|row| row.operation));
            }
            for operation in operations {
                if done_archive.contains(&operation) {
                    continue;
                }
                // A row the old generation itself read from an archived record is carried with
                // that record's archived row, a register like any other.
                let Some(wire) = old.wire(&session, &operation)? else {
                    continue;
                };
                rows.push(archive_row(&operation, from.generation, wire)?);
                archived += 1;
            }
            let cover = cover_row(
                &rolls,
                from.generation,
                &uuid,
                &frontier,
                held.len(),
                now_ms,
            )?;
            let changed = !cover.is_empty();
            if changed {
                rows.push(cover);
            }
            Ok((rows, registers, archived, changed))
        };

        // The plan runs on what this Mac holds before anything is declared, so a roll it
        // refuses (a register past the candidates a carried value holds, a cover past its
        // writers, a record too large to archive) leaves no declaration of the next generation.
        let (_, registers, archived, cover) = plan(observed.as_ref())?;
        let unread = held_ops.iter().filter(|op| op.unread).count();
        let mut rolled = Rolled {
            from: from.generation,
            to: to.generation,
            registers,
            archived,
            cover,
            unread,
            ..Rolled::default()
        };
        if !apply {
            if fresh {
                // This Mac declares the next generation when it applies.
                rolled.waiting = Directory::undeclared(&session, &snapshot, &to_descriptor);
                rolled
                    .waiting
                    .retain(|machine| keys::uuid32(machine) != uuid);
            }
            return Ok(rolled);
        }
        drop(observed);
        directory.publish(&session, &to_descriptor)?;
        directory.endorse(&session, &to_descriptor)?;
        let mut store = Store::open(
            &session,
            &directory.path(to_descriptor.scope)?,
            to_descriptor.scope,
        )?;
        let (rows, registers, archived, cover) = plan(Some(&store))?;
        (rolled.registers, rolled.archived, rolled.cover) = (registers, archived, cover);
        // The cover goes last, alone: it says the records before it are there.
        let (cover_rows, body): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|row| row.first().is_some_and(|c| c.collection == ROLL.name));
        let mut records = pack(body)?;
        records.extend(pack(cover_rows)?);
        let mut log = Log::read_with(&session, &store, to_descriptor.scope, decode)?;
        for changes in records {
            let request = typed::request_id(APPLICATION, to.table.role, &changes)?;
            let command = Command::Write { request, changes };
            if log.retry(session.actor(), &command)?.is_some() {
                continue;
            }
            application::append(&session, &mut store, &command.encode()?)?;
            rolled.records += 1;
            log = Log::read_with(&session, &store, to_descriptor.scope, decode)?;
        }
        if old_view.ops.iter().any(|op| {
            op.changes
                .iter()
                .any(|change| change.collection == typed::SCHEMA)
        }) {
            for changes in pack(att::schema_rows(
                to.table,
                &View::read(to.table, &session, &log),
            ))? {
                let request = typed::request_id(APPLICATION, to.table.role, &changes)?;
                let command = Command::Write { request, changes };
                if log.retry(session.actor(), &command)?.is_some() {
                    continue;
                }
                application::append(&session, &mut store, &command.encode()?)?;
                rolled.records += 1;
                log = Log::read_with(&session, &store, to_descriptor.scope, decode)?;
            }
        }
        drop(log);
        drop(store);
        // A build that does not compile the next generation cannot read the registry once the
        // old one is sealed (its declarations conflict there, and the next is unknown to it),
        // so the seal waits until every current Writer has declared the next generation itself
        // (`Directory::endorse`), which only a build that compiles it does.
        if fresh {
            rolled.waiting =
                Directory::undeclared(&session, &directory.snapshot(&session)?, &to_descriptor);
            if rolled.waiting.is_empty() {
                rolled.sealed = directory.seal(&session, &from.descriptor(trust)?)?;
            }
        }
        let snapshot = directory.snapshot(&session)?;
        rolled.released = directory.release_sealed(&session, &snapshot, now_ms / 1000, &covered)?;
        Ok(rolled)
    }

    /// One storage release pass on this Mac (`catalog::release`), as its claims write runs one
    /// when due, after declaring the registry generations this build compiles (`endorse`):
    /// it deletes the storage of each sealed table this build retires once that is safe, `now_ms`
    /// being this Mac's clock, and says why each other one stays.
    ///
    /// # Errors
    /// No Commons writer here, or the catalog or a table's storage cannot be read or removed.
    pub fn release(&self, now_ms: u64) -> Result<Released> {
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(self.paths(), deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        let directory = Directory::new(&paths, bootstrap.trust());
        endorse(&session, &directory, &directory.snapshot(&session)?);
        let snapshot = directory.snapshot(&session)?;
        directory.release_sealed(&session, &snapshot, now_ms / 1000, &covered)
    }
}

#[cfg(test)]
mod tests;

/// A register's candidates as the restatement reads them, for tests of [`restate`].
#[cfg(test)]
pub(crate) fn register(candidates: &[(Digest, Value)]) -> crate::database::Cell {
    crate::database::Cell {
        stopped: false,
        candidates: candidates
            .iter()
            .map(|(operation, value)| crate::database::Candidate {
                operation: *operation,
                value: value.clone(),
            })
            .collect(),
    }
}
