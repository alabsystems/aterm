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
//!    A `Secret` cell's token is sealed to its table and coordinates, so each candidate the roll
//!    carries is opened through the old generation's sealer and sealed again through the new
//!    one's (`typed::secret::Sealer`): the old token's bytes are never copied, and no plaintext
//!    leaves the roll.
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
        secret::Sealer,
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
/// `table` names as owned or log rows is no register: its rows go as archived records. A
/// `Secret` column's candidates go through `reseal` (the old generation's sealer, then the
/// new one's); a candidate the new generation already holds keeps the token it has there only
/// where that token opens there: a token an older build's roll copied as it was is sealed for
/// the old generation, so it is resealed, though its origin counts as carried.
///
/// # Errors
/// A register would hold more candidates than a carried value lists, a key breaks a rule, or a
/// `Secret` cannot be opened or sealed again.
pub(crate) fn restate(
    table: &typed::Table,
    source: &Cells,
    held: &BTreeSet<Digest>,
    target: &Cells,
    carried: &BTreeSet<Digest>,
    reseal: (&dyn Sealer, &dyn Sealer),
) -> Result<Vec<Vec<Change>>> {
    let mut rows = Vec::new();
    for (coord @ (collection, key), cell) in source {
        if NOT_RESTATED.contains(&collection.as_str())
            || cell.stopped
            || table
                .collection(collection)
                .is_some_and(|known| known.kind != typed::Kind::Cells)
        {
            continue;
        }
        let mut old: BTreeMap<Digest, Value> = cell::origins(cell).into_iter().collect();
        let secret = table.collection(collection).and_then(|known| {
            let parsed = CellKey::parse(key).ok()?;
            (parsed.elem.is_none()
                && known
                    .column(parsed.col)
                    .is_some_and(|column| matches!(column.ty, typed::Ty::Secret)))
            .then_some(parsed)
        });
        let now: BTreeMap<Digest, Value> = target
            .get(coord)
            .map(cell::origins)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let opens = |value: &Value| {
            secret
                .as_ref()
                .is_none_or(|parsed| opens_in(value, collection, parsed.pk, parsed.col, reseal.1))
        };
        if old
            .keys()
            .all(|origin| carried.contains(origin) && now.get(origin).is_none_or(opens))
        {
            continue;
        }
        if let Some(parsed) = &secret {
            for (origin, value) in &mut old {
                if let Some(there) = now.get(origin).filter(|there| opens(there)) {
                    value.clone_from(there);
                    continue;
                }
                reseal_value(value, collection, parsed.pk, parsed.col, reseal)?;
            }
        }
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

/// Whether a `Secret` candidate opens through `sealer`: an empty one (`null`, bare or typed)
/// holds nothing to open.
fn opens_in(value: &Value, collection: &str, pk: &str, column: &str, sealer: &dyn Sealer) -> bool {
    match value.get("x").unwrap_or(value) {
        Value::Null => true,
        Value::String(token) => sealer.open(collection, pk, column, token).is_ok(),
        _ => false,
    }
}

/// A `Secret` candidate's token opened through the old generation's sealer and sealed again
/// through the new one's, in place; an empty candidate (`null`, bare as a revert leaves it or
/// typed) stays.
fn reseal_value(
    value: &mut Value,
    collection: &str,
    pk: &str,
    column: &str,
    (from, to): (&dyn Sealer, &dyn Sealer),
) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let Some(object) = value.as_object_mut() else {
        return Err(Error::new("a Secret register is not a typed cell value"));
    };
    let Some(token) = object.get("x").and_then(Value::as_str) else {
        return Ok(());
    };
    let secret = from.open(collection, pk, column, token)?;
    let sealed = to.seal(collection, pk, column, &secret)?;
    object.insert("x".into(), Value::String(sealed));
    Ok(())
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
    held_of_ops(&log.snapshot().operations)
}

fn held_of_ops(operations: &[crate::documents::log::Operation]) -> Vec<Held> {
    operations
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
    let mut by_actor: BTreeMap<Actor, u64> = BTreeMap::new();
    for op in ops {
        let last = by_actor.entry(op.actor).or_default();
        *last = (*last).max(op.sequence);
    }
    by_actor
        .into_iter()
        .map(|(actor, last)| (keys::actor_key(&actor), last))
        .collect()
}

/// For each writer, the last of its records a restatement of `ops` carries: the one before its
/// first record this build cannot read, else its last. A writer's records here are a prefix of
/// its chain, since each names its predecessor, so every record up to that one is carried.
pub(crate) fn frontier(ops: &[Held]) -> BTreeMap<String, u64> {
    let mut by_actor: BTreeMap<Actor, (u64, u64)> = BTreeMap::new();
    for op in ops {
        let (last, first_unread) = by_actor.entry(op.actor).or_insert((0, u64::MAX));
        *last = (*last).max(op.sequence);
        if op.unread {
            *first_unread = (*first_unread).min(op.sequence);
        }
    }
    by_actor
        .into_iter()
        .map(|(actor, (last, first_unread))| {
            let covered = if first_unread == u64::MAX {
                last
            } else {
                last.min(first_unread.saturating_sub(1))
            };
            (keys::actor_key(&actor), covered)
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
        if !op.changes.iter().any(|c| c.collection == ROLL.name) {
            continue;
        }
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
                if let Some(last) = out.get_mut(actor) {
                    *last = (*last).max(sequence);
                } else {
                    out.insert(actor.to_owned(), sequence);
                }
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
    if target
        .rows
        .get(&pk)
        .and_then(|row| row.get("covered"))
        .and_then(Value::as_array)
        == Some(&covered)
    {
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

/// Each `Secret` register of `cells` holding a candidate whose token does not open through
/// `sealer`, as (collection, pk, column): a token sealed for another table (an older build's roll
/// copied it as it was) or under a key this Mac does not hold. An empty candidate (`null`, bare
/// or typed) opens, holding nothing; a column or collection `table` does not name as `Secret` is
/// no Secret register. Names coordinates only, never a value.
pub(crate) fn unopened(
    table: &typed::Table,
    cells: &Cells,
    sealer: &dyn Sealer,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for ((collection, key), cell) in cells {
        let Some(known) = table.collection(collection) else {
            continue;
        };
        let Ok(parsed) = CellKey::parse(key) else {
            continue;
        };
        if parsed.elem.is_some()
            || !known
                .column(parsed.col)
                .is_some_and(|column| matches!(column.ty, typed::Ty::Secret))
        {
            continue;
        }
        if cell::origins(cell)
            .iter()
            .any(|(_, value)| !opens_in(value, collection, parsed.pk, parsed.col, sealer))
        {
            out.push((
                collection.clone(),
                parsed.pk.to_owned(),
                parsed.col.to_owned(),
            ));
        }
    }
    out
}

/// What this Mac can say of a later registry generation another Mac declared, before it
/// declares it too (`endorse`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Judged {
    /// Its store is complete here, holds a roller's cover for the generation before it, and
    /// every `Secret` candidate opens under its key.
    Opens,
    /// It cannot be judged yet: this Mac holds no store of it, its history here is incomplete, no
    /// cover of the generation before it is there (a roller writes its cover last, so it says the
    /// restated records before it are there), or its key is unavailable. A later housekeeping
    /// pass judges it again.
    Unjudged,
    /// This many `Secret` registers hold a token that does not open under its key ([`unopened`]):
    /// a roll by a build that copied tokens as they were (v0.154.0). Endorsing it would let that
    /// roll seal and strand every such cell, so this Mac does not.
    Unopened(usize),
}

/// Judge the registry generation `compiled` (whose predecessor in the chain is `previous`) as
/// this Mac stores it. Fails closed: whatever cannot be read is [`Judged::Unjudged`].
fn judge(
    session: &Session<'_>,
    directory: &Directory,
    previous: &Compiled,
    compiled: &Compiled,
) -> Judged {
    let trust = directory.trust();
    let judged = || -> Result<Judged> {
        let scope = compiled.descriptor(trust)?.scope;
        let Some(store) = directory.observe_store_if_exists(session, scope)? else {
            return Ok(Judged::Unjudged);
        };
        let log = Log::read_with(session, &store, scope, decode)?;
        if !log.awaited(session.actor()).is_empty() {
            return Ok(Judged::Unjudged);
        }
        let view = View::read(compiled.table, session, &log);
        if covers(
            &view,
            owned::commons::historical(session),
            previous.generation,
        )
        .is_empty()
        {
            return Ok(Judged::Unjudged);
        }
        let name = compiled.name();
        let sealer = att::sealer(&name, session, scope)?;
        Ok(match unopened(compiled.table, &view.cells, &sealer).len() {
            0 => Judged::Opens,
            n => Judged::Unopened(n),
        })
    };
    judged().unwrap_or(Judged::Unjudged)
}

/// Each later registry generation this build compiles that `snapshot` names, with its predecessor
/// and descriptor, judged here ([`judge`]), and whether this Mac has declared it.
fn later_generations(
    session: &Session<'_>,
    directory: &Directory,
    snapshot: &crate::catalog::Snapshot,
) -> Vec<(&'static Compiled, crate::catalog::Descriptor, bool, Judged)> {
    let trust = directory.trust();
    let chain = att::REGISTRY.chain();
    let mut out = Vec::new();
    for pair in chain.windows(2) {
        let [previous, compiled] = pair else {
            continue;
        };
        let Ok(descriptor) = compiled.descriptor(trust) else {
            continue;
        };
        let Some(entry) = snapshot.entries.get(&descriptor.scope.digest()) else {
            continue;
        };
        let declared = entry
            .declarations
            .iter()
            .any(|d| d.actor == session.actor());
        out.push((
            *compiled,
            descriptor,
            declared,
            judge(session, directory, previous, compiled),
        ));
    }
    out
}

/// Declare, as this Mac, each later registry generation this build compiles that `snapshot`
/// shows another Mac declared and this one has not (`Directory::endorse`): the evidence a roll
/// waits for before it seals the generation before it. Housekeeping on a claims write and a
/// release pass, so it never fails its caller: a Mac that cannot declare now does at a later
/// write.
///
/// A generation is declared only once it is judged to open (`judge`, design one-mesh §4.8, §11
/// step A6): a roll by a build that copied a `Secret` cell's token as it was (v0.154.0, whose
/// restatement carried an unknown column as it is) leaves a token sealed to the old table's
/// name, which never opens in the new one, so this Mac refuses to declare it and the roll's seal
/// keeps waiting for it, reporting it among the Macs it waits for. Refusing changes nothing
/// here; a roll by this build reseals every such token (`restate`), and the next pass declares
/// the generation. A generation this Mac cannot judge yet (no store, an incomplete history, no
/// roller's cover) waits for a later pass. The roller's own roll declares its target directly,
/// since its own restatement repairs the generation.
pub(crate) fn endorse(
    session: &Session<'_>,
    directory: &Directory,
    snapshot: &crate::catalog::Snapshot,
) -> bool {
    let trust = directory.trust();
    let chain = att::REGISTRY.chain();
    let mut endorsed = false;
    for pair in chain.windows(2) {
        let [previous, compiled] = pair else {
            continue;
        };
        let Ok(descriptor) = compiled.descriptor(trust) else {
            continue;
        };
        let Some(entry) = snapshot.entries.get(&descriptor.scope.digest()) else {
            continue;
        };
        if entry
            .declarations
            .iter()
            .any(|d| d.actor == session.actor())
        {
            continue;
        }
        if judge(session, directory, previous, compiled) == Judged::Opens
            && directory.endorse(session, &descriptor).is_ok()
        {
            endorsed = true;
        }
    }
    endorsed
}

/// A later registry generation whose `Secret` cells do not all open under its key, as `aterm db
/// alab check` names it: by generation and table digest, never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unopened {
    pub generation: u32,
    /// Its descriptor's scope digest.
    pub table: Digest,
    /// The `Secret` registers holding a token that does not open.
    pub cells: usize,
    /// Whether this Mac declared it anyway (before this build refused such a generation).
    pub declared: bool,
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
        let old_snapshot = old_log.snapshot();
        let held_ops = held_of_ops(&old_snapshot.operations);
        let held: BTreeSet<Digest> = old_snapshot
            .operations
            .iter()
            .map(|op| op.header.id)
            .collect();
        let frontier = frontier(&held_ops);
        let to_descriptor = to.descriptor(trust)?;
        let observed = directory.observe_store_if_exists(&session, to_descriptor.scope)?;
        let uuid = session
            .members()?
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
                    let carried = carried_origins(log.eligible_iter().map(|(_, command)| command));
                    // What is carried already is each record that authenticates, wherever
                    // it was written: a cell some other writer put at a record's key first
                    // must not keep that record from being carried.
                    let archived: BTreeSet<Digest> =
                        att::archived_ops(to.table, &session, &view.ops)
                            .into_iter()
                            .map(|op| op.id)
                            .collect();
                    let rolls = view.project(&ROLL);
                    (view.cells, carried, archived, rolls)
                }
                None => Default::default(),
            };
            let from_name = from.name();
            let to_name = to.name();
            let old_sealer = att::sealer(&from_name, &session, old_scope)?;
            let new_sealer = att::sealer(&to_name, &session, to_descriptor.scope)?;
            let mut rows = restate(
                from.table,
                &old_view.cells,
                &held,
                &target,
                &carried,
                (&old_sealer, &new_sealer),
            )?;
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
                let Some(wire) = old.wire_unchecked(&operation) else {
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
                rolled.waiting = Directory::undeclared(&session, &snapshot, &to_descriptor)?;
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
        let mut appended = BTreeMap::new();
        for changes in records {
            let request = typed::request_id(APPLICATION, to.table.role, &changes)?;
            let command = Command::Write { request, changes };
            if att::retry_or_appended(&log, session.actor(), &command, &appended)?.is_some() {
                continue;
            }
            let receipt = application::append(&session, &mut store, &command.encode()?)?;
            appended.insert(request, (receipt.id, command));
            rolled.records += 1;
        }
        if old_view.ops.iter().any(|op| {
            op.changes
                .iter()
                .any(|change| change.collection == typed::SCHEMA)
        }) {
            if rolled.records > 0 {
                log = Log::read_with(&session, &store, to_descriptor.scope, decode)?;
                appended.clear();
            }
            for changes in pack(att::schema_rows(
                to.table,
                &View::read(to.table, &session, &log),
            ))? {
                let request = typed::request_id(APPLICATION, to.table.role, &changes)?;
                let command = Command::Write { request, changes };
                if att::retry_or_appended(&log, session.actor(), &command, &appended)?.is_some() {
                    continue;
                }
                let receipt = application::append(&session, &mut store, &command.encode()?)?;
                appended.insert(request, (receipt.id, command));
                rolled.records += 1;
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
                Directory::undeclared(&session, &directory.snapshot(&session)?, &to_descriptor)?;
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
        let mut snapshot = directory.snapshot(&session)?;
        if endorse(&session, &directory, &snapshot) {
            snapshot = directory.snapshot(&session)?;
        }
        directory.release_sealed(&session, &snapshot, now_ms / 1000, &covered)
    }

    /// Each later registry generation the catalog names whose `Secret` cells do not all open
    /// under its key (`Judged::Unopened`), which `endorse` refuses to declare: what `aterm db alab
    /// check` names. A generation this Mac cannot judge yet is not listed. Reads only; declares
    /// nothing.
    ///
    /// # Errors
    /// No Commons here, or the catalog cannot be read.
    pub fn unopened(&self) -> Result<Vec<Unopened>> {
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(self.paths(), deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        let directory = Directory::new(&paths, bootstrap.trust());
        let snapshot = directory.snapshot(&session)?;
        Ok(later_generations(&session, &directory, &snapshot)
            .into_iter()
            .filter_map(|(compiled, descriptor, declared, judged)| match judged {
                Judged::Unopened(cells) => Some(Unopened {
                    generation: compiled.generation,
                    table: descriptor.scope.digest(),
                    cells,
                    declared,
                }),
                Judged::Opens | Judged::Unjudged => None,
            })
            .collect())
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
