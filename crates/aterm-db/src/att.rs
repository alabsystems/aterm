// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ATT/1 tables on this Mac's Commons (docs/DESIGN-alab-lab-model-2026-11-01.md §3, §8): the
//! store side of [`crate::typed`] and the generic `aterm db table` verbs ([`command`]).
//!
//! - [`View`]: one read of a typed table, the per-Mac quota applied: past-quota automatic
//!   records never project, owned rows count only at their signer's own key.
//! - [`Handle`]: read, apply a grid save with compare-before-write, revert one operation, and
//!   list history, for any table in [`TABLES`].
//! - [`command`]: `aterm db table ls|schema|get|scan|history|check|revert|export|apply|edit`,
//!   addresses masked unless `--show-addresses` is given, and the amacs grid round trip.

pub mod command;
mod edit;

use crate::{
    Error, Result,
    alab::registry,
    catalog::Descriptor,
    database::{Change, Command, MAX_WRITES},
    documents::{application, log::Log},
    table::Session,
    typed::{
        self, Collection, Table,
        cell::{self, Cells, Projection},
        grid::{self, Plan, Stale},
        owned::{self, Op, OwnedRead, Quota, SchemaState},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_json::Value;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::collections::BTreeMap;

pub use edit::{Edited, Opener, amacs};

/// How a table keys the row a new grid row becomes, from the typed values of its cells and the
/// table's cells as they are (an account's key needs its login's address).
pub type Birth = fn(&Collection, &BTreeMap<String, Value>, &Cells) -> Result<String>;

/// A typed catalog table this build compiles.
#[derive(Clone, Copy, Debug)]
pub struct Compiled {
    pub table: &'static Table,
    /// The table descriptor's version.
    pub version: u32,
    pub birth: Birth,
    /// Which generation of its table family this is (`registry` 1, `registry.2` 2, design §3.8).
    pub generation: u32,
    /// The next generation a roll restates this one into, when this build compiles one.
    pub successor: Option<&'static Compiled>,
}

impl Compiled {
    /// This table's catalog declaration in `trust`'s lab.
    ///
    /// # Errors
    /// The table's words break the descriptor grammar.
    pub fn descriptor(&self, trust: aterm_secrets::commons::journal::Trust) -> Result<Descriptor> {
        Descriptor::application(trust, self.table.application, self.table.role, self.version)
    }

    /// This generation and every later one this build compiles, oldest first.
    pub fn chain(&'static self) -> Vec<&'static Compiled> {
        std::iter::successors(Some(self), |compiled| compiled.successor).collect()
    }

    /// The key a new grid row in `collection` is born under ([`Birth`]), refused when the
    /// table already holds a row there: a `--where` export can leave that row out of the
    /// file, and a birth never writes into a row the owner did not see (design §8).
    ///
    /// # Errors
    /// The table's birth rule stops the key, or the table holds that row already.
    pub fn born(
        &self,
        collection: &Collection,
        values: &BTreeMap<String, Value>,
        cells: &Cells,
    ) -> Result<String> {
        let pk = (self.birth)(collection, values, cells)?;
        if grid::holds_row(collection, cells, &pk) {
            return Err(Error::new(&format!(
                "this new row is the row {pk}, which the table already holds; export without \
                 a filter and edit that row"
            )));
        }
        Ok(pk)
    }

    /// `<application>/<role>`.
    pub fn name(&self) -> String {
        format!("{}/{}", self.table.application, self.table.role)
    }

    /// The cells or owned collection named `name`.
    ///
    /// # Errors
    /// The table has no such collection.
    pub fn collection(&self, name: &str) -> Result<&'static Collection> {
        self.table
            .collection(name)
            .ok_or_else(|| Error::new(&format!("{}: no collection of that name", self.name())))
    }
}

/// `alab/registry`, the lab model's owner data, until a roll seals it ([`REGISTRY_2`]).
pub const REGISTRY: Compiled = Compiled {
    table: &registry::REGISTRY,
    version: registry::VERSION,
    birth: registry::birth,
    generation: 1,
    successor: Some(&REGISTRY_2),
};

/// `alab/registry.2`, the registry's second generation (design §3.8, step 13).
pub const REGISTRY_2: Compiled = Compiled {
    table: &registry::REGISTRY_2,
    version: registry::VERSION,
    birth: registry::birth,
    generation: 2,
    successor: None,
};

/// Every typed catalog table this build compiles.
pub const TABLES: &[Compiled] = &[REGISTRY, REGISTRY_2];

/// The generation of `chain` (oldest first) readers and writers use in `snapshot`: the oldest
/// one subscribed here. A roll declares the next generation and restates into it while the old
/// one stays subscribed, so every Mac keeps reading the old one until the roll seals it; then
/// the old one's declarations conflict and the next is the oldest subscribed. With none
/// subscribed it is the first, whose read then says why.
pub(crate) fn current_in(
    snapshot: &crate::catalog::Snapshot,
    trust: aterm_secrets::commons::journal::Trust,
    chain: &[&'static Compiled],
) -> &'static Compiled {
    chain
        .iter()
        .copied()
        .find(|compiled| {
            compiled.descriptor(trust).is_ok_and(|descriptor| {
                snapshot
                    .entries
                    .get(&descriptor.scope.digest())
                    .is_some_and(|entry| entry.state == crate::catalog::State::Subscribed)
            })
        })
        .unwrap_or(chain[0])
}

/// The generation of `first`'s family this Mac's catalog says to use ([`current_in`]), read as
/// an observer: `first` itself where it has no successor, where there is no Commons, or where
/// the catalog cannot be read (the read that follows then says why).
pub fn current(paths: &Paths, first: &'static Compiled) -> &'static Compiled {
    let chain = first.chain();
    if chain.len() == 1 {
        return first;
    }
    application::peek_catalog(paths)
        .ok()
        .flatten()
        .map_or(first, |(trust, snapshot)| {
            current_in(&snapshot, trust, &chain)
        })
}

/// The compiled table of `table`'s family at `generation`.
fn generation_of(table: &Table, generation: u64) -> Option<&'static Compiled> {
    let family = |role: &str| role.split('.').next().unwrap_or(role).to_owned();
    TABLES.iter().find(|compiled| {
        compiled.table.application == table.application
            && family(compiled.table.role) == family(table.role)
            && u64::from(compiled.generation) == generation
    })
}

/// The compiled table named `<application>/<role>`.
///
/// # Errors
/// This build compiles no such table.
pub fn find(name: &str) -> Result<&'static Compiled> {
    TABLES
        .iter()
        .find(|compiled| compiled.name() == name)
        .ok_or_else(|| {
            Error::new("no typed table of that name in this build (see `aterm db table ls`)")
        })
}

/// A typed record: a canonical `Write` or `Revert`. A write never stops a cell (clearing
/// writes `x: null`, design §3.2), so a bare absence is refused; a revert may restore one.
/// Typed values are judged by the readers, so a newer build's collection or column stays.
pub(crate) fn decode(payload: &[u8]) -> Result<Command> {
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("typed table payload is not canonical"));
    }
    match &command {
        Command::Write { changes, .. } if changes.iter().all(|c| c.value.is_some()) => Ok(command),
        Command::Revert { .. } => Ok(command),
        Command::Write { .. } => Err(Error::new("a typed table write never stops a cell")),
        _ => Err(Error::new(
            "typed table records are writes and reverts only",
        )),
    }
}

/// One read of a typed table, the quota applied.
#[derive(Clone, Debug)]
pub struct View {
    /// Every eligible operation, past-quota ones included (the schema reader and history read
    /// them).
    pub ops: Vec<Op>,
    /// Each writer's automatic records, and the ones past the table's `self_quota`, which never
    /// project.
    pub quota: Quota,
    /// The projected cells without the past-quota records.
    pub cells: Cells,
    /// The owned collections' rows, past-quota records left out, each at its signer's own key.
    pub owned: BTreeMap<&'static str, OwnedRead>,
    /// No retained operation could still become visible here (`Log::awaited`: none waits on a
    /// causal parent that has not arrived, and this Mac's own writer has nothing withheld), so
    /// a comparison against these cells misses nothing a later projection would add.
    pub complete: bool,
    /// Retained operations this build never projects and that cannot become visible: refused
    /// payloads (a newer build's encoding, a reused request id), cut-off or forked writers'
    /// records and quarantined payloads. They never hold a grid save or an import back;
    /// `check` lists them.
    pub refused: Vec<Digest>,
}

impl View {
    pub(crate) fn read(table: &Table, session: &Session<'_>, log: &Log) -> Self {
        let (eligible, merges) = log.eligible_and_merges();
        let ops: Vec<Op> = eligible
            .iter()
            .filter_map(|(operation, command)| Op::from_eligible(0, operation, command))
            .collect();
        let quota = owned::quota(table, &ops, &merges);
        let within: Vec<Op> = ops
            .iter()
            .filter(|op| !quota.over.contains(&op.id))
            .cloned()
            .collect();
        let resolve = owned::commons::historical(session);
        let cells = log.cells_without(&quota.over);
        let archived = archived_ops(table, session, &within);
        let owned = table
            .collections
            .iter()
            .filter(|collection| collection.kind == typed::Kind::Owned)
            .map(|collection| {
                let mut read = owned::read_owned(collection, &within, &resolve);
                // A row a Mac wrote in this generation outranks the one a roll carried for it.
                let carried = owned::read_owned(collection, &archived, &resolve);
                read.inert += carried.inert;
                for (key, row) in carried.rows {
                    read.rows.entry(key).or_insert(row);
                }
                (collection.name, read)
            })
            .collect();
        let awaited = log.awaited(session.actor());
        let refused = log
            .snapshot()
            .operations
            .iter()
            .filter(|op| !op.available || !op.catalog_eligible || op.refusal.is_some())
            .map(|op| op.header.id)
            .filter(|id| !awaited.contains(id))
            .collect();
        // A later generation is whole only once a roll's cover row is here: before that its
        // history is still arriving from the Macs that restated it.
        let restated = table.collection(registry::ROLL.name).is_none()
            || cells
                .keys()
                .any(|(collection, _)| collection == registry::ROLL.name);
        Self {
            cells,
            complete: awaited.is_empty() && restated,
            refused,
            ops,
            quota,
            owned,
        }
    }

    /// One cells collection's rows.
    pub fn project(&self, collection: &Collection) -> Projection {
        cell::project(collection, &self.cells)
    }

    /// The stored schema row of `collection`: the highest version any eligible operation wrote.
    pub fn schema(&self, collection: &str) -> SchemaState {
        owned::schema_state(&self.ops, collection)
    }
}

/// The records a roll carried into `table`'s `archived` rows (design §3.8), each authenticated
/// under the scope of the generation that sealed it: its signer, roster and operation digest are
/// the record's own, so its owned rows go through the owned rule exactly as they did there.
///
/// They are read from every eligible operation `ops` holds that wrote an archived row, not from
/// the cells' current values: any Writer may write any archived cell, so a later write over a
/// carried record (junk, or an older genuine record of the same row) must not hide it. Every
/// record that authenticates stands, and the owned rule ranks them as it ranked them in the
/// generation that sealed them, so an older record never outranks a newer one. A row whose
/// record does not authenticate, whose key does not name its operation, or whose generation
/// this build does not compile is left out; nothing a later writer wrote can stand for another
/// Mac's row or take one away.
pub(crate) fn archived_ops(table: &Table, session: &Session<'_>, ops: &[Op]) -> Vec<Op> {
    let Some(collection) = table.collection(registry::ARCHIVED.name) else {
        return Vec::new();
    };
    let trust = session.trust();
    let mut out: BTreeMap<Digest, Op> = BTreeMap::new();
    for op in ops.iter().filter(|op| !op.revert) {
        // One archived row's cells go in one record (`pack` never splits a row).
        let mut rows: BTreeMap<&str, (Option<u64>, Option<String>)> = BTreeMap::new();
        for change in op
            .changes
            .iter()
            .filter(|change| change.collection == collection.name)
        {
            let Ok(key) = cell::CellKey::parse(&change.key) else {
                continue;
            };
            let Some(value) = change
                .value
                .as_ref()
                .and_then(|value| cell::CellValue::decode(value).ok())
            else {
                continue;
            };
            let slot = rows.entry(key.pk).or_default();
            match (key.col, key.elem) {
                ("generation", None) => slot.0 = value.x.as_u64(),
                ("wire", None) => slot.1 = value.x.as_str().map(str::to_owned),
                _ => {}
            }
        }
        for (pk, (generation, wire)) in rows {
            let (Some(generation), Some(wire)) = (generation, wire) else {
                continue;
            };
            let Some(compiled) = generation_of(table, generation) else {
                continue;
            };
            let (Some(wire), Ok(descriptor)) = (
                aterm_secrets::b64::decode_padded(&wire),
                compiled.descriptor(trust),
            ) else {
                continue;
            };
            let Ok(record) = session.authenticate(descriptor.scope, &wire) else {
                continue;
            };
            let header = record.header();
            if typed::keys::archived_pk(&header.id) != pk || out.contains_key(&header.id) {
                continue;
            }
            let Some(changes) = decode(record.payload())
                .ok()
                .and_then(|command| command.changes().map(<[Change]>::to_vec))
            else {
                continue;
            };
            out.insert(
                header.id,
                Op {
                    id: header.id,
                    actor: header.actor,
                    sequence: header.sequence,
                    roster: *record.roster(),
                    period: 0,
                    revert: false,
                    changes: changes
                        .into_iter()
                        .filter(|change| {
                            table
                                .collection(&change.collection)
                                .is_some_and(|c| c.kind == typed::Kind::Owned)
                        })
                        .collect(),
                },
            );
        }
    }
    out.into_values().collect()
}

/// Pack rows of changes into records of at most [`MAX_WRITES`] changes and a payload within
/// the Commons body limit, never splitting a row.
///
/// # Errors
/// One row alone passes a record's limits.
pub(crate) fn pack(rows: Vec<Vec<Change>>) -> Result<Vec<Vec<Change>>> {
    let fits = |changes: &[Change]| {
        changes.len() <= MAX_WRITES
            && Command::Write {
                request: [0; 16],
                changes: changes.to_vec(),
            }
            .encode()
            .is_ok_and(|payload| payload.len() <= crate::crypto::MAX_BODY)
    };
    let mut records: Vec<Vec<Change>> = Vec::new();
    for row in rows {
        if !fits(&row) {
            return Err(Error::new("one typed row passes a record's limits"));
        }
        match records.last_mut() {
            Some(last) if fits(&[last.as_slice(), row.as_slice()].concat()) => last.extend(row),
            _ => records.push(row),
        }
    }
    Ok(records)
}

/// What a grid save wrote.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// Records appended.
    pub records: usize,
    /// Registers written.
    pub cells: usize,
    /// Edits the table already held.
    pub unchanged: usize,
    /// Cells that changed since the export: left alone.
    pub stale: Vec<Stale>,
    /// Keys of born rows written by this save.
    pub born: Vec<String>,
    /// Keys of rows retired or ended by this save.
    pub retired: Vec<String>,
    /// The operations appended.
    pub operations: Vec<Digest>,
}

/// What a table's capacity asks of the owner (design §3.9), as `check` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capacity {
    /// A writer's automatic records, with the merges they cost, reached the table's quota:
    /// its later facts, observations and births are inert until the table rolls.
    QuotaSpent { writer: Actor, spent: usize },
    /// Roll the table to its next generation (`registry.2`, step 13).
    Roll(Roll),
}

/// Why a table should roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Roll {
    /// A writer's quota is spent; only a new generation gives it room again.
    Quota,
    /// The table holds [`ROLL_AT_PERCENT`] of the store's records.
    Records,
    /// The table holds [`ROLL_AT_PERCENT`] of the store's bytes.
    Bytes,
}

/// The share of the store's records or bytes at which a table should roll.
pub const ROLL_AT_PERCENT: usize = 75;

/// What `view`'s quota and the table's `usage` ask of the owner: each writer whose quota is
/// spent, and a roll once any is, or once the table holds [`ROLL_AT_PERCENT`] of the store's
/// records or bytes. Automatic writes alone never reach 75 % (128 Macs at 24 records is about
/// a third), so a spent quota is a roll trigger of its own.
pub fn capacity(table: &Table, view: &View, usage: &Usage) -> Vec<Capacity> {
    use crate::table::store::{MAX_BYTES, MAX_RECORDS};
    let mut found: Vec<Capacity> = view
        .quota
        .spent
        .iter()
        .filter(|(_, spent)| **spent >= table.self_quota)
        .map(|(writer, spent)| Capacity::QuotaSpent {
            writer: *writer,
            spent: *spent,
        })
        .collect();
    let roll = if !found.is_empty() {
        Some(Roll::Quota)
    } else if usage.records * 100 >= MAX_RECORDS * ROLL_AT_PERCENT {
        Some(Roll::Records)
    } else if usage.bytes * 100 >= MAX_BYTES * ROLL_AT_PERCENT {
        Some(Roll::Bytes)
    } else {
        None
    };
    found.extend(roll.map(Capacity::Roll));
    found
}

/// The records and bytes a table holds here, in all and authored by this Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// This Mac's writer in the table.
    pub actor: Actor,
    pub records: usize,
    pub bytes: usize,
    pub mine_records: usize,
    pub mine_bytes: usize,
}

/// One eligible operation as history shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub operation: Digest,
    pub actor: Actor,
    /// The writer's Commons machine, from the binding in the roster the record names.
    pub machine: Option<[u8; 16]>,
    /// The writer's clock claim, never an ordering input.
    pub authored_ms: u64,
    pub request: Option<[u8; 16]>,
    pub reverts: Option<Digest>,
    pub changes: Vec<Change>,
}

/// A typed table on this Mac's Commons.
pub struct Handle {
    paths: Paths,
    compiled: &'static Compiled,
}

impl Handle {
    /// The table `compiled` names, at the generation this Mac's catalog says to use
    /// ([`current`]): after a roll, `alab/registry` reaches `alab/registry.2`.
    pub fn at(paths: Paths, compiled: &'static Compiled) -> Self {
        let compiled = current(&paths, compiled);
        Self { paths, compiled }
    }

    /// The table this handle reaches.
    pub const fn compiled(&self) -> &'static Compiled {
        self.compiled
    }

    fn descriptor(&self) -> impl FnOnce(&Bootstrap) -> Result<Descriptor> + use<> {
        let table = self.compiled.table;
        let version = self.compiled.version;
        move |bootstrap| {
            Descriptor::application(bootstrap.trust(), table.application, table.role, version)
        }
    }

    /// Read the table this Mac holds, without enrolling it.
    ///
    /// # Errors
    /// No Commons, the table is not declared or not subscribed here, or its storage is damaged.
    pub fn view(&self) -> Result<View> {
        let table = self.compiled.table;
        application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
            Ok(View::read(
                table,
                session,
                &Log::read_with(session, store, scope, decode)?,
            ))
        })
    }

    /// The view and what the table's storage holds here.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn usage(&self) -> Result<(View, Usage)> {
        let table = self.compiled.table;
        application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
            let used = store.retained_use(session, session.actor())?;
            Ok((
                View::read(
                    table,
                    session,
                    &Log::read_with(session, store, scope, decode)?,
                ),
                Usage {
                    actor: session.actor(),
                    records: used.records,
                    bytes: used.bytes,
                    mine_records: used.writer_records,
                    mine_bytes: used.writer_bytes,
                },
            ))
        })
    }

    /// Every eligible operation, oldest claim first, with its writer's machine resolved through
    /// the roster the record names, so a removed Mac's edits stay attributable.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn history(&self) -> Result<Vec<Entry>> {
        application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
            let log = Log::read_with(session, store, scope, decode)?;
            let resolve = owned::commons::historical(session);
            let mut entries: Vec<Entry> = log
                .eligible()
                .into_iter()
                .filter_map(|(operation, command)| {
                    Some(Entry {
                        operation: operation.header.id,
                        actor: operation.header.actor,
                        machine: resolve(&operation.roster, &operation.header.actor),
                        authored_ms: operation.authored_ms,
                        request: operation.request,
                        reverts: operation.reverts,
                        changes: command.changes()?.to_vec(),
                    })
                })
                .collect();
            entries.sort_by_key(|e| (e.authored_ms, e.operation));
            Ok(entries)
        })
    }

    /// Write a grid save (design §8): under the store's writer, re-read the table and settle
    /// `plan` against it ([`grid::settle`]): each changed register is written only while its
    /// candidates are still the exported ones, a cell that changed meanwhile is left alone and
    /// returned as stale, and one row's changes go in one owner record (at most 64 changes, a
    /// fresh random request id each).
    ///
    /// # Errors
    /// No Commons writer; this Mac's history of the table is incomplete (an operation still
    /// waiting on a causal parent could hold a candidate the comparison cannot see); or a
    /// record cannot be written.
    pub fn apply(&self, collection: &Collection, plan: &Plan) -> Result<Applied> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let mut log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let view = View::read(table, session, &log);
                if !view.complete {
                    return Err(Error::new(
                        "this table's history here is incomplete; a grid save waits for a complete scan",
                    ));
                }
                let settled = grid::settle(collection, plan, &view.cells);
                let mut applied = Applied {
                    unchanged: settled.unchanged,
                    stale: settled.stale,
                    born: settled.born,
                    retired: settled.retired,
                    ..Applied::default()
                };
                for changes in pack(settled.rows)? {
                    let count = changes.len();
                    let command = Command::Write {
                        request: crate::crypto::random()?,
                        changes,
                    };
                    let receipt = application::append(session, store, &command.encode()?)?;
                    applied.records += 1;
                    applied.cells += count;
                    applied.operations.push(receipt.id);
                    // Re-read before admitting another append (`Log::append_records_bound`).
                    log = Log::read_with(session, store, scope, decode)?;
                }
                drop(log);
                Ok(applied)
            },
        )
    }

    /// Write rows `derive` computes from the table as it is under the store's writer (the lab
    /// importer's three-way plan, design §9), declaring and enrolling the table when `declare`:
    /// the plan is made on a complete history only, so a withheld operation can never hide an
    /// owner's edit from the comparison; its rows are packed into records of at most 64 changes,
    /// each with a content-derived request id, so a retry returns its first receipt and a re-run
    /// of the same plan writes nothing.
    ///
    /// # Errors
    /// No Commons writer; this Mac's history of the table is incomplete; `derive` fails; or a
    /// record cannot be written.
    pub(crate) fn commit_derived<T>(
        &self,
        declare: bool,
        derive: impl FnOnce(&View) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            declare,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let mut log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let view = View::read(table, session, &log);
                if !view.complete {
                    return Err(Error::new(
                        "this table's history here is incomplete; an import waits for a complete scan",
                    ));
                }
                let (planned, rows) = derive(&view)?;
                let mut applied = Applied::default();
                for changes in pack(rows)? {
                    let count = changes.len();
                    let request = typed::request_id(table.application, table.role, &changes)?;
                    let command = Command::Write { request, changes };
                    if let Some(first) = log.retry(session.actor(), &command)? {
                        applied.unchanged += count;
                        applied.operations.push(first);
                        continue;
                    }
                    let receipt = application::append(session, store, &command.encode()?)?;
                    applied.records += 1;
                    applied.cells += count;
                    applied.operations.push(receipt.id);
                    // Re-read before admitting another append (`Log::append_records_bound`).
                    log = Log::read_with(session, store, scope, decode)?;
                }
                drop(log);
                Ok((planned, applied))
            },
        )
    }

    /// Append the exact compensation of `operation` while every key it wrote still holds it as
    /// the sole candidate (`Log::compensate`); a later edit stays. The target, current value and
    /// preimage use the same quota-filtered history as [`Self::view`]. An exact retry of
    /// `request` returns the first operation.
    ///
    /// # Errors
    /// No Commons writer, the operation is not an eligible typed record here, or a key it wrote
    /// changed since.
    pub fn revert(&self, request: [u8; 16], operation: Digest) -> Result<Digest> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                if let Some(id) = log.revert_retry(session.actor(), request, operation)? {
                    return Ok(id);
                }
                let view = View::read(table, session, &log);
                let command = log.compensate(request, operation, &view.quota.over)?;
                Ok(application::append(session, store, &command.encode()?)?.id)
            },
        )
    }

    /// Publish the compiled schema rows (design §3.5), an owner verb: only where the stored row
    /// is absent or below the compiled version, never automatically when a build opens the
    /// table, never below a version already visible and never a second row at an equal version.
    /// Returns the records written.
    ///
    /// # Errors
    /// No Commons writer, or the table cannot be read or written.
    pub fn publish_schema(&self) -> Result<usize> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            true,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let mut log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let mut written = 0;
                for changes in pack(schema_rows(table, &View::read(table, session, &log)))? {
                    let request = typed::request_id(table.application, table.role, &changes)?;
                    let command = Command::Write { request, changes };
                    if log.retry(session.actor(), &command)?.is_some() {
                        continue;
                    }
                    application::append(session, store, &command.encode()?)?;
                    written += 1;
                    log = Log::read_with(session, store, scope, decode)?;
                }
                Ok(written)
            },
        )
    }
}

/// The schema rows a build compiled at these versions may publish over `view`: a collection
/// whose stored row is absent or below the compiled version. Never one below a version already
/// visible, and never a second row at an equal version (that would split it).
pub(crate) fn schema_rows(table: &Table, view: &View) -> Vec<Vec<Change>> {
    table
        .collections
        .iter()
        .filter(|collection| match view.schema(collection.name) {
            SchemaState::Absent => true,
            SchemaState::One { version, .. } | SchemaState::Split { version } => {
                version < collection.version
            }
        })
        .map(|collection| {
            vec![Change {
                collection: typed::SCHEMA.to_owned(),
                key: collection.name.to_owned(),
                value: Some(collection.schema_row()),
            }]
        })
        .collect()
}

/// Read a regular UTF-8 file of at most `limit` bytes without following a final symlink or
/// blocking on a FIFO (`LOCK_OPEN_FLAGS`).
pub(crate) fn read_regular_utf8(
    path: &std::path::Path,
    limit: usize,
    unreadable: &str,
    oversize: &str,
    non_utf8: &str,
) -> Result<String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let err = || Error::new(unreadable);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(path)
        .map_err(|_| err())?;
    let meta = file.metadata().map_err(|_| err())?;
    if !meta.is_file() {
        return Err(err());
    }
    if meta.len() > limit as u64 {
        return Err(Error::new(oversize));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| err())?;
    if bytes.len() > limit {
        return Err(Error::new(oversize));
    }
    String::from_utf8(bytes).map_err(|_| Error::new(non_utf8))
}

#[cfg(test)]
mod tests;
