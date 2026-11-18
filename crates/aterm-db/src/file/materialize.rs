// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The materializer (docs/DESIGN-shared-files-2026-11-11.md §4): one pass projects this Mac's
//! replica of each shared file to its snapshot and face, and records the state word the pass
//! ended in. It is the only code that opens custody for a shared file, and it runs in two
//! places: the process that owns the Commons runtime, shortly after a store grows and once a
//! minute besides (`crate::alab::upkeep`), and `aterm file refresh`.
//!
//! A pass authors nothing, ingests nothing and persists no receipt: it opens the home
//! [`crate::generation::materializer_generation_until`] names as an observer, reads each table
//! family and the journal ring's window once, and writes only under `~/.aterm/files`. That walk and the check it opens under
//! ([`crate::generation::Observation::check_materialize`]) admit homes no other reader may
//! open, so both take a [`Projecting`], which only this module can make.
//!
//! - **Per table.** Each file's table is read on its own, so one that refuses (excluded,
//!   undeclared, damaged) holds back no file that does not read it. `ai/limits` reads two (the
//!   journal ring, whose `ai/usage` rows it folds, and the registry it names accounts and seats
//!   by) and refuses with either rather than fold without what that one says. A file that reads
//!   the session's roster (`lab/machines`, for `=here`; a view, for the members) refuses where
//!   the roster did not read, and the files that read none (`ai/accounts`, `ai/links`,
//!   `lab/tracker`'s logs and the journal ring's) go on.
//! - **Every refusal has a word.** Whatever stops a pass (`[commons]` policy, no membership, a
//!   busy vault, missing keys, an undeclared table, damaged storage, a writer generation that
//!   moved while the pass read) leaves the snapshot as it was and records `held` or `off` with
//!   the reason in `state.json`; nothing here returns an error a reader could meet.
//! - **Monotone.** [`aterm_file::File::record`] replaces a snapshot only by one projected from a
//!   frontier that contains the old one, or from a later home or table generation, and records
//!   nothing for a pass another pass overtook. The rule orders reads of one replica: a read of
//!   another (this Mac joined another lab, or enrolled again as a new Mac) replaces the
//!   snapshot outright. A read that is behind the snapshot is recorded `held`, since the rows a
//!   reader gets are not that read's. The rule fails closed: where the snapshot's frontier
//!   file is missing or does not read, a fenced home's read is behind it.
//! - **Held, never live.** A home fenced by a succession of its own is projected and recorded
//!   `held` (`succession`), whichever way that succession stands: its import has never
//!   completed, so the home still holds every record of its lineage as of the fence
//!   ([`Standing::Preserving`]); or it completed and a later source receipt invalidated the
//!   completion, or the home is fenced and holds no selection to say which
//!   ([`Standing::Unsettled`]). A home is fenced from the first file its reservation writes,
//!   before any selection is there, and stays fenced where the selection is lost. The home's
//!   standing is read before custody is opened and again once every table is read, so a fence
//!   that falls, or a completion that is invalidated, while a pass reads is recorded by that
//!   pass.
//! - **What an unsettled home's rows can mislead.** Its successor was complete for a while: it
//!   may have authored records, and taken peers', that the fenced home never holds, and the
//!   fenced home may hold a record its successor has not imported. So an unsettled snapshot can
//!   lack this Mac's own newest writes and can show a record no live home carries yet. Three
//!   rules bound that. It is never `live`. It never replaces a snapshot taken at the
//!   successor's home, since an earlier home never replaces a later one's (the monotone rule),
//!   so a reader who saw the successor's rows keeps them; where no pass ever read the
//!   successor, what it shows contains every record the last snapshot of this home showed.
//!   And nothing is authored from it: a write made against its rows is published only where
//!   the live home's register still holds exactly what the write observed (slice SF-05,
//!   `aterm_db_core::file_merge::settle`). The `SharedFile` model walks it
//!   (`aterm_models_db::shared_file_model`: `Invalidate`, `Materialize`, `lacks`), and
//!   `super::model_tests` holds this module to that walk.
//! - **Projected where its key moved.** A pass projects a file only where the key of what its
//!   projection reads differs from the key the status records the snapshot at, or the history
//!   beside it is not the one it wrote, unless it is asked to ([`Pass::again`]). A catalog
//!   table's file reads the frontier, the log's judgment of each record, what the carried
//!   records and writers resolve to, this Mac's machine, this build's rules and the done
//!   markers' provenance; a file of the journal ring, the window's frontier, each week's
//!   judgment, what its writers resolve to, the rules, and which rows its snapshot shows past
//!   their span; a view, its frontier (the registry's read it names accounts and seats by),
//!   what fixes that read's view, the `ai/usage` snapshot it folds, whether the ring is
//!   complete, the current members and the rules. So an ordinary pass leaves what `aterm file
//!   refresh` would, but past one bound: where a ring file's rows take more than
//!   [`retain`]'s byte budget, which of them a projection leaves out turns on this Mac's clock
//!   (a row the window holds that the last projection left out is taken as seen now), which no
//!   key holds, so a pass at rest keeps the rows the last projection left and a refresh may
//!   leave out others. A pass at rest projects nothing. A snapshot kept for a reason nothing
//!   has moved since (a read behind it, a projection too large at this key) is not projected
//!   again either.
//! - **Nothing written at rest.** A pass whose read leaves what the files already say writes
//!   nothing ([`aterm_file::Recorded::Settled`]), the status included, unless another pass looked
//!   and has not recorded: then it moves the count of passes alone
//!   ([`aterm_file::Recorded::Fenced`]), so that pass, whose read may be the older, records
//!   nothing.
//! - **No secret.** A file has no `Secret` column ([`super::Type::check`]). A cell whose value
//!   this build's guards refuse (shaped like a credential, naming a secret) shows the grid's
//!   refusal text and has no typed value, and a sealed value in a column a newer build wrote is
//!   left out; both are counted in the status. A computed cell is held to the same guards: what
//!   it takes from another row (a machine's label) is taken in the spelling the grid shows, and
//!   every computed text is checked once more before it is written.

use super::{
    Computed, Store, Type,
    limits::{self, Reading, Seats},
    types::{TABLE, TSV},
};
use crate::{
    Error,
    alab::{
        account_names,
        journal::{self, Event},
        registry,
        view::{LabView, MachineNickname},
    },
    att::{self, Compiled, View},
    catalog::{Directory, State, policy::Policy, ring},
    documents::log::Log,
    generation::{self, Standing},
    table::Session,
    typed::{
        Collection, Kind, Ty,
        cell::{self, Field, Projection},
        grid, keys,
        owned::{self, LogRead, Op, OwnedRead},
        secret,
        view::Projections,
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_file::{
    AccountNames, Column, File, Frontier, Kept, Look, Name, Opened, Projected, Recorded, Row,
    Snapshot, Source, Why, Word, retain,
};
use aterm_json::{Map, Value};
use aterm_secrets::{
    client::Paths,
    commons::{self, Observation, Unavailable, bootstrap::Bootstrap},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, Instant},
};

/// Proof that a caller is this module, the shared-file materializer: the one reader allowed
/// through [`generation::materializer_generation_until`] and
/// [`generation::Observation::check_materialize`], which name and admit a home whose
/// succession was completed and then invalidated. The field is private, so no other module can
/// make one; what is made here goes only to those two and to
/// [`generation::observe_materializer`].
pub(crate) struct Projecting(());

#[cfg(test)]
impl Projecting {
    /// The proof, for a test of the walk and the check themselves.
    pub(crate) const fn in_test() -> Self {
        Self(())
    }
}

/// How long a pass waits for a vault another program is writing before it records `held`
/// (`busy`).
pub const VAULT_WAIT: Duration = Duration::from_secs(2);

/// What bounds one pass.
#[derive(Clone, Copy, Debug)]
pub struct Pass {
    /// When the pass stops reading.
    pub read_by: Instant,
    /// How long it waits for a busy vault.
    pub vault_wait: Duration,
    /// Project again though the key says nothing the projection reads has moved: `aterm file
    /// refresh`, a person's word that the snapshot is to be made again.
    pub again: bool,
    /// This Mac's clock, Unix milliseconds, for display.
    pub now_ms: u64,
}

impl Pass {
    /// An ordinary pass starting now, which projects only where the key moved.
    #[must_use]
    pub fn now() -> Self {
        Self {
            read_by: crate::admission::deadline(),
            vault_wait: VAULT_WAIT,
            again: false,
            now_ms: aterm_time::unix_millis().unwrap_or(0),
        }
    }
}

/// What one pass did for one file.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub name: Name,
    /// The word the pass ended in for this file.
    pub word: Word,
    pub why: Option<Why>,
    /// What refused, in the refusing layer's own words (never a value).
    pub detail: Option<String>,
    /// What recording did; `None` where a local file could not be written, or the file's look
    /// lock could not be taken (`problem`).
    pub recorded: Option<Recorded>,
    pub problem: Option<String>,
}

/// Why a pass stopped, for every file or for one.
#[derive(Clone, Debug)]
struct Refusal {
    word: Word,
    why: Why,
    detail: String,
}

impl Refusal {
    fn held(why: Why, detail: impl ToString) -> Self {
        Self {
            word: Word::Held,
            why,
            detail: detail.to_string(),
        }
    }

    fn off(detail: &str) -> Self {
        Self {
            word: Word::Off,
            why: Why::Policy,
            detail: detail.to_owned(),
        }
    }

    /// `error` under the word of the stage it stopped: a local holder past the wait is `busy`,
    /// a membership view that lacks the record's grant is `membership`, and a check that ran
    /// out of time before it learned anything is `custody` (it met no holder and judged no
    /// stage), at any stage.
    fn of(error: &Error, stage: Why) -> Self {
        let why = if error.is_busy() {
            Why::Busy
        } else if error.is_awaiting_membership() || error.is_enrollment_pending() {
            Why::Membership
        } else if error.is_out_of_time() {
            Why::Custody
        } else {
            stage
        };
        Self::held(why, error)
    }
}

/// The word of a vault that shows this Mac no Commons.
const fn unavailable(reason: Unavailable) -> Why {
    match reason {
        Unavailable::Busy => Why::Busy,
        Unavailable::Unconfigured
        | Unavailable::NotAdmitted
        | Unavailable::Revoked
        | Unavailable::Dropped
        | Unavailable::ControlExcluded
        | Unavailable::ControlNotVisible => Why::Membership,
        Unavailable::UnsignedSource | Unavailable::UntrustedPin | Unavailable::MissingIdentity => {
            Why::Keys
        }
        Unavailable::IncompleteApply
        | Unavailable::SourceChanged
        | Unavailable::MissingStateLock
        | Unavailable::MissingCache
        | Unavailable::PendingApply => Why::Custody,
    }
}

/// The open custody one pass reads through.
struct Custody<'a> {
    session: &'a Session<'a>,
    directory: Directory,
    catalog: crate::catalog::Snapshot,
    policy: Policy,
    era: u64,
    /// The actor this Mac's writer lineage began as, lower-case hex.
    origin: String,
    /// Who this Mac and every current member are, or why the session's roster did not read:
    /// each file that reads them refuses with that (`lab/machines` reads `=here`,
    /// [`Computed::reads_here`], and a view the members), and the files that read neither
    /// (`ai/accounts`, `ai/links` and the journal ring's) go on.
    roster: Result<Roster, Refusal>,
}

/// Who this Mac and every current member are, as the session's roster names them.
#[derive(Clone, Debug)]
struct Roster {
    /// The Commons machine uuid (32 hex) of the actor this pass reads as, where the members name
    /// it: the facts row at that key binds this Mac's machine row (`=here`).
    me: Option<String>,
    /// Every current member's Commons machine uuid (32 hex): a fenced predecessor's reading
    /// gives way to its successor's in a fold (`super::limits::Seats`).
    members: BTreeSet<String>,
}

/// How the records of `paths`' writer lineage stand for a reader that opened custody itself (a
/// registry view of a noun that has no shared file yet): `live` only where the home is judged
/// the newest of its lineage, `held` where a succession fences it, unfinished or invalidated,
/// and `held` too where the standing could not be read, since a fence not judged is not ruled
/// out.
pub(crate) fn standing(paths: &Paths, read_by: Instant) -> (Word, Option<Why>) {
    let walk = generation::materializer_generation_until(
        &generation::original(paths),
        read_by,
        &Projecting(()),
    );
    match walk {
        Ok(held) if held.standing == Standing::Newest => (Word::Live, None),
        Ok(_) => (Word::Held, Some(Why::Succession)),
        Err(error) => (Word::Held, Some(Refusal::of(&error, Why::Succession).why)),
    }
}

/// Open this Mac's custody as an observer and run `action` in it, or say what refused. With
/// what `action` read comes whether the home was the newest of its lineage from before custody
/// opened until `action` returned: a snapshot of what it read is `live` only then.
fn with_custody<T>(
    home: &Paths,
    config: &Path,
    pass: &Pass,
    action: impl FnOnce(&Custody<'_>) -> T,
) -> Result<(T, bool), Refusal> {
    let policy = Policy::read(config).map_err(|error| Refusal::held(Why::Policy, error))?;
    if !policy.enabled {
        return Err(Refusal::off("[commons] turns sharing off on this Mac"));
    }
    // Each wait for a vault or a store another program holds lasts only `vault_wait`: a pass
    // that meets one records `held` (`busy`) and leaves, rather than wait out a reader's bound.
    // The writer-generation walks read files and wait on no holder, so `read_by` alone bounds
    // them, as it bounds `observe_materializer`'s own reads: a slow walk is not a busy vault.
    let wait = || (Instant::now() + pass.vault_wait).min(pass.read_by);
    let by = Projecting(());
    let held = generation::materializer_generation_until(home, pass.read_by, &by)
        .map_err(|error| Refusal::of(&error, Why::Succession))?;
    #[cfg(test)]
    super::seam::standing_read();
    // The vault says whether this Mac is a member before any custody is opened.
    match commons::observe_until_settled(&held.paths, wait()) {
        Ok(Observation::Visible(_)) => {}
        Ok(Observation::Unavailable(reason)) => {
            return Err(Refusal::held(
                unavailable(reason),
                format!("{}: {}", reason.code(), reason.explanation()),
            ));
        }
        Err(error) => return Err(Refusal::of(&crate::table::failure(error), Why::Custody)),
    }
    let (bootstrap, journal) = Bootstrap::established_with_journal_until(&held.paths, wait())
        .map_err(|error| Refusal::of(&crate::table::failure(error), Why::Custody))?;
    let selection = generation::observe_materializer(bootstrap.paths(), pass.read_by, &wait, &by)
        .map_err(|error| Refusal::of(&error, Why::Succession))?;
    let session = Session::observe_selected(&bootstrap, journal, selection, pass.read_by)
        .map_err(|error| Refusal::of(&error, Why::Custody))?;
    let directory = Directory::new(session.paths(), session.trust());
    let catalog = directory
        .snapshot_existing(&session)
        .map_err(|error| Refusal::of(&error, Why::Catalog))?;
    // Who is a member now decides `=here` and which of one Mac's two seats a view folds the
    // reading of: a roster that did not read leaves this Mac's machine and the members unknown,
    // never absent, so each file that reads them refuses rather than record `here` blank or
    // fold a current member's reading as a predecessor's (`Custody::roster`).
    let actor = session.actor();
    let bindings = session.members();
    #[cfg(test)]
    let bindings = super::seam::roster(bindings);
    let roster = bindings
        .map_err(|error| Refusal::of(&error, Why::Membership))
        .map(|bindings| {
            let me = bindings
                .iter()
                .find(|binding| binding.actor() == actor)
                .map(|binding| keys::uuid32(binding.machine()));
            #[cfg(test)]
            let me = super::seam::bound()
                .map(|machine| keys::uuid32(&machine))
                .or(me);
            // Every current member, from the roster `me` was found in (`super::limits::Seats`).
            let members = bindings
                .iter()
                .map(|binding| keys::uuid32(binding.machine()));
            #[cfg(test)]
            let members = members
                .filter(|uuid| super::seam::left().is_none_or(|left| *uuid != keys::uuid32(&left)));
            Roster {
                me,
                members: members.collect(),
            }
        });
    let read = action(&Custody {
        roster,
        session: &session,
        directory,
        catalog,
        policy,
        era: held.era,
        origin: held
            .origin
            .clone()
            .unwrap_or_else(|| aterm_codec::hex::encode_lower(&session.actor())),
    });
    // The standing again, now that every table is read: the generation check the session
    // opened under admits a home whose own succession is reserved and unfinished, or completed
    // and invalidated, so a fence that fell since the first look is seen only here. A home that
    // is no longer the one this lineage names (its succession completed, or its predecessor's
    // completion was invalidated) is not projected at all.
    let after = generation::materializer_generation_until(home, pass.read_by, &by)
        .map_err(|error| Refusal::of(&error, Why::Succession))?;
    if after.paths != held.paths {
        return Err(Refusal::held(
            Why::Succession,
            "this Mac's writer generation moved while the pass read it",
        ));
    }
    let newest = held.standing == Standing::Newest && after.standing == Standing::Newest;
    Ok((read, newest))
}

/// One table family as one pass read it: everything a projection of its files reads from
/// custody, owned, so the projection runs once custody is closed and reads no session.
struct Family {
    compiled: &'static Compiled,
    frontier: Frontier,
    log: Log,
    resolved: att::Resolved,
    view: std::cell::OnceCell<View>,
}

impl Family {
    /// The generation of `first`'s family this Mac's catalog says to read, opened and read, with
    /// what its view resolves through the session ([`att::Resolved::read`]): a read that
    /// refuses refuses each file of the family, `custody`'s, or busy or membership's as its
    /// error says ([`Refusal::of`]).
    fn read(custody: &Custody<'_>, first: &'static Compiled) -> Result<Self, Refusal> {
        let trust = custody.session.trust();
        let compiled = att::current_in(&custody.catalog, trust, &first.chain());
        let descriptor = compiled
            .descriptor(trust)
            .map_err(|error| Refusal::of(&error, Why::Catalog))?;
        let table = descriptor.scope.digest();
        // Which replica this is: the family's first table under this trust root, which stays
        // through a roll to a later table generation, and the actor the lineage began as, which
        // stays through a succession and changes when this Mac enrolls again as a new one.
        let family = first
            .descriptor(trust)
            .map_err(|error| Refusal::of(&error, Why::Catalog))?
            .scope
            .digest();
        let mut source = Sha256::new();
        source.update(b"aterm/file/source/1\0");
        source.update(family);
        source.update(custody.origin.as_bytes());
        if !custody
            .policy
            .includes(custody.directory.catalog_scope().digest(), table)
        {
            return Err(Refusal::off("[commons] excludes this table on this Mac"));
        }
        let Some(entry) = custody.catalog.entries.get(&table) else {
            return Err(Refusal::held(
                Why::Catalog,
                "this Mac's catalog does not declare the table yet",
            ));
        };
        if entry.state != State::Subscribed
            || entry
                .declarations
                .iter()
                .any(|declaration| declaration.descriptor != descriptor)
        {
            return Err(Refusal::held(
                Why::Catalog,
                "this Mac's catalog declares the table with a descriptor this build cannot use",
            ));
        }
        let store = custody
            .directory
            .observe_table(custody.session, descriptor.scope)
            .map_err(|error| Refusal::of(&error, Why::Custody))?;
        let log = Log::read_with(custody.session, &store, descriptor.scope, att::decode)
            .map_err(|error| Refusal::of(&error, Why::Custody))?;
        let resolved = att::Resolved::read(compiled.table, custody.session, &log)
            .map_err(|error| Refusal::of(&error, Why::Custody))?;
        Ok(Self {
            compiled,
            frontier: Frontier {
                source: source.finalize(),
                era: custody.era,
                generation: compiled.generation,
                ids: log.retained_ids().copied().collect(),
            },
            log,
            resolved,
            view: std::cell::OnceCell::new(),
        })
    }

    /// The family's table view, made once a pass ([`View::of`]) for every file of the family a
    /// pass projects.
    fn view(&self) -> &View {
        self.view.get_or_init(|| {
            #[cfg(test)]
            PROJECTIONS.set(PROJECTIONS.get() + 1);
            View::of(self.compiled.table, &self.log, &self.resolved)
        })
    }
}

/// One family as one pass projects its files: what it read, and the collections of its view,
/// each projected once ([`Projections`]) for every file of the family the pass projects, and
/// only where one is projected.
struct Projector<'f> {
    family: &'f Family,
    projections: std::cell::OnceCell<Projections<'f>>,
}

impl<'f> Projector<'f> {
    const fn new(family: &'f Family) -> Self {
        Self {
            family,
            projections: std::cell::OnceCell::new(),
        }
    }

    /// The projections of the family's view, the view made at the first ask ([`Family::view`]).
    fn projections(&self) -> &Projections<'f> {
        self.projections
            .get_or_init(|| Projections::new(self.family.view()))
    }
}

#[cfg(test)]
thread_local! {
    /// How many table views this thread's passes made to project from ([`View::of`], once per
    /// family a pass projects), counted (never timed): the `files-idle` lane's drive
    /// (`crate::alab::upkeep`'s tests) reads it before and after each pass.
    pub(crate) static PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The lab journal's week ring as one pass read it: the window's tables by this Mac's clock,
/// their eligible operations, and the frontier a file of the ring was projected from.
///
/// The ring has no catalog entry: its tables are named by the ring and the week, outside the
/// catalog (`catalog::ring`), so a pass checks only that `[commons]` includes each one. Its
/// frontier orders reads of one replica by the window's first week, then by the records the
/// window's tables retained: a window that moved on to a later week replaces a snapshot
/// whatever it no longer holds (its rows stay in the snapshot, `aterm_file::retain`), and a
/// clock set back a week reads as behind.
///
/// Like a [`Family`], it holds everything a projection of its files reads from custody, owned,
/// so the projection runs once custody is closed and reads no session.
struct Ring {
    frontier: Frontier,
    ops: Vec<Op>,
    /// When each operation says it was authored, Unix milliseconds: for the history only.
    authored: BTreeMap<Digest, u64>,
    /// No retained record could still become eligible.
    complete: bool,
    /// Each week table's [`Log::judgment`], in the window's order: with the frontier, it fixes
    /// which records are eligible and how each is judged.
    judgments: Vec<[u8; 32]>,
    /// The machine each (roster, writer) of an eligible operation held a Writer seat as in that
    /// roster, `None` where it held none ([`Session::held_binding`]): the owned rule's reading
    /// of whose each row is.
    bindings: BTreeMap<(Digest, Actor), Option<[u8; 16]>>,
}

impl Ring {
    /// The window's tables at `now_s` (Unix seconds, this Mac's clock), opened and read, with
    /// the machine each eligible operation's writer resolves to through the session: a writer
    /// whose roster the session does not hold refuses the read (`custody`'s, or busy or
    /// membership's as its error says, [`Refusal::of`]) rather than leave its rows out.
    fn read(custody: &Custody<'_>, now_s: u64) -> Result<Self, Refusal> {
        let trust = custody.session.trust();
        let state = custody.directory.ring_view(custody.session, &ring::JOURNAL);
        let window = state.window(&ring::JOURNAL, now_s);
        let catalog = custody.directory.catalog_scope().digest();
        let mut scopes = Vec::new();
        for epoch in window.clone() {
            let scope = ring::JOURNAL
                .scope(trust, epoch)
                .map_err(|error| Refusal::of(&error, Why::Catalog))?;
            if !custody.policy.includes(catalog, scope.digest()) {
                return Err(Refusal::off(
                    "[commons] excludes the journal ring's tables on this Mac",
                ));
            }
            scopes.push((epoch, scope));
        }
        let mut source = Sha256::new();
        source.update(b"aterm/file/source/ring/1\0");
        source.update(journal::APPLICATION.as_bytes());
        source.update(b"\0");
        source.update(journal::ROLE.as_bytes());
        source.update(b"\0");
        source.update(catalog);
        source.update(custody.origin.as_bytes());
        let mut ids = BTreeSet::new();
        let mut ops = Vec::new();
        let mut authored = BTreeMap::new();
        let mut complete = true;
        let mut judgments = Vec::new();
        let mut bindings = BTreeMap::new();
        for (epoch, scope) in scopes {
            let Some(store) = custody
                .directory
                .observe_table_if_exists(custody.session, scope)
                .map_err(|error| Refusal::of(&error, Why::Custody))?
            else {
                continue;
            };
            let log = Log::read_with(custody.session, &store, scope, att::decode)
                .map_err(|error| Refusal::of(&error, Why::Custody))?;
            ids.extend(log.retained_ids().copied());
            complete &= log.history_complete();
            judgments.push(log.judgment());
            for (operation, _) in log.eligible_iter() {
                let writer = (operation.roster, operation.header.actor);
                if let std::collections::btree_map::Entry::Vacant(slot) = bindings.entry(writer) {
                    slot.insert(
                        custody
                            .session
                            .held_binding(&writer.0, &writer.1)
                            .map_err(|error| Refusal::of(&error, Why::Custody))?
                            .map(|binding| *binding.machine()),
                    );
                }
            }
            authored.extend(
                log.eligible_iter()
                    .map(|(operation, _)| (operation.header.id, operation.authored_ms)),
            );
            ops.extend(log.into_eligible().filter_map(|(operation, command)| {
                Op::from_eligible_owned(epoch, operation, command)
            }));
        }
        Ok(Self {
            frontier: Frontier {
                source: source.finalize(),
                era: custody.era,
                generation: u32::try_from(*window.start()).unwrap_or(u32::MAX),
                ids,
            },
            ops,
            authored,
            complete,
            judgments,
            bindings,
        })
    }

    /// The machine `actor`'s seat in `roster` binds, as the read resolved it: the owned rule's
    /// `resolve` ([`owned::read_owned`], [`owned::read_log`]).
    fn resolve(&self, roster: &Digest, actor: &Actor) -> Option<[u8; 16]> {
        self.bindings.get(&(*roster, *actor)).copied().flatten()
    }
}

/// The frontier of the view `view` (`ai/limits`), which names each account and binds each seat
/// by `registry`'s read: that read's frontier, under a source of its own, so the view and the
/// registry's files are never taken for one another's reads. It orders the view's reads of the
/// registry as the registry's files' reads are ordered, so a registry read behind the one the
/// view's snapshot was folded from keeps that snapshot (`Kept(Behind)`) rather than name its
/// accounts as an older registry did. The ring reaches the view only as the `ai/usage` snapshot
/// it folds and whether the ring is complete, and that snapshot is ordered by its own file's
/// frontier: a ring read behind it keeps it, and the view folds the one kept.
///
/// An `ai/limits` frontier written before this source (`view/1`, over the ring's) does not
/// compare with it, and a frontier of another source is another replica's read
/// ([`aterm_file::Frontier::admits_over`]): so the first pass after the upgrade replaces that
/// snapshot whether or not this home is the newest, once, and records its own frontier, which
/// every later pass orders as above. A home fenced during an unsettled succession can therefore
/// take that one snapshot (its status says `held`); the newest home's next read replaces it.
fn view_frontier(registry: &Family, view: &str) -> Frontier {
    let mut source = Sha256::new();
    source.update(b"aterm/file/source/view/2\0");
    source.update(registry.frontier.source);
    source.update(view.as_bytes());
    Frontier {
        source: source.finalize(),
        ..registry.frontier.clone()
    }
}

/// A cell's typed value as the snapshot keeps it: the value, a conflict's provisional value, a
/// set's present members (a text set's by their texts, [`cell::members`]). `None` for an empty
/// cell and for one this build's guards refuse.
pub(super) fn typed(column: &crate::typed::Column, field: &Field) -> Option<Value> {
    match field {
        Field::Set(_) => Some(Value::Array(
            cell::members(column, Some(field))
                .into_iter()
                .map(|member| Value::String(member.to_owned()))
                .collect(),
        )),
        register => register
            .value()
            .filter(|x| !x.is_null() && cell::admissible(&column.ty, x))
            .map(grid::raw_shown),
    }
}

fn text<'a>(row: &'a cell::Row, column: &str) -> Option<&'a str> {
    row.get(column).and_then(Value::as_str)
}

/// Every account's names (`alab::account_names`), from the registry's account and address rows
/// as this pass projected them ([`Projections`]): what `ai/accounts`, `ai/links` and the
/// `ai/limits` view name an account by.
fn account_names_of(
    compiled: &Compiled,
    projections: &Projections<'_>,
) -> BTreeMap<String, AccountNames> {
    let accounts = projections.project(&registry::ACCOUNT);
    let addresses = compiled
        .collection(registry::ADDRESS.name)
        .map(|collection| projections.project(collection))
        .unwrap_or_default();
    account_names::of_projected(&accounts, &addresses)
}

/// Each machine's label, by key, as a computed cell may carry it: the stored label where it is
/// one this build's guards admit, in the spelling the grid shows, and the grid's refusal text
/// ([`grid::REFUSED`]) in place of any other, which an older or modified build stored unchecked.
fn machine_labels(compiled: &Compiled, projections: &Projections<'_>) -> BTreeMap<String, String> {
    let machine = compiled
        .collection(registry::MACHINE.name)
        .unwrap_or(&registry::MACHINE);
    let Some(column) = machine.column("label") else {
        return BTreeMap::new();
    };
    projections
        .project(machine)
        .rows
        .iter()
        .filter_map(|(pk, row)| {
            let stored = row.get("label").filter(|x| !x.is_null())?;
            let shown = grid::show(&column.ty, stored);
            let label = if stored.as_str() == Some(shown.as_str()) && guarded(&shown) {
                shown
            } else {
                grid::REFUSED.to_owned()
            };
            Some((pk.clone(), label))
        })
        .collect()
}

/// Whether `text` is one the guards let a surface show: not shaped like a credential, naming a
/// secret or sealed (what [`grid::raw_shown`] replaces).
fn guarded(text: &str) -> bool {
    #[cfg(test)]
    if super::seam::build().is_some_and(|word| text.contains(word.as_str())) {
        return false;
    }
    grid::raw_shown(&Value::from(text)).as_str() == Some(text)
}

/// `text`, a cell as the grid shows it, as this build shows it: the same text, but where a
/// test's later build refuses a stored text the build before showed (`super::seam::new_build`).
fn shown_by_this_build(text: &str) -> String {
    #[cfg(test)]
    if super::seam::build().is_some_and(|word| text.contains(word.as_str())) {
        return grid::REFUSED.to_owned();
    }
    text.to_owned()
}

/// The computed cells of every row of `file`, by key then column, from the other collections of
/// the view `projections` projects.
fn computed(
    file: &Type,
    compiled: &Compiled,
    projections: &Projections<'_>,
    projection: &Projection,
    me: Option<&str>,
) -> BTreeMap<String, BTreeMap<&'static str, String>> {
    let mut out: BTreeMap<String, BTreeMap<&'static str, String>> = BTreeMap::new();
    let mut put = |pk: &str, column: &'static str, value: Option<&String>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            out.entry(pk.to_owned())
                .or_default()
                .insert(column, value.clone());
        }
    };
    match file.computed {
        // A ring file's `=seen` is its snapshot's own (`aterm_file::retain`).
        Computed::None | Computed::Kept => {}
        Computed::Accounts => {
            let accounts = account_names_of(compiled, projections);
            let machines = machine_labels(compiled, projections);
            // Each account's live links on live machines, smallest rank first, unranked last.
            let all = projections.project(&registry::MACHINE);
            let mut links: BTreeMap<&str, Vec<(u64, &str)>> = BTreeMap::new();
            let preferences = projections.project(&registry::PREFERENCE);
            for row in preferences.rows.values().filter(|row| row.live()) {
                let (Some(machine), Some(account)) = (text(row, "machine"), text(row, "account"))
                else {
                    continue;
                };
                if !all.rows.get(machine).is_some_and(cell::Row::live) {
                    continue;
                }
                let rank = row.get("rank").and_then(Value::as_u64).unwrap_or(u64::MAX);
                links
                    .entry(account)
                    .or_default()
                    .push((rank, machines.get(machine).map_or(machine, String::as_str)));
            }
            for pk in projection.rows.keys() {
                let names = accounts.get(pk);
                put(pk, "name", names.and_then(|names| names.name.as_ref()));
                put(pk, "slug", names.map(|names| &names.slug));
                put(pk, "email", names.and_then(|names| names.email.as_ref()));
                put(
                    pk,
                    "login_email",
                    names.and_then(|names| names.login.as_ref()),
                );
                if let Some(mut linked) = links.remove(pk.as_str()) {
                    linked.sort_unstable();
                    let names: Vec<&str> = linked.into_iter().map(|(_, name)| name).collect();
                    put(pk, "links", Some(&names.join(", ")));
                }
            }
        }
        Computed::Links => {
            let accounts = account_names_of(compiled, projections);
            let machines = machine_labels(compiled, projections);
            for (pk, row) in &projection.rows {
                let names = accounts.get(text(row, "account").unwrap_or_default());
                put(pk, "account_slug", names.map(|names| &names.slug));
                put(
                    pk,
                    "account_name",
                    names.and_then(|names| names.name.as_ref()),
                );
                put(
                    pk,
                    "machine_name",
                    machines.get(text(row, "machine").unwrap_or_default()),
                );
            }
        }
        Computed::Machines => {
            let view = projections.view();
            let empty = OwnedRead::default();
            let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
            // The machines file reads the seats, `=here` and the nicknames, no account's names.
            let lab = LabView::read_projected(
                &view.cells,
                &projections.project(&registry::MACHINE),
                BTreeMap::new(),
                owned(registry::MACHINE_FACTS.name),
                owned(registry::MAILBOX_OBS.name),
                me,
            );
            let mut seats: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (uuid, machine) in &lab.bound {
                seats.entry(machine).or_default().push(uuid);
            }
            for (machine, uuids) in seats {
                put(machine, "seats", Some(&uuids.join(", ")));
            }
            let yes = "yes".to_owned();
            if let Some(here) = &lab.me {
                put(here, "here", Some(&yes));
            }
            for (pk, machine) in &lab.machines {
                if matches!(
                    machine.nickname,
                    MachineNickname::Named {
                        automatic: true,
                        ..
                    }
                ) {
                    put(pk, "nickname_auto", Some(&yes));
                }
            }
            for (pk, row) in &projection.rows {
                if row.raw.contains("label") {
                    continue;
                }
                let Some(field) = row.fields.get("label").filter(|field| !field.conflicted())
                else {
                    continue;
                };
                // Only an owner's label is a number a Mac takes (registration's own rule).
                if matches!(field, Field::Value(candidate)
                    if candidate.value.a || candidate.value.s.is_some())
                {
                    put(pk, "label_auto", Some(&yes));
                }
                // The projection reads the claim rule's answer first (`cell::Row::get`).
                let claim = row.get("label").and_then(Value::as_str).map(str::to_owned);
                put(pk, "label_claim", claim.as_ref());
            }
        }
    }
    out
}

/// Project `file` from the view `projections` projects: its snapshot (the revision unset), how
/// many cells were left out for a value the guards refuse, and the baseline a write of this
/// revision is read against (what each face cell showed and each cell's candidates,
/// `super::face::Baseline`).
fn table(
    file: &Type,
    compiled: &'static Compiled,
    projections: &Projections<'_>,
    me: Option<&str>,
    now_ms: u64,
) -> Result<(Snapshot, u64, Option<Vec<u8>>), Refusal> {
    let custody = |error: Error| Refusal::of(&error, Why::Custody);
    let view = projections.view();
    let Some((_, bound)) = file.table() else {
        return Err(Refusal::held(
            Why::Catalog,
            "the file is no catalog table's",
        ));
    };
    let collection = compiled.collection(bound.name).map_err(custody)?;
    let projection = projections.project(collection);
    // The face is the grid's own export, addresses shown: one layout, one spelling of each
    // value, one refusal text.
    let export = grid::export(compiled.table, collection, &view.cells, &[]).map_err(custody)?;
    let layout = grid::layout(collection, &projection);
    let mut columns: Vec<Column> = layout
        .iter()
        .filter(|column| column.source != grid::Source::Pk)
        .map(|column| Column {
            name: column.name.clone(),
            source: match column.source {
                grid::Source::Schema(_) => Source::Schema,
                _ => Source::Unknown,
            },
            read_only: column.read_only,
            refers: match column.source {
                grid::Source::Schema(schema) => match schema.ty {
                    Ty::Ref(target) => {
                        super::types::of_collection(target).map(|file| file.name.to_owned())
                    }
                    _ => None,
                },
                _ => None,
            },
            ty: match column.source {
                grid::Source::Schema(schema) => Some(schema.ty.word()),
                _ => None,
            },
            null: match column.source {
                grid::Source::Schema(schema) => Some(schema.null),
                _ => None,
            },
        })
        .collect();
    columns.extend(file.computed.columns().iter().map(|name| Column {
        name: (*name).to_owned(),
        source: Source::Derived,
        read_only: true,
        refers: None,
        ty: None,
        null: None,
    }));
    columns.push(Column {
        name: aterm_file::snapshot::PK.to_owned(),
        source: Source::Pk,
        read_only: true,
        refers: None,
        ty: None,
        null: None,
    });
    let mut computed = computed(file, compiled, projections, &projection, me);
    let mut withheld = 0_u64;
    let mut rows = Vec::with_capacity(export.base.rows.len());
    for (pk, seen) in &export.base.rows {
        let projected = projection.rows.get(pk);
        let mut cells = BTreeMap::new();
        let mut values = Map::new();
        for column in &layout {
            let Some(seen) = seen.get(&column.name).filter(|seen| !seen.shown.is_empty()) else {
                continue;
            };
            let schema = match column.source {
                grid::Source::Schema(schema) => Some(schema),
                grid::Source::Unknown => None,
                grid::Source::Pk => continue,
            };
            let shown = shown_by_this_build(&seen.shown);
            if shown.contains(secret::MASK) {
                // A sealed value in a column a newer build wrote: never materialized.
                withheld += 1;
                continue;
            }
            let refused = shown.contains(grid::REFUSED);
            withheld += u64::from(refused);
            if let (Some(schema), false, false) = (schema, refused, seen.raw)
                && let Some(value) = projected
                    .and_then(|row| row.fields.get(&column.name))
                    .and_then(|field| typed(schema, field))
            {
                values.insert(column.name.clone(), value);
            }
            cells.insert(column.name.clone(), shown);
        }
        // Computed cells come from guarded values (an address and a label in the spelling the
        // grid shows, a uuid) and from a slug `keys::account_slugs` holds to the same guards.
        // Each is checked again here, so no computed text reaches a file unguarded: one that
        // carries a refusal, or that the guards refuse whole, shows the refusal text, has no
        // typed value and is counted.
        for (column, value) in computed.remove(pk).unwrap_or_default() {
            if !guarded(&value) {
                withheld += 1;
                cells.insert(column.to_owned(), grid::REFUSED.to_owned());
            } else if value.contains(grid::REFUSED) {
                withheld += 1;
                cells.insert(column.to_owned(), value);
            } else {
                values.insert(column.to_owned(), Value::from(value.as_str()));
                cells.insert(column.to_owned(), value);
            }
        }
        rows.push(Row {
            pk: pk.clone(),
            live: projected.is_none_or(cell::Row::live),
            cells,
            values,
        });
    }
    let snapshot = Snapshot {
        name: file.file(),
        kind: TABLE.to_owned(),
        ext: TSV.to_owned(),
        rev: 0,
        complete: view.complete,
        at_ms: now_ms,
        columns,
        handles: file.handles.iter().map(|name| (*name).to_owned()).collect(),
        rows,
    };
    // A baseline that cannot be written leaves the revision without one: a write of it then
    // asks for a refresh rather than guess what its author saw.
    let baseline = super::face::Baseline::encode(&export.base, &snapshot);
    Ok((snapshot, withheld, baseline))
}

/// Which guards the shown text of a stored cell is held to ([`ring_cell`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Held {
    /// A file of the journal ring: every text, of a known type or none, to the guards every
    /// surface keeps ([`guarded`]).
    Ring,
    /// A log of a catalog table: a value its column's type admits passed that type's own guard,
    /// and shows as its table's grid shows it ([`grid::show`]), a whole commit, a command and
    /// prose among them; any other text to the guards every surface keeps.
    Table,
}

/// A shown cell of a ring file or a log and its typed value: the grid's spelling of `x` under
/// `ty`, kept as a value only where it is one the type admits and the guards `held` names let a
/// surface show; else the grid's refusal text, no value, and one more withheld cell.
fn ring_cell(
    ty: Option<&Ty>,
    x: &Value,
    held: Held,
    withheld: &mut u64,
) -> Option<(String, Option<Value>)> {
    if x.is_null() {
        return None;
    }
    let (shown, admitted) = match ty {
        Some(ty) => (grid::show(ty, x), cell::admissible(ty, x)),
        // A column a newer build wrote: raw, never typed.
        None => match grid::raw_shown(x) {
            Value::String(text) => (text, false),
            other => (other.to_string(), false),
        },
    };
    if shown.is_empty() {
        return None;
    }
    let shows = if admitted && held == Held::Table {
        shown_by_this_build(&shown) == shown
    } else {
        guarded(&shown)
    };
    if !shows || shown.contains(grid::REFUSED) || shown.contains(secret::MASK) {
        *withheld += 1;
        return Some((grid::REFUSED.to_owned(), None));
    }
    Some((shown, admitted.then(|| x.clone())))
}

/// The face columns of a ring file: `schema`'s, then `unknown` (columns a newer build wrote),
/// then the computed ones, then the key; all read-only, since each Mac states its own rows and
/// nobody types into another's.
fn ring_columns(schema: &[Stated], unknown: &BTreeSet<String>, computed: &[&str]) -> Vec<Column> {
    let mut columns: Vec<Column> = schema
        .iter()
        .map(|stated| Column {
            name: stated.name.to_owned(),
            source: Source::Schema,
            read_only: true,
            refers: stated.refers.map(str::to_owned),
            ty: Some(stated.word.clone()),
            null: stated.null,
        })
        .collect();
    columns.extend(unknown.iter().map(|name| Column {
        name: name.clone(),
        source: Source::Unknown,
        read_only: true,
        refers: None,
        ty: None,
        null: None,
    }));
    columns.extend(computed.iter().map(|name| Column {
        name: (*name).to_owned(),
        source: Source::Derived,
        read_only: true,
        refers: None,
        ty: None,
        null: None,
    }));
    columns.push(Column {
        name: aterm_file::snapshot::PK.to_owned(),
        source: Source::Pk,
        read_only: true,
        refers: None,
        ty: None,
        null: None,
    });
    columns
}

/// The file a `Ref` column's cells name the rows of.
fn refers(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::Ref(target) => super::types::of_collection(target).map(|file| file.name),
        _ => None,
    }
}

/// One column a ring collection's schema states: its name, its type, and whether a row may
/// leave it empty where the schema says (an event's member has no such statement).
struct Stated {
    name: &'static str,
    /// The shared file its cells name the rows of.
    refers: Option<&'static str>,
    /// The type as a stored schema row spells it.
    word: Value,
    null: Option<bool>,
}

impl Stated {
    fn of(name: &'static str, ty: &Ty, null: Option<bool>) -> Self {
        Self {
            name,
            refers: refers(ty),
            word: ty.word(),
            null,
        }
    }
}

/// A ring collection's columns.
type Columns = Vec<Stated>;

/// The columns of an owned or log collection a face shows: each live one, stated as its schema
/// states it.
fn stored_columns(collection: &Collection) -> Columns {
    collection
        .columns
        .iter()
        .filter(|column| column.live_at(collection.version))
        .map(|column| Stated::of(column.name, &column.ty, Some(column.null)))
        .collect()
}

/// One stored row of an owned or log collection as a face row at `pk`: each field under its
/// column's spelling (`ring_cell`, held to the guards `held` names), a field no column of this
/// build names shown raw and added to `unknown`, and a refused cell counted in `withheld`. `None`
/// for a value that is no object.
fn stored_row(
    collection: &Collection,
    pk: &str,
    value: &Value,
    held: Held,
    unknown: &mut BTreeSet<String>,
    withheld: &mut u64,
) -> Option<Row> {
    let object = value.as_object()?;
    let mut row = Row {
        pk: keys::safe_key(pk).to_owned(),
        live: true,
        ..Row::default()
    };
    for (name, x) in object {
        // `r` is the row's collection version, not a cell.
        if name == "r" {
            continue;
        }
        let ty = collection.column(name).map(|column| &column.ty);
        if ty.is_none() {
            unknown.insert(name.clone());
        }
        if let Some((shown, value)) = ring_cell(ty, x, held, withheld) {
            row.cells.insert(name.clone(), shown);
            if let Some(value) = value {
                row.values.insert(name.clone(), value);
            }
        }
    }
    Some(row)
}

/// Each Mac's rows of an owned collection of the ring, `read` already judged by their keys
/// (`owned::read_owned`: a row at a key that does not name its signer's machine is inert and
/// never here): one row per key, `<pk>.<uuid32>`, its columns the collection's. Returns the
/// columns, the rows and the cells withheld.
fn mine(collection: &Collection, read: &OwnedRead) -> (Columns, BTreeSet<String>, Vec<Row>, u64) {
    let mut unknown = BTreeSet::new();
    let mut withheld = 0;
    let rows = read
        .rows
        .iter()
        .filter_map(|(key, owned)| {
            stored_row(
                collection,
                key,
                &owned.value,
                Held::Ring,
                &mut unknown,
                &mut withheld,
            )
        })
        .collect();
    (stored_columns(collection), unknown, rows, withheld)
}

/// Project a log of a catalog table (`lab/tracker`'s) from `view`: one row per entry the typed
/// read holds ([`View::logs`], `owned::read_log`: an entry at a key that does not name its
/// signer's machine, or at a key an earlier entry took, is inert and never here), keyed
/// `<uuid32>.<n>` in (machine, counter) order, its columns the collection's, all read-only, each
/// cell shown as the table's grid shows a value of its type ([`Held::Table`]). A catalog table
/// forgets no record, so nothing is carried. Returns the snapshot (the revision
/// unset) and the cells withheld.
fn log_table(
    file: &Type,
    collection: &'static Collection,
    view: &View,
    now_ms: u64,
) -> (Snapshot, u64) {
    let mut unknown = BTreeSet::new();
    let mut withheld = 0;
    let rows = view
        .logs
        .get(collection.name)
        .into_iter()
        .flat_map(|read| &read.entries)
        .filter_map(|entry| {
            stored_row(
                collection,
                &owned::log_entry_key(&entry.machine, entry.n),
                &entry.value,
                Held::Table,
                &mut unknown,
                &mut withheld,
            )
        })
        .collect();
    let columns = ring_columns(
        &stored_columns(collection),
        &unknown,
        file.computed_columns(),
    );
    (
        Snapshot {
            name: file.file(),
            kind: file.kind().to_owned(),
            ext: TSV.to_owned(),
            rev: 0,
            complete: view.complete,
            at_ms: now_ms,
            columns,
            handles: file.handles.iter().map(|name| (*name).to_owned()).collect(),
            rows,
        },
        withheld,
    )
}

/// The columns of `ai/events`: each event field under a readable name, its wire key and its
/// type. Times are Unix milliseconds, as every other time column of a shared file; an event
/// states seconds.
pub(super) const EVENT_COLUMNS: [(&str, &str, Ty); 19] = [
    ("at", "t", Ty::Time),
    ("kind", "k", Ty::Text(32)),
    ("account", "a", Ty::Ref("account")),
    ("cli", "c", Ty::Text(16)),
    ("sub", "s", Ty::Hex(12)),
    ("window", "w", Ty::Text(16)),
    ("pct", "p", Ty::Uint),
    ("reset", "r", Ty::Time),
    ("res", "rr", Ty::Text(16)),
    ("until", "u", Ty::Time),
    ("plan", "plan", Ty::Text(16)),
    ("src", "src", Ty::Text(16)),
    ("why", "why", Ty::Text(16)),
    ("to", "to", Ty::Ref("account")),
    ("tab", "x", Ty::Text(16)),
    ("harness", "h", Ty::Text(16)),
    ("op", "op", Ty::Text(16)),
    ("via", "via", Ty::Text(16)),
    ("program", "pg", Ty::Text(16)),
];

/// The wire keys of an event that hold a time in seconds.
const EVENT_SECONDS: [&str; 3] = ["t", "r", "u"];

/// Each Mac's events, from the ring's one log (`ev`), `read` already judged by its keys
/// (`owned::read_log`): one row per event, keyed `<uuid32>.<n>.<i>` (the batch's key and the
/// event's place in it). An event this build's vocabulary does not read
/// ([`Event::from_value`]) is no row and is counted with the inert records. Returns the
/// columns, the rows, the cells withheld and the events not read.
fn events(read: &LogRead) -> (Columns, Vec<Row>, u64, u64) {
    let schema = EVENT_COLUMNS
        .iter()
        .map(|(name, _, ty)| Stated::of(name, ty, None))
        .collect();
    let mut withheld = 0;
    let mut unread = 0_u64;
    let mut rows = Vec::new();
    for entry in &read.entries {
        let batch = entry.value.get("e").and_then(Value::as_array);
        for (i, x) in batch.into_iter().flatten().enumerate() {
            let Some(event) = Event::from_value(x) else {
                unread += 1;
                continue;
            };
            let wire = event.to_value();
            let mut row = Row {
                pk: format!("{}.{}.{i}", keys::uuid32(&entry.machine), entry.n),
                live: true,
                ..Row::default()
            };
            for (name, key, ty) in &EVENT_COLUMNS {
                let Some(x) = wire.get(key) else {
                    continue;
                };
                let x = match x.as_u64() {
                    Some(seconds) if EVENT_SECONDS.contains(key) => {
                        Value::from(seconds.saturating_mul(1000))
                    }
                    _ => x.clone(),
                };
                if let Some((shown, value)) = ring_cell(Some(ty), &x, Held::Ring, &mut withheld) {
                    row.cells.insert((*name).to_owned(), shown);
                    if let Some(value) = value {
                        row.values.insert((*name).to_owned(), value);
                    }
                }
            }
            rows.push(row);
        }
    }
    (schema, rows, withheld, unread)
}

/// The operations of `ring` that wrote `collection`, oldest claim first: which, whose, when and
/// which keys, never a value.
fn ring_history(collection: &str, ring: &Ring) -> Vec<Value> {
    let mut entries: Vec<(u64, Digest, Value)> = ring
        .ops
        .iter()
        .filter_map(|op| {
            let keys: Vec<Value> = op
                .changes
                .iter()
                .filter(|change| change.collection == collection)
                .map(|change| Value::from(keys::safe_key(&change.key)))
                .collect();
            if keys.is_empty() {
                return None;
            }
            let ms = ring.authored.get(&op.id).copied().unwrap_or(0);
            let mut entry = Map::new();
            entry.insert(
                "op".to_owned(),
                Value::from(aterm_codec::hex::encode_lower(&op.id)),
            );
            entry.insert("by".to_owned(), Value::from(keys::actor_key(&op.actor)));
            entry.insert("ms".to_owned(), Value::from(ms));
            entry.insert("revert".to_owned(), Value::from(op.revert.is_some()));
            entry.insert("keys".to_owned(), Value::Array(keys));
            Some((ms, op.id, Value::Object(entry)))
        })
        .collect();
    entries.sort_unstable_by_key(|(ms, id, _)| (*ms, *id));
    entries.into_iter().map(|(_, _, entry)| entry).collect()
}

/// The rows a previous snapshot of `held` showed, where it was projected from the same replica
/// as `frontier`: what a ring file keeps of rows the window no longer holds. A snapshot of
/// another replica, or one whose frontier does not read, carries nothing.
fn previous_rows(held: &File, frontier: &Frontier) -> Vec<Row> {
    let same = held
        .frontier()
        .is_some_and(|old| old.source == frontier.source);
    if !same {
        return Vec::new();
    }
    held.published()
        .snapshot
        .map(|snapshot| snapshot.rows)
        .unwrap_or_default()
}

/// How long, and within how many bytes, a file of the ring shows rows its window no longer
/// holds ([`retain`]): a log's entries 30 days, a Mac's rows 120.
const fn kept_for(collection: &Collection) -> retain::Keep {
    match collection.kind {
        Kind::Log => retain::LOG,
        Kind::Owned | Kind::Cells => retain::MINE,
    }
}

/// Project a file of the journal ring from `ring`: its snapshot (the revision unset), the cells
/// withheld and the records inert. The rows the window holds now, with each row a previous
/// snapshot of this replica showed and the window no longer holds, until it ages out
/// ([`retain::carry`]: a Mac's row 120 days, an event 30, from when this Mac last saw it change).
/// Whose each row is, is what the ring's read resolved ([`Ring::resolve`]); no session is read.
fn ring_file(
    file: &Type,
    collection: &'static Collection,
    ring: &Ring,
    held: &File,
    now_ms: u64,
) -> (Snapshot, u64, u64) {
    let resolve = |roster: &Digest, actor: &Actor| ring.resolve(roster, actor);
    let (schema, unknown, read, withheld, inert) = match collection.kind {
        Kind::Log => {
            let log = owned::read_log(collection, &ring.ops, resolve);
            let (schema, rows, withheld, unread) = events(&log);
            let inert = u64::try_from(log.inert)
                .unwrap_or(u64::MAX)
                .saturating_add(unread);
            (schema, BTreeSet::new(), rows, withheld, inert)
        }
        Kind::Owned | Kind::Cells => {
            let owned = owned::read_owned(collection, &ring.ops, resolve);
            let (schema, unknown, rows, withheld) = mine(collection, &owned);
            let inert = u64::try_from(owned.inert).unwrap_or(u64::MAX);
            (schema, unknown, rows, withheld, inert)
        }
    };
    let rows = retain::carry(
        &previous_rows(held, &ring.frontier),
        read,
        kept_for(collection),
        now_ms,
    );
    let columns = ring_columns(&schema, &unknown, file.computed_columns());
    (
        Snapshot {
            name: file.file(),
            kind: file.kind().to_owned(),
            ext: TSV.to_owned(),
            rev: 0,
            complete: ring.complete,
            at_ms: now_ms,
            columns,
            handles: file.handles.iter().map(|name| (*name).to_owned()).collect(),
            rows,
        },
        withheld,
        inert,
    )
}

/// Project `ai/limits` from the `ai/usage` rows `usage` (this pass's projection, or the
/// snapshot here), naming each account by the registry's view (`compiled`'s, its collections
/// projected once a pass, [`Projections`]) as `ai/accounts` names it (`alab::account_names`),
/// and preferring a current member's reading
/// (`members`) over its fenced predecessor's where the registry binds both to one Mac;
/// `complete` is the ring's. Returns the snapshot and the cells withheld.
fn limits_file(
    file: &Type,
    usage: &[Row],
    (compiled, projections): (&'static Compiled, &Projections<'_>),
    members: &BTreeSet<String>,
    complete: bool,
    now_ms: u64,
) -> (Snapshot, u64) {
    let readings: Vec<Reading> = usage.iter().filter_map(Reading::of_row).collect();
    let view = projections.view();
    let empty = OwnedRead::default();
    let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
    // The seats are the facts rows' and the live machine rows': which machine this Mac is
    // moves none of them, so the fold reads no `=here`.
    let lab = LabView::read_projected(
        &view.cells,
        &projections.project(&registry::MACHINE),
        account_names_of(compiled, projections),
        owned(registry::MACHINE_FACTS.name),
        owned(registry::MAILBOX_OBS.name),
        None,
    );
    let seats = Seats {
        bound: lab.bound,
        current: members.clone(),
    };
    let mut withheld = 0;
    let mut rows = Vec::new();
    for (pk, limit) in limits::fold(&readings, &seats) {
        let account = limit.account.as_deref().unwrap_or(pk.as_str());
        let names = lab.names.get(account);
        let slug = names.map(|names| names.slug.as_str());
        let name = names.and_then(|names| names.name.as_deref());
        let mut row = limit.row(&pk, slug, name);
        // Every computed text is held to the guards once more before it is written.
        for (column, text) in &mut row.cells {
            if !guarded(text) {
                withheld += 1;
                *text = grid::REFUSED.to_owned();
                row.values.remove(column);
            }
        }
        rows.push(row);
    }
    let columns = file
        .computed_columns()
        .iter()
        .map(|name| Column {
            name: (*name).to_owned(),
            source: Source::Derived,
            read_only: true,
            refers: (*name == "account").then(|| super::ACCOUNTS.name.to_owned()),
            ty: None,
            null: None,
        })
        .chain(std::iter::once(Column {
            name: aterm_file::snapshot::PK.to_owned(),
            source: Source::Pk,
            read_only: true,
            refers: None,
            ty: None,
            null: None,
        }))
        .collect();
    (
        Snapshot {
            name: file.file(),
            kind: file.kind().to_owned(),
            ext: TSV.to_owned(),
            rev: 0,
            complete,
            at_ms: now_ms,
            columns,
            handles: file.handles.iter().map(|name| (*name).to_owned()).collect(),
            rows,
        },
        withheld,
    )
}

/// How many retained records of `file`'s collection `view` holds inert (§5): automatic records
/// past their writer's quota ([`crate::typed::owned::Quota::over`]); in a collection under the
/// authority rules, each record holding a write they hold inert
/// ([`crate::typed::authority::Inert::records`]); in an owned collection, each row at a key
/// that does not name its signer's machine or that is no bounded object with a counter
/// ([`OwnedRead::inert`]); and, in a log, each entry at such a key or at a key an earlier entry
/// took ([`crate::typed::owned::LogRead::inert`]). They stay in history with their authors; the
/// status counts them. A file of the journal ring counts its own read's (`ring_file`): a row or
/// an entry at a key that is not its signer's, and an event this build's vocabulary does not
/// read.
pub(crate) fn inert(file: &Type, view: &View) -> u64 {
    let Some(collection) = file.collection() else {
        return 0;
    };
    let over = view
        .ops
        .iter()
        .filter(|op| {
            view.quota.over.contains(&op.id)
                && op
                    .changes
                    .iter()
                    .any(|change| change.collection == collection.name)
        })
        .count();
    let owned = view.owned.get(collection.name).map_or(0, |read| read.inert);
    let log = view.logs.get(collection.name).map_or(0, |read| read.inert);
    let authority = view
        .inert
        .records
        .get(collection.name)
        .copied()
        .unwrap_or(0);
    u64::try_from(
        over.saturating_add(owned)
            .saturating_add(log)
            .saturating_add(authority),
    )
    .unwrap_or(u64::MAX)
}

/// The operations that wrote `file`'s collection, oldest claim first (by the time each says it
/// was authored, then by its id, as `att::Handle::history` orders them): which, whose, when and
/// which cells, never a value.
fn history(file: &Type, family: &Family, view: &View, held: &File) -> Vec<Value> {
    let collection = file.collection().map_or("", |collection| collection.name);
    // Which of this Mac's intents each operation carries, who said they made it and in which
    // session (§5, Provenance): the history already written says, and the done markers add
    // what was published since. Carried forward here, it outlasts the markers' bound.
    let mut intents: BTreeMap<String, Map> = BTreeMap::new();
    for entry in held.history() {
        let (Some(op), Some(intent)) = (
            entry.get("op").and_then(Value::as_str),
            entry.get("intent").and_then(Value::as_str),
        ) else {
            continue;
        };
        let mut said = Map::new();
        said.insert("intent".to_owned(), Value::from(intent));
        for field in ["said_by", "session"] {
            if let Some(word) = entry.get(field).and_then(Value::as_str) {
                said.insert(field.to_owned(), Value::from(word));
            }
        }
        intents.insert(op.to_owned(), said);
    }
    for done in held.outbox().done {
        let Some(op) = done.op else {
            continue;
        };
        let mut said = Map::new();
        said.insert("intent".to_owned(), Value::from(done.intent.id));
        said.insert("said_by".to_owned(), Value::from(done.intent.by));
        if let Some(session) = done.intent.session {
            said.insert("session".to_owned(), Value::from(session));
        }
        intents.insert(op, said);
    }
    let authored: BTreeMap<_, u64> = family
        .log
        .eligible_iter()
        .map(|(operation, _)| (operation.header.id, operation.authored_ms))
        .collect();
    let mut entries: Vec<(u64, _, Value)> = view
        .ops
        .iter()
        .filter_map(|op| {
            let keys: Vec<Value> = op
                .changes
                .iter()
                .filter(|change| change.collection == collection)
                .map(|change| Value::from(keys::safe_key(&change.key)))
                .collect();
            if keys.is_empty() {
                return None;
            }
            let ms = authored.get(&op.id).copied().unwrap_or(0);
            let mut entry = Map::new();
            let id = aterm_codec::hex::encode_lower(&op.id);
            if let Some(said) = intents.get(&id) {
                entry.extend(said.clone());
            }
            entry.insert("op".to_owned(), Value::from(id));
            entry.insert("by".to_owned(), Value::from(keys::actor_key(&op.actor)));
            entry.insert("ms".to_owned(), Value::from(ms));
            entry.insert("revert".to_owned(), Value::from(op.revert.is_some()));
            entry.insert("keys".to_owned(), Value::Array(keys));
            Some((ms, op.id, Value::Object(entry)))
        })
        .collect();
    entries.sort_unstable_by_key(|(ms, id, _)| (*ms, *id));
    entries.into_iter().map(|(_, _, entry)| entry).collect()
}

/// What decides a projection that this build's code fixes (its decode rules, guards, schema,
/// quota, grid and claim code): the identity the linker stamped into the running image
/// ([`aterm_sysprobe::image::identity`]), read once a process. An image that carries none
/// stands for a build of its own each process, so its first pass projects; `None` where not
/// even that could be drawn, and then every pass projects.
fn rules() -> Option<[u8; 32]> {
    static RULES: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    let rules = *RULES.get_or_init(|| {
        let mut sha = Sha256::new();
        sha.update(b"aterm/file/rules/1\0");
        if let Some(identity) = aterm_sysprobe::image::identity() {
            sha.update([1]);
            sha.update((identity.len() as u64).to_le_bytes());
            sha.update(identity);
        } else {
            let mut nonce = [0_u8; 32];
            aterm_uds::rand::fill(&mut nonce).ok()?;
            sha.update([0]);
            sha.update(nonce);
        }
        Some(sha.finalize())
    });
    // A test's later build: the same image with other rules (`super::seam::new_build`).
    #[cfg(test)]
    if let (Some(rules), Some(build)) = (rules, super::seam::build()) {
        let mut sha = Sha256::new();
        sha.update(rules);
        sha.update(build.as_bytes());
        return Some(sha.finalize());
    }
    rules
}

/// Who said they made each operation, as the done markers here say (the newest marker of an
/// operation last): what [`history`] lays over the history already written.
fn provenance(held: &File) -> BTreeMap<String, (String, String, Option<String>)> {
    held.outbox()
        .done
        .into_iter()
        .filter_map(|done| {
            let op = done.op?;
            Some((op, (done.intent.id, done.intent.by, done.intent.session)))
        })
        .collect()
}

/// `word` into `sha`, its absence and its length taking places of their own, so no two lists
/// of words digest alike.
fn word(sha: &mut Sha256, word: Option<&str>) {
    match word {
        None => sha.update([0]),
        Some(word) => {
            sha.update([1]);
            sha.update((word.len() as u64).to_le_bytes());
            sha.update(word.as_bytes());
        }
    }
}

/// What fixes `family`'s view ([`View::of`]) into `sha`: its frontier, the log's judgment of
/// each record and what the session resolved.
fn family_into(sha: &mut Sha256, family: &Family) {
    sha.update(family.frontier.fingerprint());
    if !unkeyed(Unkeyed::Judgment) {
        sha.update(family.log.judgment());
    }
    if !unkeyed(Unkeyed::Resolved) {
        sha.update(family.resolved.digest());
    }
}

/// The projection key of a file of a catalog table (docs/DESIGN-shared-files-2026-11-11.md
/// §4): a digest of everything its projection reads but the history already written, which
/// the skip checks on its own ([`opened`]). The projection ([`View::of`], [`table`] or, of a
/// log, [`log_table`], [`history`], [`inert`]) takes the family's log and resolution, `me` (this Mac's machine as
/// the roster names it, for a file whose columns read it, [`Computed::reads_here`]; `None` for
/// every other, whose key a roster that moves leaves alone), this build's compiled tables and
/// the done markers, and nothing else: the frontier and the log's judgment fix the log's
/// records and how they are judged, the resolution what the session resolved, and [`rules`] the
/// code. This Mac's clock is left out: it sets only `at_ms`, which a snapshot's content leaves
/// out. `None` where the rules are unknown.
fn key(family: &Family, me: Option<&str>, held: &File) -> Option<[u8; 32]> {
    let mut sha = Sha256::new();
    sha.update(b"aterm/file/projection/1\0");
    if !unkeyed(Unkeyed::Rules) {
        sha.update(rules()?);
    }
    family_into(&mut sha, family);
    if !unkeyed(Unkeyed::Me) {
        word(&mut sha, me);
    }
    if !unkeyed(Unkeyed::Provenance) {
        let said = provenance(held);
        sha.update((said.len() as u64).to_le_bytes());
        for (op, (intent, by, session)) in &said {
            for said in [Some(op), Some(intent), Some(by), session.as_ref()] {
                word(&mut sha, said.map(String::as_str));
            }
        }
    }
    Some(sha.finalize())
}

/// The projection key of a file of the journal ring (docs/DESIGN-shared-files-2026-11-11.md
/// §4): what [`ring_file`] and [`ring_history`] read. The frontier and each week's judgment fix
/// the window's records and how each is judged, the bindings whose each row is, and [`rules`]
/// the code. The projection also reads the rows of the snapshot here (`snapshot`), which the
/// skip takes only where they are the ones the status records ([`opened`]); this Mac's clock
/// decides which of those a window that no longer holds them still shows ([`retain::carry`]),
/// so the key holds each row of `snapshot` whose span (`keep`) has passed at `now_ms`. A pass
/// at rest, its window and records standing, so projects again once a row's span passes, and
/// a row the window no longer holds leaves then. The clock also decides which rows a
/// projection leaves out once they take more than `keep`'s bytes (those seen change least
/// recently go first, and a row the last projection left out is seen now), which the key does
/// not hold: past that budget a pass at rest keeps the rows the last projection left. A ring
/// file is never too large to take, since `retain` keeps its rows within a quarter of what a
/// snapshot may hold, so no key is kept for that reason. `None` where the rules are unknown.
fn ring_key(
    ring: &Ring,
    snapshot: Option<&Snapshot>,
    keep: retain::Keep,
    now_ms: u64,
) -> Option<[u8; 32]> {
    let mut sha = Sha256::new();
    sha.update(b"aterm/file/projection/ring/1\0");
    if !unkeyed(Unkeyed::Rules) {
        sha.update(rules()?);
    }
    sha.update(ring.frontier.fingerprint());
    if !unkeyed(Unkeyed::Judgment) {
        sha.update((ring.judgments.len() as u64).to_le_bytes());
        for judgment in &ring.judgments {
            sha.update(judgment);
        }
    }
    if !unkeyed(Unkeyed::Resolved) {
        sha.update((ring.bindings.len() as u64).to_le_bytes());
        for ((roster, writer), machine) in &ring.bindings {
            sha.update(roster);
            sha.update(writer);
            match machine {
                None => sha.update([0]),
                Some(machine) => {
                    sha.update([1]);
                    sha.update(machine);
                }
            }
        }
    }
    if !unkeyed(Unkeyed::Aged) {
        let aged: Vec<&str> = snapshot
            .into_iter()
            .flat_map(|snapshot| &snapshot.rows)
            .filter(|row| {
                retain::seen(row).is_some_and(|at| now_ms.saturating_sub(at) > keep.age_ms)
            })
            .map(|row| row.pk.as_str())
            .collect();
        sha.update((aged.len() as u64).to_le_bytes());
        for pk in aged {
            word(&mut sha, Some(pk));
        }
    }
    Some(sha.finalize())
}

/// The projection key of a view (docs/DESIGN-shared-files-2026-11-11.md §4): what
/// [`limits_file`] folds. The view's frontier is the registry's read under a source of its own
/// ([`view_frontier`]); the fold reads that read's view, which names each account and binds
/// each seat (what fixes it, as [`key`] reads a family: [`family_into`]), the `ai/usage`
/// snapshot it folds (`usage`, by its content), whether the ring's history is complete, every
/// current member, and [`rules`] the code. `None` where the rules are unknown.
fn view_key(
    frontier: &Frontier,
    complete: bool,
    usage: Option<&Snapshot>,
    registry: &Family,
    members: &BTreeSet<String>,
) -> Option<[u8; 32]> {
    let mut sha = Sha256::new();
    sha.update(b"aterm/file/projection/view/2\0");
    if !unkeyed(Unkeyed::Rules) {
        sha.update(rules()?);
    }
    sha.update(frontier.fingerprint());
    family_into(&mut sha, registry);
    sha.update([u8::from(complete)]);
    if !unkeyed(Unkeyed::Usage) {
        match usage {
            None => sha.update([0]),
            Some(usage) => {
                sha.update([1]);
                sha.update(usage.content());
            }
        }
    }
    if !unkeyed(Unkeyed::Members) {
        sha.update((members.len() as u64).to_le_bytes());
        for member in members {
            word(&mut sha, Some(member));
        }
    }
    Some(sha.finalize())
}

/// Whether a test leaves `part` out of the key or the skip (`super::seam::leave_out`).
#[cfg(test)]
fn unkeyed(part: Unkeyed) -> bool {
    super::seam::unkeyed()(part)
}

/// Never outside a test: every part is in the key, and the history in the skip.
#[cfg(not(test))]
const fn unkeyed(_: Unkeyed) -> bool {
    false
}

/// A part of what a projection reads: a test may leave one out of the key, or the history or
/// the snapshot's content out of the skip (`super::seam::leave_out`), to show each is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unkeyed {
    Resolved,
    Judgment,
    Me,
    Rules,
    Provenance,
    History,
    Snapshot,
    /// A ring file's rows whose span passed ([`ring_key`]).
    Aged,
    /// The content of the `ai/usage` snapshot a view folds ([`view_key`]).
    Usage,
    /// The current members a view folds by ([`view_key`]).
    Members,
}

/// What one pass read for a file held here as `held`, from a replica at `frontier` and the home
/// `newest` says: its projection (`project`) where its key moved since the snapshot here was
/// recorded, else nothing. `key` makes the key of what the projection reads, given the snapshot
/// it reads as the one before (only a ring file's reads it, [`ring_key`]). A pass projects
/// nothing where the status records the snapshot at this key, that snapshot (by its revision,
/// extension and content) and the history beside it (by its digest) are the ones it recorded,
/// and its inert count is known; nor where the last pass kept the snapshot for a reason nothing
/// it rests on has moved since: a read behind the snapshot, a frontier too large to read back,
/// or a projection at this very key too large to take. `refresh` ([`Pass::again`]) projects
/// every snapshot it does not keep for one of those reasons, whatever the key says, and so does
/// a pass whose rules are unknown ([`rules`]). A projection is recorded at the key of what it
/// made: the snapshot the next pass reads as the one before.
fn opened(
    held: &File,
    pass: &Pass,
    newest: bool,
    frontier: &Frontier,
    key: impl Fn(Option<&Snapshot>) -> Option<[u8; 32]>,
    project: impl FnOnce() -> Result<Projected, Refusal>,
) -> Result<Opened, Refusal> {
    let state = held.state().unwrap_or_default();
    let snapshot = held.published().snapshot;
    let read = key(snapshot.as_ref());
    let current = |key: &[u8; 32]| Some(aterm_codec::hex::encode_lower(key));
    let behind = snapshot.is_some()
        && held
            .frontier()
            .map_or(!newest, |old| !frontier.admits_over(&old));
    let kept = if behind {
        Some(Kept::Behind)
    } else if frontier.encode().len() as u64 > aterm_file::snapshot::MAX_BYTES
        || (state.kept == Some(Kept::TooLarge)
            && read.is_some_and(|key| state.kept_key == current(&key)))
    {
        Some(Kept::TooLarge)
    } else {
        None
    };
    let history_unkeyed = unkeyed(Unkeyed::History);
    let snapshot_unkeyed = unkeyed(Unkeyed::Snapshot);
    // The status describes the snapshot at the revision it records. One at another revision
    // was written by a pass killed before its status: it is projected again, and that pass
    // records the revision. So is one at that revision that says other than the status
    // records (put back from a copy, written by another hand), one whose header states no type
    // for a stored column (an earlier build's put back at that revision: the content leaves the
    // types out, and this build's projection states one for each), and one that does not say its
    // inert count, which an older build leaves out when it rewrites a status a newer one
    // wrote: the count is unknown, never 0.
    let unchanged = !pass.again
        && read.is_some_and(|key| state.key == current(&key))
        && state.rev > 0
        && snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.rev == state.rev
                && (snapshot_unkeyed
                    || (snapshot.ext == state.ext
                        && state.snapshot
                            == Some(aterm_codec::hex::encode_lower(&snapshot.content()))
                        && snapshot
                            .columns
                            .iter()
                            .all(|column| column.source != Source::Schema || column.ty.is_some())))
        })
        && state.inert.is_some()
        && state.history.is_some()
        && (history_unkeyed || held.history_digest() == state.history);
    let (projection, key) = if let Some(kept) = kept {
        (aterm_file::Projection::Kept(kept), read)
    } else if unchanged {
        (aterm_file::Projection::Unchanged, read)
    } else {
        let projected = project()?;
        let made = key(Some(&projected.snapshot));
        (aterm_file::Projection::Fresh(Box::new(projected)), made)
    };
    Ok(Opened {
        newest,
        frontier: frontier.clone(),
        // A key that could not be made is no key a later pass meets: a pass that projected
        // records it, and the next projects again.
        key: key.unwrap_or_default(),
        projection,
    })
}

/// What one pass read for `file`, a catalog table's file held here as `held`, from its
/// family's read (`projector`), from the home `newest` says: `me` is this Mac's machine as the
/// roster names it (`None` where it names none) for a file whose columns read it
/// ([`Computed::reads_here`]), and `None` for any other file.
fn opened_table(
    projector: &Projector<'_>,
    file: &Type,
    held: &File,
    pass: &Pass,
    me: Option<&str>,
    newest: bool,
) -> Result<Opened, Refusal> {
    let family = projector.family;
    opened(
        held,
        pass,
        newest,
        &family.frontier,
        |_| key(family, me, held),
        || {
            let projections = projector.projections();
            let view = projections.view();
            let (snapshot, withheld, baseline) = match file.collection() {
                // A log is each Mac's entries: no grid laid it out, so no write is read
                // against a baseline of it (`Type::edited`).
                Some(collection) if collection.kind == Kind::Log => {
                    let (snapshot, withheld) = log_table(file, collection, view, pass.now_ms);
                    (snapshot, withheld, None)
                }
                _ => table(file, family.compiled, projections, me, pass.now_ms)?,
            };
            Ok(Projected {
                snapshot,
                history: history(file, family, view, held),
                baseline,
                withheld,
                inert: inert(file, view),
            })
        },
    )
}

/// What one pass read under custody for its files, owned, so that each file is projected once
/// custody is closed and no projection reads a session: each table family a file binds, read
/// once (the registry's too where a view names accounts by it); the journal ring where a file
/// binds it, read once; and who this Mac and every current member are, or why the roster did
/// not read.
struct Inputs {
    families: Vec<(&'static str, Result<Family, Refusal>)>,
    ring: Option<Result<Ring, Refusal>>,
    roster: Result<Roster, Refusal>,
}

impl Inputs {
    /// What a pass over `files` reads under `custody`. A file refuses with the first input it
    /// reads that refused (a catalog table's file whose columns read `=here` the roster, then
    /// its family, and any other its family alone; a view the ring, the roster, then the
    /// registry), so a family is read only for a file that no input before it refused.
    fn read(
        custody: &Custody<'_>,
        files: &[(&'static Type, File, std::io::Result<Look>)],
        pass: &Pass,
    ) -> Self {
        let mut inputs = Self {
            families: Vec::new(),
            ring: None,
            roster: custody.roster.clone(),
        };
        for (file, _, _) in files {
            match file.store {
                Store::Table { family, .. } => {
                    if !file.computed.reads_here() || inputs.roster.is_ok() {
                        inputs.family(custody, family);
                    }
                }
                Store::Journal(_) => inputs.ring(custody, pass),
                Store::View(super::Fold::Limits) => {
                    inputs.ring(custody, pass);
                    // The names and seats a view shows come from the registry, read where the
                    // ring and the roster read.
                    if inputs.ring.as_ref().is_some_and(Result::is_ok) && inputs.roster.is_ok() {
                        inputs.family(custody, &att::REGISTRY);
                    }
                }
            }
        }
        inputs
    }

    /// `first`'s family, read now where no file before asked for it.
    fn family(&mut self, custody: &Custody<'_>, first: &'static Compiled) {
        let role = first.table.role;
        if !self.families.iter().any(|(read, _)| *read == role) {
            self.families.push((role, Family::read(custody, first)));
        }
    }

    /// The ring's window at this pass's clock, read now where no file before asked for it.
    fn ring(&mut self, custody: &Custody<'_>, pass: &Pass) {
        if self.ring.is_none() {
            self.ring = Some(Ring::read(custody, pass.now_ms / 1000));
        }
    }
}

/// What one pass read for every file of `files`, in order, from what `inputs` holds and the
/// home `newest` says: a catalog table's files from their family, whose collections each are
/// projected once for all of them ([`Projector`]); the ring's files from its one read; and a
/// view from the `ai/usage` rows it folds, as this pass projected them or as the snapshot here,
/// and the registry it names accounts by. Each refuses with the first input it reads that
/// refused ([`Inputs::read`]), so a view is never folded without the registry's names and
/// seats, nor with the members unknown.
fn read_files(
    inputs: &Inputs,
    root: &aterm_file::Root,
    files: &[(&'static Type, File, std::io::Result<Look>)],
    pass: &Pass,
    newest: bool,
) -> Vec<Result<Opened, Refusal>> {
    // One projector a family, so the files of a family share each collection's projection.
    let projectors: Vec<(&str, Result<Projector<'_>, &Refusal>)> = inputs
        .families
        .iter()
        .map(|(role, family)| (*role, family.as_ref().map(Projector::new)))
        .collect();
    let projector = |first: &Compiled| match projectors
        .iter()
        .find(|(role, _)| *role == first.table.role)
    {
        Some((_, Ok(projector))) => Ok(projector),
        Some((_, Err(refusal))) => Err((*refusal).clone()),
        None => Err(Refusal::held(Why::Custody, "the table was not read")),
    };
    let ring = || match &inputs.ring {
        Some(Ok(ring)) => Ok(ring),
        Some(Err(refusal)) => Err(refusal.clone()),
        None => Err(Refusal::held(Why::Custody, "the journal ring was not read")),
    };
    let roster = || inputs.roster.as_ref().map_err(Refusal::clone);
    // `ai/usage` as this pass projected it: what a view later in the list folds.
    let mut usage: Option<Snapshot> = None;
    let mut out = Vec::with_capacity(files.len());
    for (file, held, _) in files {
        let read = match file.store {
            Store::Table { family, .. } => {
                // Only a file whose columns read `=here` reads the roster, and refuses where it
                // did not read; every other projects with no machine of this Mac's.
                let me = if file.computed.reads_here() {
                    roster().map(|roster| roster.me.as_deref())
                } else {
                    Ok(None)
                };
                me.and_then(|me| opened_table(projector(family)?, file, held, pass, me, newest))
            }
            Store::Journal(collection) => ring().and_then(|ring| {
                let keep = kept_for(collection);
                opened(
                    held,
                    pass,
                    newest,
                    &ring.frontier,
                    |snapshot| ring_key(ring, snapshot, keep, pass.now_ms),
                    || {
                        let (snapshot, withheld, inert) =
                            ring_file(file, collection, ring, held, pass.now_ms);
                        Ok(Projected {
                            history: ring_history(collection.name, ring),
                            snapshot,
                            baseline: None,
                            withheld,
                            inert,
                        })
                    },
                )
            }),
            Store::View(super::Fold::Limits) => ring().and_then(|ring| {
                let members = &roster()?.members;
                // A registry that refuses refuses the view, which would otherwise fold its
                // readings with no account's name and no seat's machine.
                let registry = projector(&att::REGISTRY)?;
                let frontier = view_frontier(registry.family, file.name);
                let kept;
                let usage = if let Some(usage) = &usage {
                    Some(usage)
                } else {
                    kept = File::at(root, &super::USAGE.file()).published().snapshot;
                    kept.as_ref()
                };
                opened(
                    held,
                    pass,
                    newest,
                    &frontier,
                    |_| view_key(&frontier, ring.complete, usage, registry.family, members),
                    || {
                        let rows = usage.map_or(&[][..], |usage| usage.rows.as_slice());
                        let (snapshot, withheld) = limits_file(
                            file,
                            rows,
                            (registry.family.compiled, registry.projections()),
                            members,
                            ring.complete,
                            pass.now_ms,
                        );
                        Ok(Projected {
                            snapshot,
                            history: Vec::new(),
                            baseline: None,
                            withheld,
                            inert: 0,
                        })
                    },
                )
            }),
        };
        if file.name == super::USAGE.name
            && let Ok(Opened {
                projection: aterm_file::Projection::Fresh(projected),
                ..
            }) = &read
        {
            usage = Some(projected.snapshot.clone());
        }
        out.push(read);
    }
    out
}

/// Run one pass over `only`, or over every file this build compiles: `paths` is any home of this
/// Mac's writer lineage, `config` its `aterm.toml`. Each file gets its outcome; nothing is an
/// error.
#[must_use]
pub fn pass(paths: &Paths, config: &Path, only: Option<&Name>, pass: &Pass) -> Vec<Outcome> {
    materialize_files(paths, config, only, pass)
}

/// [`pass`], whole. The `SharedFile` anchor sits here rather than on a function named `pass`:
/// the step audit tells anchored functions apart by their bare names, and the publisher's pass
/// and the catalog's release pass are anchored too.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Materialize",
        project = "aterm_db::file::model_tests::read_transition"
    )
)]
fn materialize_files(
    paths: &Paths,
    config: &Path,
    only: Option<&Name>,
    pass: &Pass,
) -> Vec<Outcome> {
    let home = generation::original(paths);
    let root = super::root(&home);
    let files: Vec<(&'static Type, File, std::io::Result<Look>)> = super::FILES
        .iter()
        .filter(|file| only.is_none_or(|only| *only == file.file()))
        .map(|file| {
            let held = File::at(&root, &file.file());
            // Noted before custody opens, and its lease held until the record: a pass that
            // another overtakes records nothing, also where the other changed nothing.
            let look = held.look();
            (file, held, look)
        })
        .collect();
    let read = with_custody(&home, config, pass, |custody| {
        Inputs::read(custody, &files, pass)
    });
    // Custody is closed: each file is projected from what its pass read, and nothing else.
    let read: Vec<Result<Opened, Refusal>> = match read {
        Ok((inputs, newest)) => read_files(&inputs, &root, &files, pass, newest),
        Err(refusal) => files.iter().map(|_| Err(refusal.clone())).collect(),
    };
    #[cfg(test)]
    super::seam::between();
    files
        .into_iter()
        .zip(read)
        .map(|((file, held, look), read)| {
            let (recording, detail) = match read {
                Ok(opened) => (aterm_file::Pass::Opened(Box::new(opened)), None),
                Err(refusal) => (
                    aterm_file::Pass::Refused {
                        word: refusal.word,
                        why: refusal.why,
                    },
                    Some(refusal.detail),
                ),
            };
            let recorded = look.and_then(|look| held.record(look, recording, pass.now_ms));
            let (recorded, problem) = match recorded {
                // The snapshot now holds what this Mac published: its markers say so, and a
                // read stops laying those writes over it.
                Ok(recorded) => (
                    Some(recorded),
                    held.settle_done().err().map(|error| error.to_string()),
                ),
                Err(error) => (None, Some(error.to_string())),
            };
            let status = held.status();
            let kept = match recorded {
                Some(Recorded::Kept(kept)) => Some(kept),
                // The pass changed nothing: the reason the status keeps is this pass's too.
                Some(Recorded::Settled | Recorded::Fenced) => {
                    status.state.as_ref().and_then(|state| state.kept)
                }
                _ => None,
            };
            let detail = detail.or_else(|| match kept {
                Some(Kept::Behind) => Some(
                    "the replica read is behind the snapshot: an earlier home, one without a \
                     record the snapshot was projected from, or a fenced home where the \
                     snapshot's frontier is not here to compare"
                        .to_owned(),
                ),
                Some(Kept::TooLarge) => Some(
                    "the projection is larger than a snapshot a read takes, or its frontier \
                     larger than one a pass reads back"
                        .to_owned(),
                ),
                None => None,
            });
            Outcome {
                name: file.file(),
                word: status.word,
                why: status.why,
                detail,
                recorded,
                problem,
            }
        })
        .collect()
}
