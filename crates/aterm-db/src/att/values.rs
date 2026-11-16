// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every value one cell held, and what one write replaced and had not seen
//! (docs/DESIGN-tables-app-2026-11-14.md §4 item 4, tracker LT-34).
//!
//! Both reads are one store read, one projection ([`View::of`] what `Resolved::read` resolves:
//! what stands now, the quota, and the records a roll archived) and one pass over the log's
//! eligible records in history order, looking only at the changes to the cells asked for.
//!
//! **A cell** is a typed cell's stored key, or an owned or log collection's row key, whose value
//! is the whole row. A write is listed only where a reader keeps it: every write to a typed
//! cell, and to an owned or log row the writes at the key their signer's machine owns
//! (`typed::owned::kept`), so a row another Mac wrote at this Mac's key is not listed.
//!
//! **Whether one write saw another** is the signed graph's answer (`Log::observes`), never a
//! clock's. Two concurrent writes tie on the derived clock and are ordered by digest, so neither
//! is "earlier": a write's unseen writes are those with which it is mutually unobserved, and each
//! side of a conflict names the other. What a write replaced is the candidates the cell held in
//! its causal past: the writes it observed that no other write it observed had observed. For an
//! owned row the same rule names its writer's previous row; which row stands there is by the
//! row's counter (`typed::owned::read_owned`).
//!
//! **A claim's re-entry.** A claim column's cell (a machine's label) can stand under an
//! operation that wrote no change to it: a row made live again re-asserts the claims it held
//! (`typed::claim`), and the view names that row write as the cell's candidate. A re-entry is no
//! write to the cell and is not listed: the writes it re-asserts stand, which are the candidates
//! the cell held in the re-entry's causal past, by the rule of what a write replaced.
//!
//! **Past quota.** An automatic record past its writer's quota never projects
//! (`typed::owned::quota`): it is listed in its cell's values, marked [`Inert::Quota`], never
//! stands, is in no replaced or unseen list, and itself replaced and missed nothing.
//!
//! **Secrets and refused values.** A write is listed with no value ([`Held::withheld`]) where
//! its value is a secret's: by the rule a roll's carried history keeps
//! (`alab::history::secret_change`: a `Secret` or `SecretRef` column, or a sealed token anywhere
//! in the value), and in a column this build does not compile whose name is a secret's
//! (`typed::secret_named`). Any other value is listed as stored but for the strings no other
//! surface shows, by the test `aterm db table history` applies (`typed::grid::show`,
//! `typed::grid::raw_shown`): a string shaped like a credential or naming a secret, in a value
//! its column's type does not admit, in a column or collection this build does not compile, in
//! a cell's import source or anywhere in an owned row, is the refusal mark
//! (`typed::grid::REFUSED`), and the write says so ([`Held::refused`]). A key shaped like a
//! credential (a set member another build stored) is that mark too ([`Wrote::key`]). So no
//! secret, sealed token or refused string leaves these reads.
//!
//! **Through a roll.** A roll restates a register as one carried value naming the operation
//! that wrote each candidate (`typed::cell::carried`), and carries the old log's entries as
//! history rows (`alab::history`). A restatement is no edit: each candidate it lists is listed
//! under its own operation, with the writer and clocks the carried history states for it
//! ([`Source::Carried`]) or, where no history row names it, those of the record that restated it
//! ([`Source::Restated`]). The carried entries that wrote the same typed cell are listed too. An
//! earlier generation's clocks are of another log, so its writes come first, by generation.
//!
//! **An owned row through a roll.** A carried history entry is its roller's word, and any Writer
//! that rolled may state one; the owned rule takes a row only on its signer's. So at an owned or
//! log key a history entry never stands in for a signed record:
//! - A row a roll archived as its signed record (`super::archived_records`) is a write to its
//!   key whether or not it stands there. Its operation, value, writer and machine are the
//!   record's own, authenticated here. Only its clocks are another's: those the carried history
//!   states for that operation where the entry names the record's own writer and machine
//!   ([`Source::Carried`]), else those of the record of this log that archived it
//!   ([`Source::Restated`]).
//! - A row only a history entry states is listed apart ([`Source::Stated`]), where the key is
//!   the stated machine's own, and never stands: nothing here can check that the machine signed
//!   it. A secret's row, or one trimmed, is listed with no value as any other.
//!
//! An archived row is no record of this log, so no clock of this log places it: for what a
//! write replaced it counts as written where it was archived, below every row this generation's
//! log holds at its key. A write that observed no such row replaced the archived row its key
//! held then, which is the one the owned rule ranks first among those archived by records the
//! write observed (the record `super::archived_records` names for each: the first that archived
//! it). An archived row is in no unseen list: it is its key's own Mac's write of an earlier
//! generation, which that Mac's later writes followed.

use super::{Handle, Inert, Resolved, View, decode};
use crate::{
    Error, Result,
    alab::{self, registry},
    database::Change,
    documents::{
        application,
        log::{Hlc, Log, Operation},
    },
    typed::{
        self, Kind, Table,
        cell::{self, CellKey, CellValue},
        grid, keys, owned,
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_json::{Value, json};
use std::{
    borrow::Cow,
    cell::OnceCell,
    collections::{BTreeMap, BTreeSet},
};

/// Where a listed write's record is, and so whose its writer and clocks are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A record of this generation's log: the writer, machine and clocks are its own.
    Log,
    /// A write of an earlier generation that a roll's carried history states
    /// (`alab::history`): the clocks are the ones that generation's log gave it, as its roller
    /// stated them. So are a typed cell's writer and machine; an owned row's are its archived
    /// record's own signer, authenticated here, which the entry names too.
    Carried {
        /// The generation whose log holds the operation.
        generation: u32,
        /// Its values passed what one history entry carries, so it is listed with no value.
        trimmed: bool,
    },
    /// An owned or log row of an earlier generation that only a roll's carried history states:
    /// this Mac holds no signed record of it, so that it was written, its value, its writer,
    /// machine and clocks are all its roller's word, which the owned rule cannot check. It never
    /// stands.
    Stated {
        /// The generation whose log is stated to hold the operation.
        generation: u32,
        /// Its values passed what one history entry carries, so it is listed with no value.
        trimmed: bool,
    },
    /// A write of an earlier generation that no carried history names, listed at `by`, the
    /// record of this log that restated it (a carried register) or archived it (an owned row),
    /// with that record's clocks. A carried candidate's writer and machine are that record's
    /// too: only its operation and value are its own. An archived row's are its own signer's,
    /// authenticated here: only its clocks are the archiving record's.
    Restated { by: Digest },
}

/// One value a cell held, with the write that put it there.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    /// The operation that wrote the value.
    pub operation: Digest,
    pub source: Source,
    pub actor: Actor,
    /// The writer's Commons machine, from the binding in the roster the record names.
    pub machine: Option<[u8; 16]>,
    /// The signed writer clock claim.
    pub authored_ms: u64,
    /// The causal clock its log derived for it.
    pub hlc: Hlc,
    /// The operation it reverted, where it is a revert.
    pub reverts: Option<Digest>,
    /// The value as stored, but for what `refused` marks: a typed cell's `{r, x, …}`, an owned
    /// row's whole row. A revert in a generation a roll wrote into restores a carried register:
    /// it is listed as the one value that register holds, or as the register where it holds
    /// several (`{"k": [{"o", "v"}, …]}`, `typed::cell::carried`). `None` for a revert back to
    /// absence, a withheld secret, or a trimmed carried entry.
    pub value: Option<Value>,
    /// A secret's write: listed with no value.
    pub withheld: bool,
    /// `value` is not wholly as stored: each string of it that no other surface shows (one
    /// shaped like a credential or naming a secret, where the value's type does not admit it)
    /// is the refusal mark (`typed::grid::REFUSED`).
    pub refused: bool,
    /// Why it never projected: only [`Inert::Quota`] here, since a row no reader keeps is not
    /// listed.
    pub inert: Option<Inert>,
    /// It is one of the cell's candidates at the head, as the view reads them (claims
    /// interpreted, the quota applied), or the row an owned or log key holds there. Where a
    /// claim's re-entry stands at the cell, the writes it re-asserts do.
    pub stands: bool,
}

/// One cell an operation wrote: what it wrote there, what that replaced, and the writes to the
/// cell it had not seen.
#[derive(Clone, Debug, PartialEq)]
pub struct Wrote {
    /// The collection's name; the refusal mark (`typed::grid::REFUSED`) for one shaped like a
    /// credential.
    pub collection: String,
    /// The stored cell key, or an owned or log collection's row key; the refusal mark for one
    /// shaped like a credential, as every other surface prints it.
    pub key: String,
    /// What it wrote: one write, or each candidate a roll's restatement lists.
    pub wrote: Vec<Held>,
    /// The candidates the cell held in the operation's causal past, which its write dropped: the
    /// writes it observed that no other write it observed had observed, in history order. At an
    /// owned key where it observed none, the row a roll archived that the key held then.
    pub replaced: Vec<Held>,
    /// The writes neither it observed nor that observed it: the values that stand, or stood,
    /// beside its own. In history order. A row a roll archived is never one.
    pub unseen: Vec<Held>,
}

impl Handle {
    /// Every value the cell `key` of `collection` held, each with the write that put it there:
    /// the writes an earlier generation's roll carried first, by generation, then this log's in
    /// history order (causal clock, then digest), each operation once. A secret is listed with
    /// no value ([`Held::withheld`]), and a string no other surface shows as the refusal mark
    /// ([`Held::refused`]).
    ///
    /// # Errors
    /// As [`Self::view`]; also incomplete causal clocks or a logical counter overflow.
    pub fn cell_values(&self, collection: &str, key: &str) -> Result<Vec<Held>> {
        self.reading(|reader| reader.cell(collection, key))
    }

    /// For each cell `operation` wrote, in collection and key order: what it wrote, what that
    /// replaced and what it had not seen ([`Wrote`]). A change no reader keeps (an owned row at
    /// another Mac's key) is left out, and so is a claim cell a row's re-entry re-asserted, which
    /// the operation did not write.
    ///
    /// # Errors
    /// As [`Self::cell_values`], or `operation` is no eligible write of this table's log here:
    /// one this Mac does not hold, a refused revert, or a write of an earlier generation, which
    /// a roll carried as history and not as a record.
    pub fn wrote(&self, operation: Digest) -> Result<Vec<Wrote>> {
        self.reading(|reader| reader.wrote(&operation))
    }

    fn reading<T>(&self, read: impl Fn(&Reader<'_>) -> Result<T>) -> Result<T> {
        let table = self.compiled.table;
        super::reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, decode)?;
                let resolved = Resolved::read(table, session, &log)?;
                let view = View::of(table, &log, &resolved);
                let resolve = owned::commons::historical(session);
                read(&Reader {
                    table,
                    log: &log,
                    carried: alab::history::carried(&view, &resolve, None),
                    view: &view,
                    resolve: &resolve,
                    archive: &|| super::archived_records(&resolved, &within(&view)),
                    archived: OnceCell::new(),
                })
            })
        })
    }
}

/// The operations that stand at each cell asked for.
type Wanted<'k> = BTreeMap<(&'k str, &'k str), BTreeSet<Digest>>;

/// The records of this log found at each cell, in history order.
type Found = BTreeMap<(String, String), Vec<Record>>;

/// One record of this log at one cell: its clocks, and what it wrote there (each [`Held`] names
/// the record's own writer and Mac).
struct Record {
    id: Digest,
    authored_ms: u64,
    hlc: Hlc,
    held: Vec<Held>,
}

impl Record {
    /// Whether it projects: it is within its writer's quota.
    fn counts(&self) -> bool {
        self.held.iter().all(|held| held.inert.is_none())
    }
}

/// One read of a table: its log, the head's view and the history its rolls carried.
struct Reader<'a> {
    table: &'static Table,
    log: &'a Log,
    view: &'a View,
    resolve: &'a dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    carried: alab::history::Carried,
    /// The records a roll archived, each with the operation of this log that archived it
    /// (`super::archived_records` of the view's resolution over [`within`]).
    archive: &'a dyn Fn() -> Vec<(Digest, owned::Op)>,
    /// What `archive` read, once an owned key asks for it.
    archived: OnceCell<Vec<(Digest, owned::Op)>>,
}

/// `view`'s operations within their writers' quota: the ones its owned and log rows are read
/// from.
fn within(view: &View) -> Cow<'_, [owned::Op]> {
    let over = &view.quota.over;
    if over.is_empty() {
        return Cow::Borrowed(&view.ops);
    }
    Cow::Owned(
        view.ops
            .iter()
            .filter(|op| !over.contains(&op.id))
            .cloned()
            .collect(),
    )
}

/// One owned row a roll archived as its signed record.
struct Archived<'r> {
    /// The archived record: the operation of an earlier generation that wrote the row.
    record: &'r owned::Op,
    /// The machine the roster it was sealed under binds its signer to: the one its key names.
    machine: Option<[u8; 16]>,
    /// The operation of this log that archived it.
    by: Digest,
    /// The key of the archived row's `wire` cell, which `by` wrote.
    wire: String,
    row: &'r Value,
}

/// A row that holds only a counter, which the owned rule keeps wherever it keeps the key: it
/// stands in for the row of a carried entry stated with no value (`Reader::stated`).
fn keyed() -> Value {
    json!({"n": 0})
}

/// A stored value as listed.
struct Shown {
    value: Option<Value>,
    withheld: bool,
    refused: bool,
}

/// `value`, a value of no known type, as every surface shows it (`grid::raw_shown`).
fn raw(value: &Value) -> Shown {
    let shown = grid::raw_shown(value);
    Shown {
        refused: shown != *value,
        value: Some(shown),
        withheld: false,
    }
}

/// The value a write stored at `key` of `collection` as listed (the module's "Secrets and
/// refused values"): none for a secret's, and any other with each string no surface shows as the
/// refusal mark.
fn shown(table: &Table, collection: &str, key: &str, value: Option<&Value>) -> Shown {
    let none = |withheld| Shown {
        value: None,
        withheld,
        refused: false,
    };
    if alab::history::secret_change(table, collection, key, value) {
        return none(true);
    }
    let Some(value) = value else {
        return none(false);
    };
    let declared = table.collection(collection);
    if declared.is_some_and(|declared| declared.kind != Kind::Cells) {
        return raw(value);
    }
    let Ok(parsed) = CellKey::parse(key) else {
        return raw(value);
    };
    let column = declared.and_then(|declared| declared.column(parsed.col));
    // Another build's column: its name is all this one knows of what it holds.
    if column.is_none() && typed::secret_named(parsed.col) {
        return none(true);
    }
    let one =
        |value: &Value| column.map_or_else(|| raw(value), |column| typed_value(column, value));
    let Some(items) = cell::carried_refs(value) else {
        return one(value);
    };
    // A register of several candidates, which a revert restored: each candidate is listed as
    // its own value is, and the operations the register names as they are.
    let mut register = value.clone();
    let mut refused = false;
    let slots = register
        .as_object_mut()
        .and_then(|register| register.get_mut("k"))
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten();
    for (slot, (_, candidate)) in slots.zip(items) {
        let shown = one(candidate);
        refused |= shown.refused;
        if let (Some(slot), Some(value)) = (slot.as_object_mut(), shown.value) {
            slot.insert("v".into(), value);
        }
    }
    Shown {
        value: Some(register),
        withheld: false,
        refused,
    }
}

/// One stored value of the typed `column` as listed: as stored where its type admits it, else
/// as every surface shows a value of no known type; its import source by the same guard.
fn typed_value(column: &typed::Column, value: &Value) -> Shown {
    let Ok(mut cell) = CellValue::decode(value) else {
        return raw(value);
    };
    let mut refused = false;
    if !cell::admissible(&column.ty, &cell.x) {
        let x = grid::raw_shown(&cell.x);
        refused = x != cell.x;
        cell.x = x;
    }
    if cell.s.as_deref().is_some_and(typed::guard::withheld) {
        cell.s = Some(grid::REFUSED.to_owned());
        refused = true;
    }
    Shown {
        value: Some(if refused {
            cell.encode()
        } else {
            value.clone()
        }),
        withheld: false,
        refused,
    }
}

/// A revert's value as listed: the one candidate a restored carried register holds.
fn restored(value: &Value) -> &Value {
    match cell::carried_refs(value) {
        Some(items) if items.len() == 1 => items[0].1,
        _ => value,
    }
}

/// A listed write's place: an earlier generation's first, by generation, then this log's order,
/// a restatement's candidates at their record's place.
fn place(held: &Held) -> (u8, u32, Hlc, Digest, Digest) {
    match held.source {
        Source::Carried { generation, .. } | Source::Stated { generation, .. } => {
            (0, generation, held.hlc, held.operation, held.operation)
        }
        Source::Restated { by } => (1, 0, held.hlc, by, held.operation),
        Source::Log => (1, 0, held.hlc, held.operation, held.operation),
    }
}

/// `records`' writes, each operation once, without those `own` names.
fn listed(records: &[&Record], own: &BTreeSet<Digest>) -> Vec<Held> {
    let mut seen = BTreeSet::new();
    records
        .iter()
        .flat_map(|record| &record.held)
        .filter(|held| !own.contains(&held.operation) && seen.insert(held.operation))
        .cloned()
        .collect()
}

impl Reader<'_> {
    /// The operations that stand at `key` of `collection` at the head: a register's candidates,
    /// each carried one under the operation its register names (a revert that restored a
    /// register stands as itself); the row an owned key holds; a log key's first entry. A claim
    /// cell's candidate may be a row's re-entry, which wrote nothing there ([`Self::reassert`]).
    fn standing(&self, collection: &str, key: &str) -> BTreeSet<Digest> {
        let declared = self.table.collection(collection);
        match declared.map(|declared| declared.kind) {
            Some(Kind::Owned) => self
                .view
                .owned
                .get(collection)
                .and_then(|read| read.rows.get(key))
                .map(|row| row.operation)
                .into_iter()
                .collect(),
            Some(Kind::Log) => {
                let within = within(self.view);
                declared
                    .map(|declared| owned::read_log(declared, &within, self.resolve))
                    .unwrap_or_default()
                    .entries
                    .iter()
                    .filter(|entry| format!("{}.{}", keys::uuid32(&entry.machine), entry.n) == key)
                    .map(|entry| entry.operation)
                    .collect()
            }
            _ => self
                .view
                .cells
                .get(&(collection.to_owned(), key.to_owned()))
                .into_iter()
                .flat_map(|cell| &cell.candidates)
                .flat_map(|candidate| {
                    let reverted = self
                        .log
                        .operation(&candidate.operation)
                        .is_some_and(|operation| operation.reverts.is_some());
                    match cell::carried_refs(&candidate.value) {
                        Some(items) if !reverted => {
                            items.into_iter().map(|(origin, _)| origin).collect()
                        }
                        _ => vec![candidate.operation],
                    }
                })
                .collect(),
        }
    }

    /// What `operation`, clocked `hlc` and signed by `machine`, wrote with `change`: itself, or,
    /// where it is a roll's restatement, each candidate its carried register lists.
    fn held(
        &self,
        operation: &Operation,
        hlc: Hlc,
        machine: Option<[u8; 16]>,
        change: &Change,
        standing: &BTreeSet<Digest>,
    ) -> Vec<Held> {
        let id = operation.header.id;
        let inert = self.view.quota.over.contains(&id).then_some(Inert::Quota);
        let show =
            |value: Option<&Value>| shown(self.table, &change.collection, &change.key, value);
        let own = |value: Option<&Value>| {
            let shown = show(value);
            Held {
                operation: id,
                source: Source::Log,
                actor: operation.header.actor,
                machine,
                authored_ms: operation.authored_ms,
                hlc,
                reverts: operation.reverts,
                value: shown.value,
                withheld: shown.withheld,
                refused: shown.refused,
                inert,
                stands: inert.is_none() && standing.contains(&id),
            }
        };
        let stored = change.value.as_ref();
        if operation.reverts.is_some() {
            return vec![own(stored.map(restored))];
        }
        let Some(items) = stored.and_then(cell::carried_refs) else {
            return vec![own(stored)];
        };
        items
            .into_iter()
            .map(|(origin, value)| {
                let shown = show(Some(value));
                let stated = self.carried.entries.get(&origin);
                Held {
                    operation: origin,
                    source: stated.map_or(Source::Restated { by: id }, |entry| Source::Carried {
                        generation: entry.generation,
                        trimmed: false,
                    }),
                    actor: stated.map_or(operation.header.actor, |entry| entry.actor),
                    machine: stated.map_or(machine, |entry| entry.machine),
                    authored_ms: stated.map_or(operation.authored_ms, |entry| entry.authored_ms),
                    hlc: stated.map_or(hlc, |entry| entry.hlc),
                    reverts: stated.and_then(|entry| entry.reverts),
                    value: shown.value,
                    withheld: shown.withheld,
                    refused: shown.refused,
                    inert,
                    stands: inert.is_none() && standing.contains(&origin),
                }
            })
            .collect()
    }

    /// The records of this log that write the cells `wanted` and that a reader keeps, for each
    /// cell in history order, the writes a claim's re-entry re-asserts standing
    /// ([`Self::reassert`]).
    fn records(&self, wanted: &Wanted<'_>) -> Result<Found> {
        let mut found = Found::new();
        for (hlc, id) in self.log.causal_order()? {
            let Some((operation, command)) = self.log.eligible_record(&id) else {
                continue;
            };
            let mut signer = None;
            for change in command.changes().unwrap_or_default() {
                let Some(standing) = wanted.get(&(change.collection.as_str(), change.key.as_str()))
                else {
                    continue;
                };
                let actor = operation.header.actor;
                let machine =
                    *signer.get_or_insert_with(|| (self.resolve)(&operation.roster, &actor));
                if !owned::kept(self.table, change, machine) {
                    continue;
                }
                let record = Record {
                    id,
                    authored_ms: operation.authored_ms,
                    hlc,
                    held: self.held(operation, hlc, machine, change, standing),
                };
                let records = found
                    .entry((change.collection.clone(), change.key.clone()))
                    .or_default();
                // A record's last change to a key is the one that applies.
                match records.last_mut() {
                    Some(last) if last.id == id => *last = record,
                    _ => records.push(record),
                }
            }
        }
        for ((collection, key), records) in &mut found {
            if let Some(standing) = wanted.get(&(collection.as_str(), key.as_str())) {
                self.reassert(collection, standing, records);
            }
        }
        Ok(found)
    }

    /// The candidates the cell of `records` held in `operation`'s causal past, latest first:
    /// the counting records it observed that no other of them it observed had observed.
    fn tops<'r>(&self, operation: &Digest, records: &'r [Record]) -> Vec<&'r Record> {
        let mut tops: Vec<&Record> = Vec::new();
        // Latest first: a write that observed another is after it in history order, so a write
        // observed is a candidate unless one already found had observed it.
        for other in records
            .iter()
            .rev()
            .filter(|other| other.counts() && other.id != *operation)
        {
            if self.log.observes(operation, &other.id)
                && !tops.iter().any(|top| self.log.observes(&top.id, &other.id))
            {
                tops.push(other);
            }
        }
        tops
    }

    /// Marks as standing the writes of `records`, one typed cell's, that a claim's re-entry
    /// re-asserts. A candidate of `standing` that no record here lists wrote nothing to the
    /// cell: it is a row made live again, under which claim interpretation re-asserts the claim
    /// the cell held (`typed::claim`), so what stands is the cell's candidates in that
    /// operation's causal past ([`Self::tops`]).
    fn reassert(&self, collection: &str, standing: &BTreeSet<Digest>, records: &mut [Record]) {
        let typed = self
            .table
            .collection(collection)
            .is_none_or(|declared| declared.kind == Kind::Cells);
        if !typed {
            return;
        }
        let listed: BTreeSet<Digest> = records
            .iter()
            .flat_map(|record| &record.held)
            .map(|held| held.operation)
            .collect();
        let reasserted: BTreeSet<Digest> = standing
            .difference(&listed)
            .flat_map(|reentry| self.tops(reentry, records))
            .map(|record| record.id)
            .collect();
        for held in records
            .iter_mut()
            .filter(|record| reasserted.contains(&record.id))
            .flat_map(|record| &mut record.held)
        {
            held.stands = held.inert.is_none();
        }
    }

    /// What the carried history's `entry` wrote at `key` of `collection`, where a reader keeps
    /// it: a typed cell's write as its log gave it ([`Source::Carried`]); an owned or log row,
    /// which no signed record here backs, apart and never standing ([`Source::Stated`]), where
    /// the key is the stated machine's own.
    fn stated(
        &self,
        entry: &alab::history::Entry,
        collection: &str,
        key: &str,
        standing: &BTreeSet<Digest>,
    ) -> Option<Held> {
        let written = entry
            .changes
            .iter()
            .rev()
            .find(|written| written.collection == collection && written.key == key)?;
        let value = written.value.as_ref().map(|value| match entry.reverts {
            Some(_) => restored(value),
            None => value,
        });
        let rows = self
            .table
            .collection(collection)
            .is_some_and(|declared| declared.kind != Kind::Cells);
        // A secret's row and a trimmed one are carried with no value: the owned rule then
        // judges the key alone against the machine stated.
        let unvalued = written.withheld || entry.trimmed;
        if rows
            && !owned::kept(
                self.table,
                &Change {
                    collection: collection.to_owned(),
                    key: key.to_owned(),
                    value: value.cloned().or_else(|| unvalued.then(keyed)),
                },
                entry.machine,
            )
        {
            return None;
        }
        let shown = shown(self.table, collection, key, value);
        let (generation, trimmed) = (entry.generation, entry.trimmed && !written.withheld);
        Some(Held {
            operation: entry.operation,
            source: if rows {
                Source::Stated {
                    generation,
                    trimmed,
                }
            } else {
                Source::Carried {
                    generation,
                    trimmed,
                }
            },
            actor: entry.actor,
            machine: entry.machine,
            authored_ms: entry.authored_ms,
            hlc: entry.hlc,
            reverts: entry.reverts,
            value: shown.value,
            withheld: written.withheld || shown.withheld,
            refused: shown.refused,
            inert: None,
            stands: !rows && standing.contains(&entry.operation),
        })
    }

    /// The rows at `key` of the owned `collection` that a roll archived as their signed records,
    /// each one a reader keeps: its key is its own signer's machine, as the roster it was sealed
    /// under binds it.
    fn archived(&self, collection: &str, key: &str) -> Vec<Archived<'_>> {
        let owned = self
            .table
            .collection(collection)
            .is_some_and(|declared| declared.kind == Kind::Owned);
        if !owned {
            return Vec::new();
        }
        self.archived
            .get_or_init(|| (self.archive)())
            .iter()
            .filter_map(|(by, record)| {
                let machine = (self.resolve)(&record.roster, &record.actor);
                let row = record.changes.iter().rev().find(|change| {
                    change.collection == collection
                        && change.key == key
                        && owned::kept(self.table, change, machine)
                })?;
                Some(Archived {
                    record,
                    machine,
                    by: *by,
                    wire: format!("{}.wire", keys::archived_pk(&record.id)),
                    row: row.value.as_ref()?,
                })
            })
            .collect()
    }

    /// The archived `row` at `key` of `collection` as listed: its operation, value, writer and
    /// machine its record's own. Its clocks are those the carried history states for its
    /// operation, where that entry names the record's own writer and machine, else those of
    /// the record of this log that archived it, which `found` holds at the row's `wire` cell.
    fn restated(
        &self,
        collection: &str,
        key: &str,
        row: &Archived<'_>,
        standing: &BTreeSet<Digest>,
        found: &Found,
    ) -> Option<Held> {
        let record = row.record;
        let stated = self
            .carried
            .entries
            .get(&record.id)
            .filter(|entry| entry.actor == record.actor && entry.machine == row.machine);
        let (source, authored_ms, hlc, reverts) = match stated {
            Some(entry) => (
                Source::Carried {
                    generation: entry.generation,
                    trimmed: false,
                },
                entry.authored_ms,
                entry.hlc,
                entry.reverts,
            ),
            None => {
                let by = found
                    .get(&(registry::ARCHIVED.name.to_owned(), row.wire.clone()))
                    .and_then(|records| records.iter().find(|by| by.id == row.by))?;
                (Source::Restated { by: by.id }, by.authored_ms, by.hlc, None)
            }
        };
        let shown = shown(self.table, collection, key, Some(row.row));
        Some(Held {
            operation: record.id,
            source,
            actor: record.actor,
            machine: row.machine,
            authored_ms,
            hlc,
            reverts,
            value: shown.value,
            withheld: shown.withheld,
            refused: shown.refused,
            inert: None,
            stands: standing.contains(&record.id),
        })
    }

    fn cell(&self, collection: &str, key: &str) -> Result<Vec<Held>> {
        let standing = self.standing(collection, key);
        // An owned row a roll archived is no record of this log: the record that archived it is
        // the one of this log that can stand for it.
        let archived = self.archived(collection, key);
        let mut wanted = Wanted::from([((collection, key), standing.clone())]);
        for row in &archived {
            wanted
                .entry((registry::ARCHIVED.name, row.wire.as_str()))
                .or_default();
        }
        let mut found = self.records(&wanted)?;
        let mut seen = BTreeSet::new();
        let mut out: Vec<Held> = found
            .remove(&(collection.to_owned(), key.to_owned()))
            .unwrap_or_default()
            .into_iter()
            .flat_map(|record| record.held)
            .filter(|held| seen.insert(held.operation))
            .collect();
        // A signed record first: a history entry never stands in for one.
        for row in &archived {
            if !seen.contains(&row.record.id)
                && let Some(held) = self.restated(collection, key, row, &standing, &found)
            {
                seen.insert(held.operation);
                out.push(held);
            }
        }
        for entry in self.carried.entries.values() {
            if !seen.contains(&entry.operation)
                && let Some(held) = self.stated(entry, collection, key, &standing)
            {
                seen.insert(held.operation);
                out.push(held);
            }
        }
        out.sort_by_key(place);
        Ok(out)
    }

    fn wrote(&self, operation: &Digest) -> Result<Vec<Wrote>> {
        let (_, command) = self.log.eligible_record(operation).ok_or_else(|| {
            Error::new(
                "no eligible write with that digest in this table here (see `aterm db table history`)",
            )
        })?;
        let changes = command.changes().unwrap_or_default();
        let archived: BTreeMap<(&str, &str), Vec<Archived<'_>>> = changes
            .iter()
            .map(|change| {
                let cell = (change.collection.as_str(), change.key.as_str());
                (cell, self.archived(cell.0, cell.1))
            })
            .filter(|(_, rows)| !rows.is_empty())
            .collect();
        let mut wanted: Wanted<'_> = changes
            .iter()
            .map(|change| {
                let cell = (change.collection.as_str(), change.key.as_str());
                (cell, self.standing(cell.0, cell.1))
            })
            .collect();
        for row in archived.values().flatten() {
            wanted
                .entry((registry::ARCHIVED.name, row.wire.as_str()))
                .or_default();
        }
        let found = self.records(&wanted)?;
        let mut out = Vec::new();
        for ((collection, key), records) in &found {
            let Some(write) = records.iter().find(|record| record.id == *operation) else {
                continue;
            };
            let cell = (collection.as_str(), key.as_str());
            let own: BTreeSet<Digest> = write.held.iter().map(|held| held.operation).collect();
            let (mut replaced, mut unseen) = (Vec::new(), Vec::new());
            if write.counts() {
                replaced = self.tops(operation, records);
                replaced.reverse();
                unseen = records
                    .iter()
                    .filter(|other| {
                        other.counts()
                            && other.id != *operation
                            && !self.log.observes(operation, &other.id)
                            && !self.log.observes(&other.id, operation)
                    })
                    .collect();
            }
            let mut replaced = listed(&replaced, &own);
            // No row of this log at the key before it: it replaced the archived row the key
            // held, the one the owned rule ranks first of those it observed archived.
            if write.counts()
                && replaced.is_empty()
                && let Some(rows) = archived.get(&cell)
                && let Some(declared) = self.table.collection(collection)
            {
                let observed: Vec<&Archived<'_>> = rows
                    .iter()
                    .filter(|row| self.log.observes(operation, &row.by))
                    .collect();
                let ranked: Vec<owned::Op> =
                    observed.iter().map(|row| row.record.clone()).collect();
                let held = owned::read_owned(declared, &ranked, self.resolve)
                    .rows
                    .get(key)
                    .map(|row| row.operation);
                replaced.extend(
                    observed
                        .iter()
                        .find(|row| Some(row.record.id) == held)
                        .and_then(|row| {
                            self.restated(collection, key, row, &wanted[&cell], &found)
                        }),
                );
            }
            out.push(Wrote {
                wrote: write.held.clone(),
                replaced,
                unseen: listed(&unseen, &own),
                collection: keys::safe_key(collection).to_owned(),
                key: keys::safe_key(key).to_owned(),
            });
        }
        Ok(out)
    }
}
