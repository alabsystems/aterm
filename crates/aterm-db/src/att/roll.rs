// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The roll of a table family that keeps every signed record (tracker TK-31; decisions TK-D2,
//! TK-D17 and TK-D31, docs/DESIGN-lab-tracker-2026-11-14.md §8).
//!
//! A family none of whose generations declares a secret column ([`super::keeps_records`]) rolls
//! here, for any family this build compiles with a successor; a family that declares one (the
//! registry) rolls through `alab::roll`, which must take each secret out of what it carries.
//! [`Handle::roll`] is run on one Mac:
//!
//! 1. **Restate.** Every register of the old generation's collections (its schema rows and
//!    its own cover rows apart) goes into the next as one carried value listing each candidate
//!    it holds with the operation that wrote it, exactly as the registry's roll restates one
//!    (`alab::roll::restate_with_claims`): an open conflict stays open, and a list the next
//!    generation holds as a text set goes in as its members. The whole restatement must fit
//!    this Mac's writer share of the next generation and each row one record; a roll that does
//!    not is refused before anything is declared.
//! 2. **Cover.** One `roll` row for this Mac says which records of each writer the restatement
//!    carried (`alab::roll::cover_row`), written after the restatement: a reader of the next
//!    generation takes it as whole only once a cover is there (`View::complete`), so a Mac that
//!    hears the seal before the restatement reads no half of it. The next generation therefore
//!    declares the `roll` collection (`alab::registry::ROLL`); a roll into one that does not is
//!    refused. Where the old generation held schema rows, the next takes this build's compiled
//!    ones after the cover, as the registry's does.
//! 3. **Seal** the old generation, once every current Writer has declared the next one in its
//!    own name ([`endorse`], `catalog::Directory::undeclared`), which only a build that compiles
//!    it does. Readers and writers move to the next generation.
//!
//! **Nothing of the old generation's history is written, and nothing is released.** The roll
//! writes no history row and no horizon (`alab::history`), and archives no record: the sealed
//! generation's log is the history. It stays where it is on every Mac, the catalog reads it as kept
//! (`catalog::State::Kept`) so every Mac repairs it and a Mac that joins later fetches it, no
//! release pass names it (`catalog::release::retired`), and history, as-of and diff read each
//! record there under its own signer's signature, with its principal, reason and evidence
//! ([`super::kept`]). A sealed generation so holds a storage slot on every Mac for good
//! (`catalog::MAX_TABLES`); a checkpoint's cold segments are what bound it (tracker TK-01).
//!
//! **A record that reached a Mac late** (it edited the old generation before it heard the seal)
//! is repaired to every Mac like any record of a kept generation, and the roll run again on any
//! of them restates it: each run first carries what the sealed generations hold that their next
//! generation does not, oldest first, one generation a run.
//!
//! **Owned and log rows** (tracker TK-40). A restatement carries registers. An owned row or a
//! log entry is its signer's own record, which only the sealed generation's log holds: the
//! roll writes none of them, and a view of the next generation reads each from the sealed log,
//! under its own signer's signature and at its own key ([`super::kept::Rows`]). One that
//! reaches a generation after its seal is read the same way and needs no later run. The next
//! generation reads them through its own declaration of the collection, so a roll into a
//! generation that does not declare one of the old generation's owned or log collections as
//! the same kind is refused: nothing would read those rows, and their registers would be
//! restated as cells.
//!
//! The records a roll writes say who ran it where the handle says so ([`Handle::saying`]) and
//! the next generation takes version 2 records.

use super::{Compiled, Handle, View, pack_saying};
use crate::said::Said;
use crate::{
    Error, Result,
    alab::{
        registry::ROLL,
        roll::{
            carried_origins, cover_row, frontier, held_of_ops, request_role, restate_with_claims,
        },
    },
    catalog::{Directory, Snapshot},
    database::{Change, Command},
    documents::{application, log::Log},
    table::{
        Session,
        store::{Store, WRITER_BYTES, WRITER_RECORDS},
    },
    typed::{
        self, keys,
        secret::{Sealer, Secret},
    },
};
use astream_wire::sync::Digest;
use aterm_secrets::commons::bootstrap::Bootstrap;
use std::collections::{BTreeMap, BTreeSet};

/// The most a signed operation adds to its command's bytes.
const SIGNED: usize = crate::crypto::MAX_OPERATION - crate::crypto::MAX_BODY;

/// What a roll found or did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rolled {
    /// The generation restated, and the one it went into.
    pub from: u32,
    pub to: u32,
    /// Registers restated, and whether this Mac's cover row changed.
    pub registers: usize,
    pub cover: bool,
    /// Records appended (none for a plan).
    pub records: usize,
    /// Whether this roll sealed the old generation (an earlier one, or another Mac, may have).
    pub sealed: bool,
    /// The current Writers, by Commons machine, that have not declared the next generation, so
    /// have shown no build that reads it: the seal waits for them (for a plan, would wait).
    pub waiting: Vec<[u8; 16]>,
}

/// A family with no secret column holds none: the restatement meets no sealed token to open
/// and names no store entry, so one that asks is refused, naming no value.
struct NoSecret;

fn no_secret<T>() -> Result<T> {
    Err(Error::new(
        "this table family declares no secret column, so it holds no sealed value and names no \
         secret-store entry",
    ))
}

impl Sealer for NoSecret {
    fn table(&self) -> &str {
        ""
    }
    fn seal(&self, _: &str, _: &str, _: &str, _: &Secret) -> Result<String> {
        no_secret()
    }
    fn open(&self, _: &str, _: &str, _: &str, _: &str) -> Result<Secret> {
        no_secret()
    }
}

impl typed::secret::Store for NoSecret {
    fn vault(&self) -> Result<String> {
        no_secret()
    }
    fn admit(&self) -> Result<()> {
        no_secret()
    }
    fn put(&self, _: &str, _: &Secret) -> Result<()> {
        no_secret()
    }
    fn get(&self, _: &str) -> Result<Option<Secret>> {
        no_secret()
    }
    fn remove(&self, _: &str) -> Result<()> {
        no_secret()
    }
    fn owe(&self, _: &[String]) -> Result<()> {
        no_secret()
    }
    fn orphaned(&self) -> Result<BTreeSet<String>> {
        no_secret()
    }
    fn paid(&self, _: &BTreeSet<String>) -> Result<()> {
        no_secret()
    }
    fn cleared(&self, _: &BTreeSet<String>) -> Result<()> {
        no_secret()
    }
}

impl Handle {
    /// Roll this handle's family to its next generation (module docs), or with `apply` false,
    /// say what a roll would write and write nothing. Run again on a Mac that holds records of a
    /// sealed generation its next generation does not carry, it restates those first. `now_ms`
    /// stamps the cover row. The handle is one made at the family's first generation
    /// ([`Self::at`]).
    ///
    /// # Errors
    /// The family declares a secret column (`alab::roll` is its roll); no Commons writer; this
    /// Mac holds no table to roll, or its history of either generation is incomplete; this
    /// build compiles no later generation, or one that declares no `roll` collection; the
    /// next generation does not declare an owned or log collection of this one as the same
    /// kind; a register is too large to carry; the
    /// restatement passes this Mac's writer share of the next generation; or a record cannot be
    /// written.
    pub fn roll(&self, apply: bool, now_ms: u64) -> Result<Rolled> {
        let first = self.family;
        if !super::keeps_records(first) {
            return Err(Error::new(&format!(
                "{} declares a secret column: its roll restates its history and releases its \
                 sealed generation (`aterm db alab roll`)",
                first.name()
            )));
        }
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(&self.paths, deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        session.writer()?;
        let directory = Directory::new(&paths, bootstrap.trust());
        let snapshot = directory.snapshot(&session)?;
        let chain = first.chain();
        let current = super::current_in(&snapshot, bootstrap.trust(), &chain);
        // A sealed generation first, oldest first: what reached it late is carried before the
        // current one rolls.
        for &from in chain
            .iter()
            .filter(|compiled| compiled.generation <= current.generation)
        {
            let Some(to) = from.successor else {
                break;
            };
            let step = Step {
                session: &session,
                directory: &directory,
                from,
                to,
                fresh: from.generation == current.generation,
                said: self.said.as_deref(),
            };
            if let Some(rolled) = step.run(&snapshot, apply, now_ms)? {
                return Ok(rolled);
            }
        }
        Err(Error::new(&format!(
            "this build compiles no generation of {} after the current one",
            first.name()
        )))
    }

    /// Declare, as this Mac, each later generation of this handle's family that another Mac
    /// declared and this one has not ([`endorse`]): what a roll's seal waits for. Whether a
    /// declaration was written. A claims write does the same for every registered family.
    ///
    /// # Errors
    /// No Commons writer here, or the catalog cannot be read.
    pub fn endorse(&self) -> Result<bool> {
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(&self.paths, deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        session.writer()?;
        let directory = Directory::new(&paths, bootstrap.trust());
        let snapshot = directory.snapshot(&session)?;
        Ok(endorse_family(&session, &directory, &snapshot, self.family))
    }
}

/// Declare, as this Mac, each later generation of `first`'s family that `snapshot` shows
/// another Mac declared and this one has not (`Directory::endorse`), where the family keeps its
/// records: this Mac's word that it runs a build that compiles that generation. Whether one was
/// written; a declaration that cannot be written now is written by a later pass.
fn endorse_family(
    session: &Session<'_>,
    directory: &Directory,
    snapshot: &Snapshot,
    first: &'static Compiled,
) -> bool {
    if !super::keeps_records(first) {
        return false;
    }
    let mut endorsed = false;
    for compiled in first.chain().into_iter().skip(1) {
        let Ok(descriptor) = compiled.descriptor(directory.trust()) else {
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
        endorsed |= directory.endorse(session, &descriptor).unwrap_or(false);
    }
    endorsed
}

/// [`Handle::endorse`] for every family this build registers that keeps its records, on the
/// writer pass a claims write makes beside the registry's own (`alab::roll::endorse`).
/// Housekeeping, so it never fails its caller.
pub(crate) fn endorse(session: &Session<'_>, directory: &Directory, snapshot: &Snapshot) -> bool {
    let mut endorsed = false;
    for first in super::families() {
        endorsed |= endorse_family(session, directory, snapshot, first);
    }
    endorsed
}

/// One generation's restatement into the next.
struct Step<'a> {
    session: &'a Session<'a>,
    directory: &'a Directory,
    from: &'static Compiled,
    to: &'static Compiled,
    /// `from` is the family's current generation: this roll declares `to` and seals `from`.
    /// Else `from` is sealed already, and only what reached it late is carried.
    fresh: bool,
    said: Option<&'a Said>,
}

impl Step<'_> {
    /// The records and bytes `rows` take of this Mac's writer share of the next generation,
    /// packed and encoded as [`Self::run`] writes them, each with what it says of who rolled.
    ///
    /// # Errors
    /// A row is not the next generation's, or passes one record's limits.
    fn charge(&self, rows: &[Vec<Change>]) -> Result<(usize, usize)> {
        let mut rows = rows.to_vec();
        for row in &mut rows {
            super::conform(self.to.table, row)?;
        }
        let said = self.to.carries(self.said);
        let records = pack_saying(rows, said.as_deref())?;
        let mut bytes = 0;
        for changes in &records {
            let command = Command::Write {
                request: [0; 16],
                changes: changes.clone(),
                said: said.clone(),
            };
            bytes += command.encode()?.len() + SIGNED;
        }
        Ok((records.len(), bytes))
    }

    /// Plan the step and, with `apply`, write it. `None` where `from` is sealed and its next
    /// generation carries everything this Mac holds of it, or this Mac holds none of it.
    fn run(&self, snapshot: &Snapshot, apply: bool, now_ms: u64) -> Result<Option<Rolled>> {
        let Self {
            session,
            directory,
            from,
            to,
            fresh,
            said,
        } = *self;
        let trust = directory.trust();
        let old_scope = from.descriptor(trust)?.scope;
        let Some(old) = directory.observe_store_if_exists(session, old_scope)? else {
            if fresh {
                return Err(Error::new(&format!(
                    "this Mac holds no {} to roll",
                    from.name()
                )));
            }
            return Ok(None);
        };
        let old_log = Log::read_with(session, &old, old_scope, from.decoder())?;
        if !old_log.awaited(session.actor()).is_empty() {
            return Err(Error::new(&format!(
                "this Mac's history of {} is incomplete; a roll waits for a complete scan",
                from.name()
            )));
        }
        let old_view = View::read(from.table, session, &old_log)?;
        if !old_view.complete {
            return Err(Error::new(&format!(
                "{} is itself a restatement still arriving here; a roll waits for its cover",
                from.name()
            )));
        }
        old_view.require_roll_source(from.table)?;
        // An owned row or a log entry stays in the sealed log, where the next generation's
        // views read it through their own declaration of its collection (`super::kept`).
        if let Some(lost) = from.table.collections.iter().find(|collection| {
            collection.kind != typed::Kind::Cells
                && to
                    .table
                    .collection(collection.name)
                    .is_none_or(|next| next.kind != collection.kind)
        }) {
            return Err(Error::new(&format!(
                "{} does not declare `{}` as {} does: no view of it would read the rows the \
                 sealed generation keeps there; this build cannot roll into it",
                to.name(),
                lost.name,
                from.name()
            )));
        }
        if to.table.collection(ROLL.name).is_none() {
            return Err(Error::new(&format!(
                "{} declares no `roll` collection, so no reader could tell its restatement \
                 whole from one still arriving; this build cannot roll into it",
                to.name()
            )));
        }
        let old_claims = old_view.claim_provenance(from.table)?;
        let old_snapshot = old_log.snapshot();
        let held_ops = held_of_ops(&old_snapshot.operations);
        let held: BTreeSet<Digest> = old_snapshot
            .operations
            .iter()
            .map(|op| op.header.id)
            .collect();
        let frontier = frontier(&held_ops);
        let to_descriptor = to.descriptor(trust)?;
        let observed = directory.observe_store_if_exists(session, to_descriptor.scope)?;
        let uuid = session
            .members()?
            .iter()
            .find(|binding| binding.actor() == session.actor())
            .map(|binding| keys::uuid32(binding.machine()))
            .ok_or_else(|| Error::new("this Mac is not in its own Commons roster"))?;

        // The rows to write, the registers among them, and whether the cover changed.
        let plan = |new: Option<&Store>| -> Result<(Vec<Vec<Change>>, usize, bool)> {
            let (target, carried, rolls, target_claims, used) = match new {
                Some(store) => {
                    let log = Log::read_with(session, store, to_descriptor.scope, to.decoder())?;
                    if !log.awaited(session.actor()).is_empty() {
                        return Err(Error::new(&format!(
                            "this Mac's history of {} is incomplete; a roll waits for a \
                             complete scan",
                            to.name()
                        )));
                    }
                    let view = View::read(to.table, session, &log)?;
                    let carried = carried_origins(log.eligible_iter().map(|(_, command)| command));
                    let rolls = view.project(&ROLL);
                    let claims = view.claim_provenance(to.table)?;
                    let used = store.retained_use(session, session.actor())?;
                    (view.cells, carried, rolls, claims, used)
                }
                None => Default::default(),
            };
            let mut claims = target_claims;
            typed::claim::merge_claims(&mut claims, old_claims.clone())?;
            let restated = restate_with_claims(
                (from.table, to.table),
                &old_view.cells,
                &held,
                &target,
                &carried,
                (&NoSecret, None, &NoSecret),
                &claims,
            )?;
            if !restated.puts.is_empty() || !restated.drops.is_empty() {
                return no_secret();
            }
            let mut rows = restated.rows;
            let registers = rows.len();
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
            // Every record is this Mac's own: the roll is refused here, before anything is
            // declared, where they pass a record's limits or this Mac's writer share of the
            // next generation.
            let (records, bytes) = self.charge(&rows)?;
            if used.writer_records.saturating_add(records) > WRITER_RECORDS
                || used.writer_bytes.saturating_add(bytes) > WRITER_BYTES
            {
                return Err(Error::new(&format!(
                    "this roll passes what this Mac may write into {}; nothing was declared",
                    to.name()
                )));
            }
            Ok((rows, registers, changed))
        };

        // The plan runs on what this Mac holds before anything is declared, so a roll it
        // refuses leaves no declaration of the next generation.
        let (planned, registers, cover) = plan(observed.as_ref())?;
        if !fresh && registers == 0 {
            return Ok(None);
        }
        let mut rolled = Rolled {
            from: from.generation,
            to: to.generation,
            registers,
            cover,
            ..Rolled::default()
        };
        if !apply {
            if fresh {
                // This Mac declares the next generation when it applies.
                rolled.waiting = Directory::undeclared(session, snapshot, &to_descriptor)?;
                rolled
                    .waiting
                    .retain(|machine| keys::uuid32(machine) != uuid);
            }
            return Ok(Some(rolled));
        }
        drop(planned);
        drop(observed);
        if fresh {
            directory.publish(session, &to_descriptor)?;
            directory.endorse(session, &to_descriptor)?;
        }
        // A write the old generation took after it was read is not in the plan: the roll is run
        // again.
        let reread = Log::read_with(session, &old, old_scope, from.decoder())?.snapshot();
        if reread
            .operations
            .iter()
            .any(|op| !held.contains(&op.header.id))
        {
            return Err(Error::new(&format!(
                "{} changed while the roll read it; run the roll again",
                from.name()
            )));
        }
        // A sealed generation's next one is written where this Mac stores it, sealed or not: a
        // record written into a sealed generation is one that reached it late, which the next
        // run carries on.
        let path = directory.path(to_descriptor.scope)?;
        if !fresh && !path.try_exists()? {
            return Err(Error::new(&format!(
                "this Mac does not hold {} yet; a repair brings it",
                to.name()
            )));
        }
        let mut store = Store::open(session, &path, to_descriptor.scope)?;
        let (mut rows, registers, cover) = plan(Some(&store))?;
        (rolled.registers, rolled.cover) = (registers, cover);
        for row in &mut rows {
            super::conform(to.table, row)?;
        }
        let said = to.carries(said);
        // The cover goes last, alone: it says the records before it are there.
        let (cover_rows, body): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|row| row.first().is_some_and(|c| c.collection == ROLL.name));
        let mut records = pack_saying(body, said.as_deref())?;
        records.extend(pack_saying(cover_rows, said.as_deref())?);
        let mut log = Log::read_with(session, &store, to_descriptor.scope, to.decoder())?;
        let role = request_role(to.table.role, &log);
        let mut appended = BTreeMap::new();
        let write = |log: &Log,
                     store: &mut Store,
                     role: &str,
                     appended: &mut BTreeMap<[u8; 16], (Digest, Command)>,
                     changes: Vec<Change>|
         -> Result<bool> {
            let request = super::said_request(
                typed::request_id(to.table.application, role, &changes)?,
                said.as_deref(),
            )?;
            let command = Command::Write {
                request,
                changes,
                said: said.clone(),
            };
            to.admits(&command)?;
            if super::retry_or_appended(log, session.actor(), &command, appended)?.is_some() {
                return Ok(false);
            }
            let receipt = application::append(session, store, &command.encode()?)?;
            appended.insert(request, (receipt.id, command));
            Ok(true)
        };
        for changes in records {
            if write(&log, &mut store, &role, &mut appended, changes)? {
                rolled.records += 1;
            }
        }
        if old_view.ops.iter().any(|op| {
            op.changes
                .iter()
                .any(|change| change.collection == typed::SCHEMA)
        }) {
            if rolled.records > 0 {
                log = Log::read_with(session, &store, to_descriptor.scope, to.decoder())?;
                appended.clear();
            }
            let schema = super::schema_rows(to.table, &View::read(to.table, session, &log)?);
            for changes in pack_saying(schema, said.as_deref())? {
                if write(&log, &mut store, to.table.role, &mut appended, changes)? {
                    rolled.records += 1;
                }
            }
        }
        drop(log);
        drop(store);
        // A build that does not compile the next generation cannot read the family once the old
        // one is sealed, so the seal waits until every current Writer has declared the next
        // generation itself, which only a build that compiles it does.
        if fresh {
            rolled.waiting =
                Directory::undeclared(session, &directory.snapshot(session)?, &to_descriptor)?;
            if rolled.waiting.is_empty() {
                rolled.sealed = directory.seal(session, &from.descriptor(trust)?)?;
            }
        }
        Ok(Some(rolled))
    }
}
