// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every value one cell held, and what one write replaced, had not seen and what has written its
//! cells since (docs/DESIGN-tables-app-2026-11-14.md §4 item 4 and §3.3, tracker LT-34 and LT-25,
//! whose History shows a selected record so); and every write to a few
//! typed cells with both, read at once (tracker FL-258: this Mac's name's History and Undo read
//! its registry row's `label` and `nickname` so).
//!
//! Each read is one store read, one projection ([`View::of`] what `Resolved::read` resolves:
//! what stands now, the quota, and the records a roll archived) and one pass over the log's
//! eligible records in history order, looking only at the changes to the cells asked for. A
//! family that keeps its records adds, on every read, one store read and one projection for
//! each sealed generation before the handle's, and one pass over a sealed log where a read
//! asks it for its records ("A family that keeps its records" below).
//!
//! **A cell** is a typed cell's stored key, or an owned or log collection's row key, whose value
//! is the whole row. A write is listed only where a reader keeps it: every write to a typed
//! cell, and to an owned or log row the writes at the key their signer's machine owns
//! (`typed::owned::kept`), so a row another Mac wrote at this Mac's key is not listed.
//!
//! **Whether one write saw another** is the signed graph's answer (`Log::observes`), never a
//! clock's. Two concurrent writes tie on the derived clock and are ordered by digest, so neither
//! is "earlier": a write's unseen writes are those with which it is mutually unobserved, and each
//! side of a conflict names the other; what has written a cell since a write is the writes that
//! observed it. What a write replaced is the candidates the cell held in
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
//!
//! **A family that keeps its records** (`super::keeps_records`, tracker TK-41). Its roll
//! carries no history and archives no row: a sealed generation's log stays where it is, and an
//! owned row or a log entry signed into it is in no later log (`super::kept`). So each read
//! here opens the sealed generations before the one the handle reaches ([`Handle::earlier`])
//! and reads each as that generation reads its own log, under its own table, quota and
//! signers:
//! - What stands at an owned or log key is the family's row there: the head's view over the
//!   sealed generations' rows (`kept::overlay`), so a row no later generation wrote again, and
//!   every log entry, stands from the sealed log that holds it.
//! - A cell that is an owned or log key lists each sealed generation's writes there before
//!   this log's, by generation, each as a signed record of its own log ([`Source::Sealed`]):
//!   its operation, value, writer, machine and clocks are the record's own.
//! - What a sealed record wrote ([`Handle::wrote`]) is read in the log it was signed into: what
//!   it replaced, had not seen and what has written its cells since are that log's writes, as
//!   the signed graph of that log says, since a later generation's writes are of another log.
//!   Whether each write stands is the family's head's answer.
//!
//! A typed cell's writes are listed by [`Handle::cell_values`] and [`Handle::writes`] as
//! before: a restated register's candidates under [`Source::Restated`], at the record that
//! restated them, so those two and [`Handle::wrote`] of the sealed record that wrote one
//! (which lists it as that record, [`Source::Sealed`]) name its writer and clocks
//! differently. Nor does a write of a later generation list a sealed row as the one it
//! replaced.

use super::{Compiled, EntryKind, Handle, Inert, Mac, Macs, Resolved, View, kept};
use crate::{
    Error, Result,
    alab::{self, registry},
    database::Change,
    documents::{
        application,
        log::{Hlc, Log, Operation},
    },
    table::{Scope, Session, store::Store},
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
    time::Instant,
};

/// Where a listed write's record is, and so whose its writer and clocks are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A record of this generation's log: the writer, machine and clocks are its own.
    Log,
    /// A record of a sealed generation's own log, in a family that keeps its records
    /// (`super::kept`): the writer, machine and clocks are its own, as that log gave them, and
    /// it is authenticated here under its own signer's signature.
    Sealed {
        /// The generation whose log holds the record.
        generation: u32,
    },
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
    /// The writes to the cell that observed it: what has written there since, whether or not it
    /// stands now. In history order. Like `replaced` and `unseen`, only writes that project
    /// count, and a write that does not project has none.
    pub since: Vec<Held>,
}

/// One write to the typed cells [`Handle::writes`] asked for: the operation once, its writer
/// named as history names it, its kind, and each of those cells it wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The operation that wrote the cells.
    pub operation: Digest,
    pub source: Source,
    pub actor: Actor,
    /// The writer's Commons machine, from the binding in the roster the record names.
    pub machine: Option<[u8; 16]>,
    /// The Mac history names the writer by ([`Macs::name`]).
    pub mac: Mac,
    /// The signed writer clock claim.
    pub authored_ms: u64,
    /// The causal clock its log derived for it.
    pub hlc: Hlc,
    /// The operation it reverted, where it is a revert.
    pub reverts: Option<Digest>,
    /// What kind of write it is ([`EntryKind::of`]) over every change it made: a record of this
    /// log by its own changes; a write a roll carried by the changes its history entry kept, or,
    /// where no entry names it, by the value the roll restated.
    pub kind: EntryKind,
    /// Each cell asked for that it wrote, in the order asked: what it wrote there, what that
    /// replaced and what it had not seen ([`Wrote`]). A write of an earlier generation replaced
    /// and missed nothing here: the writes it saw are of another log.
    pub cells: Vec<Wrote>,
}

impl Handle {
    /// Every write to the cells `keys` of the typed collection `collection`, each operation once
    /// with each of those cells it wrote ([`Written`]), in history order as
    /// [`Self::cell_values`] lists one cell's: what [`Self::cell_values`] and [`Self::wrote`]
    /// answer for those cells, read in one observation of the store that gives up at `read_by`,
    /// so the values, what stands and the Macs that wrote them are of one moment. In a family
    /// that keeps its records, a sealed generation's write to one of these cells is listed as
    /// the roll restated it, as [`Self::cell_values`] lists it; [`Self::wrote`] of that
    /// operation reads it in its own log, so the two differ there (the module's "A family
    /// that keeps its records").
    ///
    /// # Errors
    /// As [`Self::view_until`]: the store is busy past `read_by`, among others; `collection` is
    /// no typed collection of this table; incomplete causal clocks or a logical counter overflow.
    pub fn writes(
        &self,
        collection: &str,
        keys: &[&str],
        read_by: Instant,
    ) -> Result<Vec<Written>> {
        super::reopened(read_by, || {
            application::observe_store_within(
                &self.paths,
                self.descriptor(),
                read_by,
                &|| crate::admission::deadline().min(read_by),
                |session, store, scope| {
                    over(
                        self.compiled,
                        &self.earlier(),
                        session,
                        store,
                        scope,
                        &|reader| reader.writes(collection, keys),
                    )
                },
            )
        })
    }

    /// Every value the cell `key` of `collection` held, each with the write that put it there:
    /// the writes an earlier generation's roll carried first, by generation, then this log's in
    /// history order (causal clock, then digest), each operation once. At an owned or log key
    /// of a family that keeps its records, the writes each sealed generation's log holds there
    /// come first too, by generation ([`Source::Sealed`]). A secret is listed with
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
    /// a roll carried as history and not as a record. A record of a sealed generation of a
    /// family that keeps its records is no such write: it is read from the log it was signed
    /// into (the module's "A family that keeps its records").
    pub fn wrote(&self, operation: Digest) -> Result<Vec<Wrote>> {
        self.reading(|reader| reader.wrote(&operation))
    }

    /// [`Self::wrote`], with the join that names each listed write's Mac ([`Macs`]) from the same
    /// read, so a reader that says who wrote each value names them as history does.
    ///
    /// # Errors
    /// As [`Self::wrote`].
    pub fn wrote_named(&self, operation: Digest) -> Result<(Vec<Wrote>, Macs)> {
        self.reading(|reader| Ok((reader.wrote(&operation)?, Macs::of(reader.view))))
    }

    fn reading<T>(&self, read: impl Fn(&Reader<'_>) -> Result<T>) -> Result<T> {
        super::reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                over(self.compiled, &self.earlier(), session, store, scope, &read)
            })
        })
    }
}

/// `read` over one observation of `compiled`'s store: its log, read with the records its
/// generation takes, the head's view and the history its rolls carried; and, where `earlier`
/// names the sealed generations of a family that keeps its records ([`Handle::earlier`]), each
/// of those as this Mac stores it, read as that generation reads its own log, with the head's
/// view laid over their rows (`kept::overlay`).
///
/// # Errors
/// As `Resolved::read`; this Mac stores no log of a sealed generation (`kept::sealed`), or its
/// storage cannot be read.
fn over<T>(
    compiled: &'static Compiled,
    earlier: &[&'static Compiled],
    session: &Session<'_>,
    store: &Store,
    scope: Scope,
    read: &impl Fn(&Reader<'_>) -> Result<T>,
) -> Result<T> {
    let table = compiled.table;
    let log = Log::read_with(session, store, scope, compiled.decoder())?;
    let resolve = owned::commons::historical(session);
    // Each sealed generation: its log, and its own view of it.
    let mut sealed = Vec::with_capacity(earlier.len());
    let mut rows = Vec::with_capacity(earlier.len());
    for generation in kept::sealed(session, earlier)? {
        let log = generation.log(session)?;
        let (resolved, view) = generation.view(session, &log)?;
        rows.push(generation.rows_of(&log, &resolved, &view));
        sealed.push((generation.compiled, log, resolved, view));
    }
    let resolved = Resolved::read(table, session, &log)?.over(rows);
    let view = View::of(table, &log, &resolved);
    let sealed: Vec<Reader<'_>> = sealed
        .iter()
        .map(|(compiled, log, resolved, view)| Reader {
            table: compiled.table,
            generation: Some(compiled.generation),
            log,
            carried: alab::history::carried(view, &resolve, None),
            view,
            resolved,
            resolve: &resolve,
            sealed: &[],
            archived: OnceCell::new(),
        })
        .collect();
    read(&Reader {
        table,
        generation: None,
        log: &log,
        carried: alab::history::carried(&view, &resolve, None),
        view: &view,
        resolved: &resolved,
        resolve: &resolve,
        sealed: &sealed,
        archived: OnceCell::new(),
    })
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

/// One read of a table: its log, the head's view and the history its rolls carried; or one
/// sealed generation of a family that keeps its records, read the same way from its own log.
struct Reader<'a> {
    table: &'static Table,
    /// The sealed generation this reads; `None` for the generation the handle reaches.
    generation: Option<u32>,
    log: &'a Log,
    /// The view of `log`: the handle's generation's over the sealed generations' rows, or a
    /// sealed generation's own.
    view: &'a View,
    /// What `view` resolved.
    resolved: &'a Resolved,
    resolve: &'a dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    carried: alab::history::Carried,
    /// The sealed generations before the handle's, oldest first, in a family that keeps its
    /// records; empty for any other family and in a sealed generation's own reader.
    sealed: &'a [Reader<'a>],
    /// The records a roll archived, each with the operation of this log that archived it
    /// (`super::archived_records` of the view's resolution over [`within`]), once an owned key
    /// asks for them.
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
const fn place(held: &Held) -> (u8, u32, Hlc, Digest, Digest) {
    match held.source {
        Source::Carried { generation, .. }
        | Source::Stated { generation, .. }
        | Source::Sealed { generation } => {
            (0, generation, held.hlc, held.operation, held.operation)
        }
        Source::Restated { by } => (1, 0, held.hlc, by, held.operation),
        Source::Log => (1, 0, held.hlc, held.operation, held.operation),
    }
}

/// The kind of the write a roll's carried history `entry` states, by the changes it kept.
fn carried_kind(table: &Table, entry: &alab::history::Entry) -> EntryKind {
    let changes: Vec<Change> = entry
        .changes
        .iter()
        .map(|written| Change {
            collection: written.collection.clone(),
            key: written.key.clone(),
            value: written.value.clone(),
        })
        .collect();
    EntryKind::of(table, entry.reverts.is_some(), &changes)
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
    /// An owned row or a log entry may stand from a sealed generation's log, in a family that
    /// keeps its records.
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
            // The view's own read (`owned::read_log` of the operations within quota), over
            // the sealed generations' entries where the family keeps its records.
            Some(Kind::Log) => self
                .view
                .logs
                .get(collection)
                .into_iter()
                .flat_map(|read| &read.entries)
                .filter(|entry| owned::log_entry_key(&entry.machine, entry.n) == key)
                .map(|entry| entry.operation)
                .collect(),
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

    /// Where a record of this reader's log is: this generation's log, or a sealed one's.
    const fn source(&self) -> Source {
        match self.generation {
            Some(generation) => Source::Sealed { generation },
            None => Source::Log,
        }
    }

    /// The writes at the owned or log key `key` of `collection` that this reader's log holds
    /// and a reader keeps, in its history order; none where this generation declares no such
    /// collection of rows. `standing` is what stands at the key at the family's head.
    fn rows_at(
        &self,
        collection: &str,
        key: &str,
        standing: &BTreeSet<Digest>,
    ) -> Result<Vec<Held>> {
        let rows = self
            .table
            .collection(collection)
            .is_some_and(|declared| declared.kind != Kind::Cells);
        if !rows {
            return Ok(Vec::new());
        }
        let wanted = Wanted::from([((collection, key), standing.clone())]);
        Ok(self
            .records(&wanted)?
            .remove(&(collection.to_owned(), key.to_owned()))
            .unwrap_or_default()
            .into_iter()
            .flat_map(|record| record.held)
            .collect())
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
                source: self.source(),
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

    /// The counting records of one cell's `records` beside `operation`'s write there, in history
    /// order: those it had not seen, which neither it nor they observed, and those that observed
    /// it, written there since.
    fn beside<'r>(
        &self,
        operation: &Digest,
        records: &'r [Record],
    ) -> (Vec<&'r Record>, Vec<&'r Record>) {
        let others = records
            .iter()
            .filter(|other| other.counts() && other.id != *operation);
        let unseen = others
            .clone()
            .filter(|other| {
                !self.log.observes(operation, &other.id) && !self.log.observes(&other.id, operation)
            })
            .collect();
        let since = others
            .filter(|other| self.log.observes(&other.id, operation))
            .collect();
        (unseen, since)
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
            .get_or_init(|| super::archived_records(self.resolved, &within(self.view)))
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
        // An owned row or a log entry of a sealed generation is in no later log: each is read
        // from the log it was signed into, as its signed record.
        for sealed in self.sealed {
            for held in sealed.rows_at(collection, key, &standing)? {
                if seen.insert(held.operation) {
                    out.push(held);
                }
            }
        }
        out.sort_by_key(place);
        Ok(out)
    }

    /// [`Handle::writes`]: each write to the typed cells `keys` of `collection`, once, in
    /// history order.
    fn writes(&self, collection: &str, keys: &[&str]) -> Result<Vec<Written>> {
        let typed = self
            .table
            .collection(collection)
            .is_some_and(|declared| declared.kind == Kind::Cells);
        if !typed {
            return Err(Error::new(&format!(
                "{} is no typed collection of this table",
                keys::safe_key(collection)
            )));
        }
        let wanted: Wanted<'_> = keys
            .iter()
            .map(|key| ((collection, *key), self.standing(collection, key)))
            .collect();
        let found = self.records(&wanted)?;
        let macs = Macs::of(self.view);
        let mut writes: BTreeMap<Digest, (Held, EntryKind, Vec<Wrote>)> = BTreeMap::new();
        let mut cell = |held: &Held, kind: EntryKind, wrote: Wrote| {
            writes
                .entry(held.operation)
                .or_insert_with(|| (held.clone(), kind, Vec::new()))
                .2
                .push(wrote);
        };
        let at = |key: &str, wrote: Vec<Held>, replaced, unseen, since| Wrote {
            collection: keys::safe_key(collection).to_owned(),
            key: keys::safe_key(key).to_owned(),
            wrote,
            replaced,
            unseen,
            since,
        };
        // The kind of a write only a value names: one change of it at the cell.
        let valued = |key: &str, value: Option<&Value>| {
            EntryKind::of(
                self.table,
                false,
                &[Change {
                    collection: collection.to_owned(),
                    key: key.to_owned(),
                    value: value.cloned(),
                }],
            )
        };
        for key in keys {
            let records = found
                .get(&(collection.to_owned(), (*key).to_owned()))
                .map_or(&[][..], Vec::as_slice);
            let mut seen = BTreeSet::new();
            for record in records {
                let own: BTreeSet<Digest> = record.held.iter().map(|held| held.operation).collect();
                let logged = record.held.iter().any(|held| held.source == self.source());
                if !logged {
                    // A roll's restatement: each candidate it lists is a write of an earlier
                    // generation, listed under its own operation.
                    for held in &record.held {
                        if seen.insert(held.operation) {
                            let kind = self.carried.entries.get(&held.operation).map_or_else(
                                || valued(key, held.value.as_ref()),
                                |entry| carried_kind(self.table, entry),
                            );
                            cell(
                                held,
                                kind,
                                at(key, vec![held.clone()], Vec::new(), Vec::new(), Vec::new()),
                            );
                        }
                    }
                    continue;
                }
                seen.extend(own.iter().copied());
                let (mut replaced, mut unseen, mut since) = (Vec::new(), Vec::new(), Vec::new());
                if record.counts() {
                    replaced = self.tops(&record.id, records);
                    replaced.reverse();
                    (unseen, since) = self.beside(&record.id, records);
                }
                let kind = self.log.eligible_record(&record.id).map_or_else(
                    || valued(key, None),
                    |(operation, command)| {
                        EntryKind::of(
                            self.table,
                            operation.reverts.is_some(),
                            command.changes().unwrap_or_default(),
                        )
                    },
                );
                let first = &record.held[0];
                cell(
                    first,
                    kind,
                    at(
                        key,
                        record.held.clone(),
                        listed(&replaced, &own),
                        listed(&unseen, &own),
                        listed(&since, &own),
                    ),
                );
            }
            let standing = &wanted[&(collection, *key)];
            for entry in self.carried.entries.values() {
                if !seen.contains(&entry.operation)
                    && let Some(held) = self.stated(entry, collection, key, standing)
                {
                    seen.insert(held.operation);
                    let kind = carried_kind(self.table, entry);
                    cell(
                        &held,
                        kind,
                        at(key, vec![held.clone()], Vec::new(), Vec::new(), Vec::new()),
                    );
                }
            }
        }
        let mut out: Vec<(Held, Written)> = writes
            .into_values()
            .map(|(held, kind, cells)| {
                let written = Written {
                    operation: held.operation,
                    source: held.source,
                    actor: held.actor,
                    machine: held.machine,
                    mac: macs.name(&held.actor, held.machine.as_ref()),
                    authored_ms: held.authored_ms,
                    hlc: held.hlc,
                    reverts: held.reverts,
                    kind,
                    cells,
                };
                (held, written)
            })
            .collect();
        out.sort_by_key(|(held, _)| place(held));
        Ok(out.into_iter().map(|(_, written)| written).collect())
    }

    /// [`Handle::wrote`]: `operation` read in the log that holds it, this one or, in a family
    /// that keeps its records, a sealed generation's, with what stands judged at this reader,
    /// the family's head.
    fn wrote(&self, operation: &Digest) -> Result<Vec<Wrote>> {
        if self.log.eligible_record(operation).is_none()
            && let Some(sealed) = self
                .sealed
                .iter()
                .find(|sealed| sealed.log.eligible_record(operation).is_some())
        {
            // What stands is the head's whole answer: a sealed generation's own restatement
            // lists candidates of a still earlier log, which stand where the head says so.
            return sealed.wrote_in(operation, &|collection, key| self.standing(collection, key));
        }
        self.wrote_in(operation, &|collection, key| self.standing(collection, key))
    }

    /// What `operation`, a record of this reader's log, wrote at each of its cells, with what
    /// that replaced, had not seen and what has written there since, all of this log;
    /// `standing` says what stands at a cell at the family's head.
    fn wrote_in(
        &self,
        operation: &Digest,
        standing: &dyn Fn(&str, &str) -> BTreeSet<Digest>,
    ) -> Result<Vec<Wrote>> {
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
                (cell, standing(cell.0, cell.1))
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
            let (mut replaced, mut unseen, mut since) = (Vec::new(), Vec::new(), Vec::new());
            if write.counts() {
                replaced = self.tops(operation, records);
                replaced.reverse();
                (unseen, since) = self.beside(operation, records);
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
                since: listed(&since, &own),
                collection: keys::safe_key(collection).to_owned(),
                key: keys::safe_key(key).to_owned(),
            });
        }
        Ok(out)
    }
}
