// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reading a family's signed records across a roll (tracker TK-31; decisions TK-D2 and TK-D17,
//! docs/DESIGN-lab-tracker-2026-11-14.md §8).
//!
//! A family that declares no secret column keeps every signed record ([`super::keeps_records`]):
//! its roll ([`super::roll`]) restates the registers into the next generation and seals the old
//! one, whose log stays on every Mac that holds it and is repaired to every Mac that does not
//! (`catalog::State::Kept`). Nothing restates its history, so nothing bounds it and no horizon
//! hides any of it: history, as-of and diff open the sealed generation's own log here, and each
//! record is read as its generation reads its records, authenticated under its own signer's
//! signature, with the principal, reason and evidence it states ([`crate::said`]).
//!
//! - **History** ([`Handle::history_page`]) is every eligible record of every generation. A
//!   record of one generation never observed one of another in the signed graph, so the two are
//!   ordered as concurrent records of one log are: by derived clock, then digest. Within a
//!   generation that is its log's own causal order; a roll's restatement is authored when the
//!   roll ran, so it follows what it restates wherever the clocks agree, and a record a Mac
//!   wrote into the sealed generation after the roll (it had not heard the seal) is listed
//!   where its clock puts it. Each entry names its generation ([`Entry::generation`]).
//! - **A revision** ([`Handle::revision`]) covers every generation's store ([`revision`]): a
//!   record that reaches a sealed generation late moves it, as one that reaches the current
//!   generation does.
//! - **As of** ([`Handle::view_at`]) a point of a sealed generation is that generation's table
//!   at the cut the operation names ([`as_of`]).
//! - **Diff** ([`Handle::diff`]) from a point of a sealed generation to a later one, or to the
//!   head, compares each register by the operations that wrote its candidates
//!   (`typed::cell::origins`: a restated register stands for the candidates it lists), so a cell
//!   the roll only restated does not differ, and one that does is attributed to the record that
//!   wrote it, in whichever generation's log that is ([`diff`]). A point of a later generation
//!   is taken to have observed every point of an earlier one: a record that reached the sealed
//!   generation after the roll restated it is not in the next generation until the roll is run
//!   again, and until then such a comparison shows its cell as the next generation holds it.
//!
//! **Owned rows and log entries** (tracker TK-40). A restatement carries registers. An owned
//! row or a log entry is its signer's own record, which only the log it was signed into holds:
//! no roll restates or archives one. A view of a later generation therefore reads them from
//! each sealed generation's log ([`Rows`]): each generation's own read of its own log, under
//! its own table, quota and signers, laid under the later generation's own
//! ([`overlay`]). The generations rank as the periods of one ring do
//! (`typed::owned::read_owned`, `read_log`): an owned row a Mac wrote in a later generation
//! stands over the one it wrote in an earlier one, whatever their counters; and a log is
//! append-only across the roll as within one log, so an entry at a key an earlier generation
//! holds is inert, and an append continues its Mac's counter past every sealed generation
//! of the family ([`before`], [`Sealed::spent`]) so that no key is used twice. As of a point of any generation, and on
//! each side of a diff, the rows of the generations before the point's own are read whole, as
//! a later point is taken to have observed an earlier generation (above). A checkpoint signs
//! its own log's state alone, and a quota counts its own generation's records alone: neither
//! reads these rows (`View::read`).
//!
//! Two readers that open no handle's view read them the same way (tracker TK-41): the
//! shared-file materializer, whose files, history and frontier are of every generation
//! (`file::materialize`, [`earlier`], [`Sealed::witnesses`]), and the per-cell reads
//! (`super::values`, [`Sealed::view`]), which list a sealed row's record from its own log.
//!
//! A column the next generation holds in another form (a list it holds as a text set) has no
//! register to compare key for key, so a diff between two such generations is refused rather
//! than show every such cell as gone and come back; and a later point that does not hold the
//! roll's whole restatement yet (its cover has not arrived) is refused the same way.
//!
//! A read across a roll is made through a handle of the family's first generation
//! ([`Handle::at`]): a handle asked for a later generation reads the generations from that one
//! to the one it reaches. A Mac that does not hold a sealed generation yet (it joined after the
//! seal and has not repaired it) refuses these reads until it does: an entry left out would be
//! a history that lies. A sealed generation is read as the current one is, with what a
//! recovery replica holds of it (`Directory::observe_table_if_exists`).

use super::{
    Compiled, Diff, Handle, Maker, Point, Resolved, View,
    diff::between,
    history::{Entry, Macs},
};
use crate::{
    Error, Result,
    catalog::Directory,
    database::Candidate,
    documents::log::Log,
    table::{Scope, Session, store::Store},
    typed::{
        Kind, Table, cell,
        owned::{self, LogRead, Op, OwnedRead},
    },
};
use astream_wire::sync::{Actor, Digest};
use std::collections::BTreeMap;

/// One sealed generation of a family as this Mac stores it.
pub(crate) struct Sealed {
    pub(crate) compiled: &'static Compiled,
    scope: Scope,
    store: Store,
}

impl Sealed {
    /// Its log, each record read as this generation reads its records.
    ///
    /// # Errors
    /// Its storage cannot be read.
    pub(crate) fn log(&self, session: &Session<'_>) -> Result<Log> {
        Log::read_with(session, &self.store, self.scope, self.compiled.decoder())
    }

    /// The custody witness of each store this generation was read from, in the read's order
    /// (`Store::witnesses`): what a shared file's frontier names beside the current
    /// generation's (`aterm_file::Frontier::stores`).
    pub(crate) fn witnesses(&self) -> Vec<aterm_file::Witnessed> {
        self.store.witnesses()
    }

    /// This generation's own view of `log`, its log, with what that view resolved through
    /// `session`: its own read of its own log, under its own table, quota and signers, with
    /// nothing of an earlier generation laid under it.
    ///
    /// # Errors
    /// As `Resolved::read`.
    pub(crate) fn view(&self, session: &Session<'_>, log: &Log) -> Result<(Resolved, View)> {
        let table = self.compiled.table;
        let resolved = Resolved::read(table, session, log)?;
        let view = View::of(table, log, &resolved);
        Ok((resolved, view))
    }

    /// The owned rows and log entries `view` holds ([`Rows`]): `view` is [`Self::view`] of
    /// `log` under `resolved`.
    pub(crate) fn rows_of(&self, log: &Log, resolved: &Resolved, view: &View) -> Rows {
        Rows {
            generation: self.compiled.generation,
            judgment: log.judgment(),
            resolution: resolved.digest(),
            owned: view.owned.clone(),
            logs: view.logs.clone(),
        }
    }

    /// The owned rows and log entries `log`, this generation's log, holds ([`Rows`]).
    ///
    /// # Errors
    /// As `Resolved::read`.
    pub(crate) fn rows(&self, session: &Session<'_>, log: &Log) -> Result<Rows> {
        let (resolved, view) = self.view(session, log)?;
        Ok(Rows {
            generation: self.compiled.generation,
            judgment: log.judgment(),
            resolution: resolved.digest(),
            owned: view.owned,
            logs: view.logs,
        })
    }

    /// The highest counter `machine` spent in this generation's log `collection`
    /// (`owned::log_spent`): 0 where this generation declares no such log.
    ///
    /// # Errors
    /// Its storage cannot be read.
    pub(crate) fn spent(
        &self,
        session: &Session<'_>,
        collection: &str,
        machine: &[u8; 16],
    ) -> Result<u64> {
        let Some(collection) = self
            .compiled
            .table
            .collection(collection)
            .filter(|held| held.kind == Kind::Log)
        else {
            return Ok(0);
        };
        let log = self.log(session)?;
        let ops: Vec<Op> = log
            .eligible_iter()
            .filter_map(|(operation, command)| Op::from_eligible(0, operation, command))
            .collect();
        Ok(owned::log_spent(
            collection,
            &ops,
            owned::commons::historical(session),
            machine,
        ))
    }
}

/// What one sealed generation's log holds of its signers' own rows (module docs): its owned
/// collections' rows and its log collections' entries as that generation's own view reads
/// them, each at its signer's key, the records past that generation's quota left out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Rows {
    generation: u32,
    /// What fixes the two reads: the log's judgment of each record it holds, and the digest
    /// of what the session resolved for it.
    judgment: [u8; 32],
    resolution: [u8; 32],
    owned: BTreeMap<&'static str, OwnedRead>,
    logs: BTreeMap<&'static str, LogRead>,
}

impl Rows {
    /// What fixes these rows into `sha`: equal bytes read every row alike.
    pub(super) fn digest_into(&self, sha: &mut aterm_digest::Sha256) {
        // Every field takes a position: a new one does not compile until it does. The two
        // reads are functions of the log and its resolution, which the two digests fix.
        let Self {
            generation,
            judgment,
            resolution,
            owned: _,
            logs: _,
        } = self;
        sha.update(generation.to_le_bytes());
        sha.update(judgment);
        sha.update(resolution);
    }
}

/// Each of `sealed`'s rows, oldest first.
///
/// # Errors
/// A generation's log cannot be read, or as `Resolved::read`.
pub(crate) fn rows(session: &Session<'_>, sealed: &[Sealed]) -> Result<Vec<Rows>> {
    sealed
        .iter()
        .map(|generation| generation.rows(session, &generation.log(session)?))
        .collect()
}

/// Lay `sealed`, the rows of the generations before a view's own, oldest first, under
/// `owned` and `logs`, that view's own reads of its own log (module docs). A collection is
/// read from a sealed generation where both declare it, by name, as the same kind.
///
/// An owned key keeps the row of the latest generation that holds one. A log keeps, at each
/// key, the entry of the earliest generation that holds one, and counts each later one inert,
/// as `owned::read_log` counts a reused counter within one log.
pub(super) fn overlay(
    sealed: &[Rows],
    owned: &mut BTreeMap<&'static str, OwnedRead>,
    logs: &mut BTreeMap<&'static str, LogRead>,
) {
    for (name, read) in owned {
        for earlier in sealed.iter().rev() {
            let Some(earlier) = earlier.owned.get(name) else {
                continue;
            };
            read.inert += earlier.inert;
            for (key, row) in &earlier.rows {
                read.rows.entry(key.clone()).or_insert_with(|| row.clone());
            }
        }
    }
    for (name, read) in logs {
        if sealed
            .iter()
            .all(|earlier| !earlier.logs.contains_key(name))
        {
            continue;
        }
        let own = std::mem::take(read);
        let mut entries = BTreeMap::new();
        for held in sealed
            .iter()
            .filter_map(|earlier| earlier.logs.get(name))
            .chain([&own])
        {
            read.inert += held.inert;
            for entry in &held.entries {
                match entries.entry((entry.machine, entry.n)) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(entry.clone());
                    }
                    std::collections::btree_map::Entry::Occupied(_) => read.inert += 1,
                }
            }
        }
        // In (machine, n) order, as one log's read is.
        read.entries = entries.into_values().collect();
    }
}

impl Handle {
    /// The generations from the one this handle was asked for ([`Handle::at`]: a family's
    /// first) up to the one it reaches, that one left out, oldest first, where they keep every
    /// signed record across a roll ([`super::keeps_records`]): the sealed generations a read
    /// across a roll opens. Empty for any other family, and where the handle reaches the
    /// generation it was asked for.
    pub(crate) fn earlier(&self) -> Vec<&'static Compiled> {
        earlier(self.family, self.compiled)
    }
}

/// The generations of `first`'s family before `reached`, oldest first, where the family keeps
/// every signed record across a roll ([`super::keeps_records`]): the sealed generations a read
/// of `reached` made from `first` opens. Empty for any other family, and where `reached` is
/// `first`.
pub(crate) fn earlier(
    first: &'static Compiled,
    reached: &'static Compiled,
) -> Vec<&'static Compiled> {
    if !super::keeps_records(first) {
        return Vec::new();
    }
    first
        .chain()
        .into_iter()
        .take_while(|compiled| compiled.generation < reached.generation)
        .collect()
}

/// Every generation before `reached` in its family, oldest first, where the family keeps its
/// records: the family is the registered one whose chain holds `reached`
/// (`super::families`), whichever generation a handle was asked for, and `asked`'s where none
/// is registered. An append reads these ([`Sealed::spent`]): a key spent in any of them
/// stands in every later view, so a handle asked for a later generation, which reads from
/// that generation on ([`Handle::earlier`]), must still not use one twice.
pub(crate) fn before(
    asked: &'static Compiled,
    reached: &'static Compiled,
) -> Vec<&'static Compiled> {
    let first = super::families()
        .into_iter()
        .find(|first| {
            first
                .chain()
                .iter()
                .any(|held| std::ptr::eq(*held, reached))
        })
        .unwrap_or(asked);
    earlier(first, reached)
}

/// Each of `earlier`, a family's sealed generations, as this Mac stores it.
///
/// # Errors
/// This Mac stores no log of one of them, or its storage cannot be read.
pub(crate) fn sealed(session: &Session<'_>, earlier: &[&'static Compiled]) -> Result<Vec<Sealed>> {
    if earlier.is_empty() {
        return Ok(Vec::new());
    }
    let directory = Directory::new(session.paths(), session.trust());
    let mut out = Vec::with_capacity(earlier.len());
    for &compiled in earlier {
        let scope = compiled.descriptor(session.trust())?.scope;
        let store = directory
            .observe_table_if_exists(session, scope)?
            .ok_or_else(|| {
                Error::new(&format!(
                    "{}: this Mac does not hold this sealed generation's records yet; a repair \
                     brings them",
                    compiled.name()
                ))
            })?;
        out.push(Sealed {
            compiled,
            scope,
            store,
        });
    }
    Ok(out)
}

/// The revision of a family read across a roll: `own`, the current generation's store's, where
/// no generation is sealed; else a digest over it and each sealed generation's store's, so a
/// record that reaches any of them moves it.
///
/// # Errors
/// A sealed store's revision cannot be read.
pub(crate) fn revision(own: Digest, session: &Session<'_>, sealed: &[Sealed]) -> Result<Digest> {
    if sealed.is_empty() {
        return Ok(own);
    }
    let mut sha = aterm_digest::Sha256::new();
    sha.update(b"aterm/att/family-revision/1\0");
    sha.update(own);
    for generation in sealed {
        sha.update(generation.compiled.generation.to_le_bytes());
        sha.update(generation.store.revision(session)?);
    }
    Ok(sha.finalize())
}

/// `own`, the current generation's entries in history's order, with every eligible entry of
/// each sealed generation's log, in history's order: derived clock, then digest (module docs).
/// `refused`, the records the current generation holds and does not project, takes each sealed
/// generation's too: a record no generation projects is still named.
///
/// # Errors
/// A sealed generation's log cannot be read or ordered.
pub(crate) fn merged(
    mut own: Vec<Entry>,
    refused: &mut Vec<crate::typed::Unread>,
    session: &Session<'_>,
    sealed: &[Sealed],
    resolve: &dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    macs: &Macs,
) -> Result<Vec<Entry>> {
    for generation in sealed {
        let log = generation.log(session)?;
        own.extend(super::history::ordered(
            generation.compiled,
            &log,
            resolve,
            macs,
        )?);
        refused.extend(log.refusals(&log.refused(&log.awaited(session.actor())), resolve));
    }
    if !sealed.is_empty() {
        own.sort_by_key(Entry::cursor);
    }
    Ok(own)
}

/// Whether `later` holds every column it shares with `earlier` in the form `earlier` holds it:
/// the same kind of collection and the same type of column, so a register of one is a register
/// of the other at the same key.
fn same_form(earlier: &Table, later: &Table) -> bool {
    earlier.collections.iter().all(|was| {
        later.collection(was.name).is_none_or(|is| {
            was.kind == is.kind
                && was
                    .columns
                    .iter()
                    .all(|column| is.column(column.name).is_none_or(|now| now.ty == column.ty))
        })
    })
}

/// The sealed generation whose log holds `at`, with its view as of that operation, over the
/// owned rows and log entries of the generations before it ([`Rows`]); `None` where none of
/// `earlier` holds it.
///
/// # Errors
/// As [`sealed`], or the view's resolution refuses.
pub(crate) fn as_of(
    session: &Session<'_>,
    earlier: &[&'static Compiled],
    at: &Digest,
) -> Result<Option<View>> {
    let mut before = Vec::new();
    for generation in sealed(session, earlier)? {
        let log = generation.log(session)?;
        if log.holds(at) {
            let table = generation.compiled.table;
            return View::read_over(table, session, &log.as_of(at)?, before).map(Some);
        }
        before.push(generation.rows(session, &log)?);
    }
    Ok(None)
}

/// `view`, each register holding the candidates it stands for, each under the operation that
/// wrote it (`cell::origins`): a restated register is no candidate of its own.
fn by_origin(mut view: View) -> View {
    for register in view.cells.values_mut() {
        register.candidates = cell::origins(register)
            .into_iter()
            .map(|(operation, value)| Candidate { operation, value })
            .collect();
    }
    view
}

/// What changed from `from`, a point of one of `earlier`'s logs, to `to` (module docs), `own`
/// being the log of `current`, the generation the handle reaches; `None` where no sealed
/// generation holds `from`.
///
/// # Errors
/// As [`sealed`]; this Mac retains no operation `to` names in the family; `to` is a point of
/// the same or an earlier generation that did not observe `from`; `to` is a point of a later
/// generation that does not hold the roll's whole restatement yet; or the two generations hold
/// a column in different forms.
pub(crate) fn diff(
    session: &Session<'_>,
    current: &'static Compiled,
    own: &Log,
    earlier: &[&'static Compiled],
    from: Digest,
    to: Point,
) -> Result<Option<Diff>> {
    let generations = sealed(session, earlier)?;
    let mut logs = Vec::with_capacity(generations.len());
    for generation in &generations {
        logs.push(generation.log(session)?);
    }
    let Some(first) = logs.iter().position(|log| log.holds(&from)) else {
        return Ok(None);
    };
    // Each point reads the owned rows and log entries of the generations before its own.
    let mut rows = Vec::with_capacity(generations.len());
    for (generation, log) in generations.iter().zip(&logs) {
        rows.push(generation.rows(session, log)?);
    }
    let before = View::read_over(
        generations[first].compiled.table,
        session,
        &logs[first].as_of(&from)?,
        rows[..first].to_vec(),
    )?;
    let (table, after) = match to {
        Point::Head => (
            current.table,
            View::read_over(current.table, session, own, rows)?,
        ),
        Point::At(at) if own.holds(&at) => (
            current.table,
            View::read_over(current.table, session, &own.as_of(&at)?, rows)?,
        ),
        Point::At(at) => {
            let Some(second) = logs.iter().position(|log| log.holds(&at)) else {
                return Err(Error::new(
                    "no operation with that digest in this table here (see `aterm db table \
                     history`)",
                ));
            };
            let later = logs[second].as_of(&at)?;
            if second < first || (second == first && !later.holds(&from)) {
                return Err(Error::new(
                    "the later point did not observe the first: compare each with head",
                ));
            }
            let table = generations[second].compiled.table;
            (
                table,
                View::read_over(table, session, &later, rows[..second].to_vec())?,
            )
        }
    };
    let from_table = generations[first].compiled.table;
    if !std::ptr::eq(from_table, table) {
        if !same_form(from_table, table) {
            return Err(Error::new(
                "the two generations hold a column in different forms: compare points of one \
                 generation",
            ));
        }
        if !after.awaiting.restated {
            return Err(Error::new(
                "the later point does not hold the roll's whole restatement yet: compare with a \
                 later point, or with head once it has arrived",
            ));
        }
    }
    let (before, after) = (by_origin(before), by_origin(after));
    let resolve = owned::commons::historical(session);
    let cells = between(table, &before, &after, |id| {
        logs.iter().chain([own]).find_map(|log| {
            let operation = log.operation(id)?;
            Some(Maker {
                operation: *id,
                actor: operation.header.actor,
                machine: resolve(&operation.roster, &operation.header.actor),
                said: log.said(id).cloned(),
            })
        })
    });
    Ok(Some(Diff {
        from,
        to,
        cells,
        refused: after.refusals.clone(),
    }))
}
