// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The materializer (docs/DESIGN-shared-files-2026-11-11.md §4): one pass projects this Mac's
//! replica of each shared file to its snapshot and face, and records the state word the pass
//! ended in. It is the only code that opens custody for a shared file, and it runs in two
//! places: the process that owns the Commons runtime, after a store grows
//! (`crate::alab::upkeep`), and `aterm file refresh`.
//!
//! A pass authors nothing, ingests nothing and persists no receipt: it opens the home
//! [`crate::generation::materializer_generation_until`] names as an observer, reads each table
//! family once, and writes only under `~/.aterm/files`. That walk and the check it opens under
//! ([`crate::generation::Observation::check_materialize`]) admit homes no other reader may
//! open, so both take a [`Projecting`], which only this module can make.
//!
//! - **Per table.** Each file's table is read on its own, so one that refuses (excluded,
//!   undeclared, damaged) holds back no other.
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
//! - **Nothing new, nothing projected.** A pass whose frontier is the snapshot's projects
//!   nothing again, unless it is asked to ([`Pass::again`]).
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
    },
};
use astream_wire::sync::Digest;
use aterm_digest::Sha256;
use aterm_file::{
    Column, File, Frontier, Kept, Name, Opened, Projected, Recorded, Row, Snapshot, Source, Why,
    Word, retain,
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

/// The version of the projection rules: a snapshot made under another version is projected
/// again whatever its frontier.
pub const PROJECTION: u32 = 4;

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
    /// Project again though nothing arrived: `refresh`, and the owner's occasional pass, which
    /// take up what changes without a record of the table (a membership record that makes an
    /// owned row readable).
    pub again: bool,
    /// This Mac's clock, Unix milliseconds, for display.
    pub now_ms: u64,
}

impl Pass {
    /// An ordinary pass starting now.
    #[must_use]
    pub fn now(again: bool) -> Self {
        Self {
            read_by: crate::admission::deadline(),
            vault_wait: VAULT_WAIT,
            again,
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
    /// What recording did; `None` where a local file could not be written (`problem`).
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
    /// The Commons machine uuid (32 hex) of the actor this pass reads as, where the session's
    /// members name it: the facts row at that key binds this Mac's machine row (`=here`).
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
    let actor = session.actor();
    let bindings = session.members().ok();
    let me = bindings.as_ref().and_then(|members| {
        members
            .iter()
            .find(|binding| binding.actor() == actor)
            .map(|binding| keys::uuid32(binding.machine()))
    });
    let members = bindings
        .iter()
        .flat_map(|members| members.iter())
        .map(|binding| keys::uuid32(binding.machine()))
        .collect();
    let read = action(&Custody {
        me,
        members,
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

/// One table family as one pass read it.
struct Family {
    compiled: &'static Compiled,
    frontier: Frontier,
    log: Log,
    view: std::cell::OnceCell<View>,
}

impl Family {
    /// The generation of `first`'s family this Mac's catalog says to read, opened and read.
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
        Ok(Self {
            compiled,
            frontier: Frontier {
                source: source.finalize(),
                era: custody.era,
                generation: compiled.generation,
                ids: log.retained_ids().copied().collect(),
            },
            log,
            view: std::cell::OnceCell::new(),
        })
    }

    fn view(&self, session: &Session<'_>) -> &View {
        self.view
            .get_or_init(|| View::read(self.compiled.table, session, &self.log))
    }
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
struct Ring {
    frontier: Frontier,
    ops: Vec<Op>,
    /// When each operation says it was authored, Unix milliseconds: for the history only.
    authored: BTreeMap<Digest, u64>,
    /// No retained record could still become eligible.
    complete: bool,
}

impl Ring {
    /// The window's tables at `now_s` (Unix seconds, this Mac's clock), opened and read.
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
        })
    }

    /// The frontier of a view folded from the ring's files: the ring's, under a source of its
    /// own, so the view and the file it folds are never taken for one another's reads.
    fn view_frontier(&self, view: &str) -> Frontier {
        let mut source = Sha256::new();
        source.update(b"aterm/file/source/view/1\0");
        source.update(self.frontier.source);
        source.update(view.as_bytes());
        Frontier {
            source: source.finalize(),
            ..self.frontier.clone()
        }
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

/// The address the `address` row `pk` says now, in the spelling the grid shows it; `None` where
/// the row is missing, conflicted to nothing, or says something this build refuses to show.
fn address_of(addresses: &Projection, pk: &str) -> Option<String> {
    let x = addresses.rows.get(pk)?.get("addr")?;
    let shown = grid::show(&Ty::Email, x);
    keys::address(&shown)
        .ok()
        .filter(|address| *address == shown)
}

fn text<'a>(row: &'a cell::Row, column: &str) -> Option<&'a str> {
    row.get(column).and_then(Value::as_str)
}

/// What the account files derive from the registry's other collections.
struct Accounts {
    /// Each account's claims address.
    email: BTreeMap<String, String>,
    /// Each account's login address, which a sign-in types: the claims address too unless an
    /// identity is set.
    login: BTreeMap<String, String>,
    slug: BTreeMap<String, String>,
    name: BTreeMap<String, String>,
}

impl Accounts {
    fn read(compiled: &Compiled, view: &View) -> Self {
        let accounts = view.project(&registry::ACCOUNT);
        let addresses = compiled
            .collection(registry::ADDRESS.name)
            .map(|collection| view.project(collection))
            .unwrap_or_default();
        let mut email = BTreeMap::new();
        let mut login = BTreeMap::new();
        for (pk, row) in &accounts.rows {
            if let Some(address) = text(row, "login").and_then(|ad| address_of(&addresses, ad)) {
                login.insert(pk.clone(), address);
            }
            // The claims address: the identity's when one is set, else the login's.
            let claims = text(row, "identity").or_else(|| text(row, "login"));
            if let Some(address) = claims.and_then(|ad| address_of(&addresses, ad)) {
                email.insert(pk.clone(), address);
            }
        }
        let rows: Vec<keys::Slugged<'_>> = accounts
            .rows
            .iter()
            .map(|(pk, row)| keys::Slugged {
                pk,
                realm: text(row, "realm").unwrap_or_default(),
                label: text(row, "label"),
                claims_email: email.get(pk).map(String::as_str),
                live: row.live(),
            })
            .collect();
        let slug = keys::account_slugs(&rows);
        let name = accounts
            .rows
            .iter()
            .filter_map(|(pk, row)| {
                let name = keys::account_name(text(row, "realm")?, email.get(pk)?)?;
                Some((pk.clone(), name))
            })
            .collect();
        Self {
            email,
            login,
            slug,
            name,
        }
    }
}

/// Each machine's label, by key, as a computed cell may carry it: the stored label where it is
/// one this build's guards admit, in the spelling the grid shows, and the grid's refusal text
/// ([`grid::REFUSED`]) in place of any other, which an older or modified build stored unchecked.
fn machine_labels(compiled: &Compiled, view: &View) -> BTreeMap<String, String> {
    let machine = compiled
        .collection(registry::MACHINE.name)
        .unwrap_or(&registry::MACHINE);
    let Some(column) = machine.column("label") else {
        return BTreeMap::new();
    };
    view.project(machine)
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
    grid::raw_shown(&Value::from(text)).as_str() == Some(text)
}

/// The computed cells of every row of `file`, by key then column.
fn computed(
    file: &Type,
    compiled: &Compiled,
    view: &View,
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
            let accounts = Accounts::read(compiled, view);
            let machines = machine_labels(compiled, view);
            // Each account's live links on live machines, smallest rank first, unranked last.
            let all = view.project(&registry::MACHINE);
            let mut links: BTreeMap<&str, Vec<(u64, &str)>> = BTreeMap::new();
            let preferences = view.project(&registry::PREFERENCE);
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
                put(pk, "name", accounts.name.get(pk));
                put(pk, "slug", accounts.slug.get(pk));
                put(pk, "email", accounts.email.get(pk));
                put(pk, "login_email", accounts.login.get(pk));
                if let Some(mut linked) = links.remove(pk.as_str()) {
                    linked.sort_unstable();
                    let names: Vec<&str> = linked.into_iter().map(|(_, name)| name).collect();
                    put(pk, "links", Some(&names.join(", ")));
                }
            }
        }
        Computed::Links => {
            let accounts = Accounts::read(compiled, view);
            let machines = machine_labels(compiled, view);
            for (pk, row) in &projection.rows {
                let account = text(row, "account").unwrap_or_default();
                put(pk, "account_slug", accounts.slug.get(account));
                put(pk, "account_name", accounts.name.get(account));
                put(
                    pk,
                    "machine_name",
                    machines.get(text(row, "machine").unwrap_or_default()),
                );
            }
        }
        Computed::Machines => {
            let empty = OwnedRead::default();
            let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
            let lab = LabView::read_view(
                view,
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
        }
    }
    out
}

/// Project `file` from `view`: its snapshot (the revision unset), how many cells were left
/// out for a value the guards refuse, and the baseline a write of this revision is read
/// against (each cell's candidates, [`grid::Base::to_json`]).
fn table(
    file: &Type,
    compiled: &'static Compiled,
    view: &View,
    me: Option<&str>,
    now_ms: u64,
) -> Result<(Snapshot, u64, Option<Vec<u8>>), Refusal> {
    let custody = |error: Error| Refusal::of(&error, Why::Custody);
    let Some((_, bound)) = file.table() else {
        return Err(Refusal::held(
            Why::Catalog,
            "the file is no catalog table's",
        ));
    };
    let collection = compiled.collection(bound.name).map_err(custody)?;
    let projection = view.project(collection);
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
        })
        .collect();
    columns.extend(file.computed.columns().iter().map(|name| Column {
        name: (*name).to_owned(),
        source: Source::Derived,
        read_only: true,
        refers: None,
    }));
    columns.push(Column {
        name: aterm_file::snapshot::PK.to_owned(),
        source: Source::Pk,
        read_only: true,
        refers: None,
    });
    let mut computed = computed(file, compiled, view, &projection, me);
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
            if seen.shown.contains(secret::MASK) {
                // A sealed value in a column a newer build wrote: never materialized.
                withheld += 1;
                continue;
            }
            let refused = seen.shown.contains(grid::REFUSED);
            withheld += u64::from(refused);
            if let (Some(schema), false, false) = (schema, refused, seen.raw)
                && let Some(value) = projected
                    .and_then(|row| row.fields.get(&column.name))
                    .and_then(|field| typed(schema, field))
            {
                values.insert(column.name.clone(), value);
            }
            cells.insert(column.name.clone(), seen.shown.clone());
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
    Ok((
        Snapshot {
            name: file.file(),
            kind: TABLE.to_owned(),
            ext: TSV.to_owned(),
            rev: 0,
            complete: view.complete,
            at_ms: now_ms,
            columns,
            handles: file.handles.iter().map(|name| (*name).to_owned()).collect(),
            rows,
        },
        withheld,
        // A baseline that cannot be written leaves the revision without one: a write of it
        // then asks for a refresh rather than guess what its author saw.
        aterm_json::to_vec(&export.base.to_json()).ok(),
    ))
}

/// A shown cell of a ring file and its typed value: the grid's spelling of `x` under `ty`, kept
/// as a value only where it is one the type admits and the guards let a surface show; else the
/// grid's refusal text, no value, and one more withheld cell.
fn ring_cell(ty: Option<&Ty>, x: &Value, withheld: &mut u64) -> Option<(String, Option<Value>)> {
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
    if !guarded(&shown) || shown.contains(grid::REFUSED) || shown.contains(secret::MASK) {
        *withheld += 1;
        return Some((grid::REFUSED.to_owned(), None));
    }
    Some((shown, admitted.then(|| x.clone())))
}

/// The face columns of a ring file: `schema`'s, then `unknown` (columns a newer build wrote),
/// then the computed ones, then the key; all read-only, since each Mac states its own rows and
/// nobody types into another's.
fn ring_columns(
    schema: &[(&str, Option<&'static str>)],
    unknown: &BTreeSet<String>,
    computed: &[&str],
) -> Vec<Column> {
    let mut columns: Vec<Column> = schema
        .iter()
        .map(|(name, refers)| Column {
            name: (*name).to_owned(),
            source: Source::Schema,
            read_only: true,
            refers: refers.map(str::to_owned),
        })
        .collect();
    columns.extend(unknown.iter().map(|name| Column {
        name: name.clone(),
        source: Source::Unknown,
        read_only: true,
        refers: None,
    }));
    columns.extend(computed.iter().map(|name| Column {
        name: (*name).to_owned(),
        source: Source::Derived,
        read_only: true,
        refers: None,
    }));
    columns.push(Column {
        name: aterm_file::snapshot::PK.to_owned(),
        source: Source::Pk,
        read_only: true,
        refers: None,
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

/// A ring collection's columns: each column's name and the type it shows, where it has one.
type Columns = Vec<(&'static str, Option<&'static str>)>;

/// Each Mac's rows of an owned collection of the ring, `read` already judged by their keys
/// (`owned::read_owned`: a row at a key that does not name its signer's machine is inert and
/// never here): one row per key, `<pk>.<uuid32>`, its columns the collection's. Returns the
/// columns, the rows and the cells withheld.
fn mine(collection: &Collection, read: &OwnedRead) -> (Columns, BTreeSet<String>, Vec<Row>, u64) {
    let schema: Vec<(&'static str, Option<&'static str>)> = collection
        .columns
        .iter()
        .filter(|column| column.live_at(collection.version))
        .map(|column| (column.name, refers(&column.ty)))
        .collect();
    let mut unknown = BTreeSet::new();
    let mut withheld = 0;
    let mut rows = Vec::with_capacity(read.rows.len());
    for (key, owned) in &read.rows {
        let mut row = Row {
            pk: keys::safe_key(key).to_owned(),
            live: true,
            ..Row::default()
        };
        let Some(object) = owned.value.as_object() else {
            continue;
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
            if let Some((shown, value)) = ring_cell(ty, x, &mut withheld) {
                row.cells.insert(name.clone(), shown);
                if let Some(value) = value {
                    row.values.insert(name.clone(), value);
                }
            }
        }
        rows.push(row);
    }
    (schema, unknown, rows, withheld)
}

/// The columns of `ai/events`: each event field under a readable name, its wire key and its
/// type. Times are Unix milliseconds, as every other time column of a shared file; an event
/// states seconds.
const EVENT_COLUMNS: [(&str, &str, Ty); 19] = [
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
        .map(|(name, _, ty)| (*name, refers(ty)))
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
                if let Some((shown, value)) = ring_cell(Some(ty), &x, &mut withheld) {
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
            entry.insert("revert".to_owned(), Value::from(op.revert));
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

/// Project a file of the journal ring from `ring`: its snapshot (the revision unset), the cells
/// withheld and the records inert. The rows the window holds now, with each row a previous
/// snapshot of this replica showed and the window no longer holds, until it ages out
/// ([`retain::carry`]: a Mac's row 120 days, an event 30, from when this Mac last saw it change).
fn ring_file(
    file: &Type,
    collection: &'static Collection,
    custody: &Custody<'_>,
    ring: &Ring,
    held: &File,
    now_ms: u64,
) -> (Snapshot, u64, u64) {
    let resolve = owned::commons::historical(custody.session);
    let (schema, unknown, read, withheld, inert, keep) = match collection.kind {
        Kind::Log => {
            let log = owned::read_log(collection, &ring.ops, &resolve);
            let (schema, rows, withheld, unread) = events(&log);
            let inert = u64::try_from(log.inert)
                .unwrap_or(u64::MAX)
                .saturating_add(unread);
            (schema, BTreeSet::new(), rows, withheld, inert, retain::LOG)
        }
        Kind::Owned | Kind::Cells => {
            let owned = owned::read_owned(collection, &ring.ops, &resolve);
            let (schema, unknown, rows, withheld) = mine(collection, &owned);
            let inert = u64::try_from(owned.inert).unwrap_or(u64::MAX);
            (schema, unknown, rows, withheld, inert, retain::MINE)
        }
    };
    let rows = retain::carry(&previous_rows(held, &ring.frontier), read, keep, now_ms);
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
/// snapshot a pass kept), naming each account by the registry `registry` read where it was
/// read, and preferring a current member's reading over its fenced predecessor's where the
/// registry binds both to one Mac. Returns the snapshot and the cells withheld.
fn limits_file(
    file: &Type,
    usage: &[Row],
    registry: Option<(&'static Compiled, &View)>,
    custody: &Custody<'_>,
    ring: &Ring,
    now_ms: u64,
) -> (Snapshot, u64) {
    let readings: Vec<Reading> = usage.iter().filter_map(Reading::of_row).collect();
    let (accounts, bound) = match registry {
        Some((compiled, view)) => {
            let empty = OwnedRead::default();
            let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
            let lab = LabView::read_view(
                view,
                owned(registry::MACHINE_FACTS.name),
                owned(registry::MAILBOX_OBS.name),
                custody.me.as_deref(),
            );
            (Some(Accounts::read(compiled, view)), lab.bound)
        }
        None => (None, BTreeMap::new()),
    };
    let seats = Seats {
        bound,
        current: custody.members.clone(),
    };
    let mut withheld = 0;
    let mut rows = Vec::new();
    for (pk, limit) in limits::fold(&readings, &seats) {
        let account = limit.account.as_deref().unwrap_or(pk.as_str());
        let slug = accounts.as_ref().and_then(|a| a.slug.get(account));
        let name = accounts.as_ref().and_then(|a| a.name.get(account));
        let mut row = limit.row(&pk, slug.map(String::as_str), name.map(String::as_str));
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
        })
        .chain(std::iter::once(Column {
            name: aterm_file::snapshot::PK.to_owned(),
            source: Source::Pk,
            read_only: true,
            refers: None,
        }))
        .collect();
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
    )
}

/// How many retained records of `file`'s collection `view` holds inert (§5): automatic records
/// past their writer's quota ([`crate::typed::owned::Quota::over`]), and, in an owned
/// collection, each row at a key that does not name its signer's machine or that is no bounded
/// object with a counter ([`OwnedRead::inert`]). They stay in history with their authors; the
/// status counts them. A file of the journal ring counts its own read's (`ring_file`): a row or
/// an entry at a key that is not its signer's, and an event this build's vocabulary does not
/// read.
pub(super) fn inert(file: &Type, view: &View) -> u64 {
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
    u64::try_from(over.saturating_add(owned)).unwrap_or(u64::MAX)
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
            entry.insert("revert".to_owned(), Value::from(op.revert));
            entry.insert("keys".to_owned(), Value::Array(keys));
            Some((ms, op.id, Value::Object(entry)))
        })
        .collect();
    entries.sort_unstable_by_key(|(ms, id, _)| (*ms, *id));
    entries.into_iter().map(|(_, _, entry)| entry).collect()
}

/// What one pass read for `file` from a replica at `frontier`, whose last recorded pass left
/// `held`: nothing projected again where the frontier is the snapshot's (`project` is not run),
/// else what `project` makes of it, the cells it withheld and the records it holds inert.
fn opened(
    held: &File,
    pass: &Pass,
    frontier: &Frontier,
    project: impl FnOnce() -> Result<(Projected, u64, u64), Refusal>,
) -> Result<Opened, Refusal> {
    let fingerprint = aterm_codec::hex::encode_lower(&frontier.fingerprint());
    // The status describes the snapshot at the revision it records. One at another revision
    // was written by a pass killed before its status: it is projected again, and that pass
    // records the revision. So is one that does not say its inert count, which an older build
    // leaves out when it rewrites a status a newer one wrote: the count is unknown, never 0.
    let kept = held
        .state()
        .filter(|state| {
            !pass.again
                && state.rev > 0
                && state.fingerprint == fingerprint
                && state.projection == PROJECTION
                && held
                    .published()
                    .snapshot
                    .is_some_and(|snapshot| snapshot.rev == state.rev)
        })
        .and_then(|state| state.inert.map(|inert| (state.withheld, inert)));
    let (projected, withheld, inert) = if let Some((withheld, inert)) = kept {
        (None, withheld, inert)
    } else {
        let (projected, withheld, inert) = project()?;
        (Some(projected), withheld, inert)
    };
    Ok(Opened {
        // Set once the pass has read the home's standing again (`with_custody`).
        newest: false,
        frontier: frontier.clone(),
        projection: PROJECTION,
        withheld,
        inert,
        projected,
    })
}

/// What one pass read for a file of a catalog table, from its family's read.
fn opened_table(
    custody: &Custody<'_>,
    family: &Family,
    file: &Type,
    held: &File,
    pass: &Pass,
) -> Result<Opened, Refusal> {
    opened(held, pass, &family.frontier, || {
        let view = family.view(custody.session);
        let (snapshot, withheld, baseline) = table(
            file,
            family.compiled,
            view,
            custody.me.as_deref(),
            pass.now_ms,
        )?;
        Ok((
            Projected {
                snapshot,
                history: history(file, family, view, held),
                baseline,
            },
            withheld,
            inert(file, view),
        ))
    })
}

/// The family of `first` this pass read, read now where no file before asked for it.
fn family_of<'a>(
    families: &'a mut Vec<(&'static str, Result<Family, Refusal>)>,
    custody: &Custody<'_>,
    first: &'static Compiled,
) -> Result<&'a Family, Refusal> {
    let role = first.table.role;
    if !families.iter().any(|(read, _)| *read == role) {
        families.push((role, Family::read(custody, first)));
    }
    match families.iter().find(|(read, _)| *read == role) {
        Some((_, Ok(family))) => Ok(family),
        Some((_, Err(refusal))) => Err(refusal.clone()),
        None => Err(Refusal::held(Why::Custody, "the table was not read")),
    }
}

/// What one pass read for every file of `files`, in order, under `custody`: a catalog table's
/// files from its family, read once; the ring's files from one read of its window; and a view
/// from the files it folds, as this pass projected them or as the snapshot a pass kept.
fn read_files(
    custody: &Custody<'_>,
    root: &aterm_file::Root,
    files: &[(&'static Type, File, u64)],
    pass: &Pass,
) -> Vec<Result<Opened, Refusal>> {
    let mut families: Vec<(&'static str, Result<Family, Refusal>)> = Vec::new();
    let mut ring: Option<Result<Ring, Refusal>> = None;
    // The rows this pass projected, by file: what a view later in the list folds.
    let mut projected: BTreeMap<&'static str, Vec<Row>> = BTreeMap::new();
    let mut out = Vec::with_capacity(files.len());
    for (file, held, _) in files {
        let read = match file.store {
            Store::Table { family: first, .. } => family_of(&mut families, custody, first)
                .and_then(|family| opened_table(custody, family, file, held, pass)),
            Store::Journal(collection) => {
                match ring.get_or_insert_with(|| Ring::read(custody, pass.now_ms / 1000)) {
                    Err(refusal) => Err(refusal.clone()),
                    Ok(ring) => opened(held, pass, &ring.frontier, || {
                        let (snapshot, withheld, inert) =
                            ring_file(file, collection, custody, ring, held, pass.now_ms);
                        Ok((
                            Projected {
                                history: ring_history(collection.name, ring),
                                snapshot,
                                baseline: None,
                            },
                            withheld,
                            inert,
                        ))
                    }),
                }
            }
            Store::View(super::Fold::Limits) => {
                match ring.get_or_insert_with(|| Ring::read(custody, pass.now_ms / 1000)) {
                    Err(refusal) => Err(refusal.clone()),
                    Ok(ring) => {
                        let frontier = ring.view_frontier(file.name);
                        // The names a view shows come from the registry; a registry that
                        // refuses leaves them blank and folds the readings all the same.
                        let registry = family_of(&mut families, custody, &att::REGISTRY)
                            .ok()
                            .map(|family| (family.compiled, family.view(custody.session)));
                        opened(held, pass, &frontier, || {
                            let usage =
                                projected
                                    .get(super::USAGE.name)
                                    .cloned()
                                    .unwrap_or_else(|| {
                                        File::at(root, &super::USAGE.file())
                                            .published()
                                            .snapshot
                                            .map(|snapshot| snapshot.rows)
                                            .unwrap_or_default()
                                    });
                            let (snapshot, withheld) =
                                limits_file(file, &usage, registry, custody, ring, pass.now_ms);
                            Ok((
                                Projected {
                                    snapshot,
                                    history: Vec::new(),
                                    baseline: None,
                                },
                                withheld,
                                0,
                            ))
                        })
                    }
                }
            }
        };
        if let Ok(Opened {
            projected: Some(projection),
            ..
        }) = &read
        {
            projected.insert(file.name, projection.snapshot.rows.clone());
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
    let files: Vec<(&'static Type, File, u64)> = super::FILES
        .iter()
        .filter(|file| only.is_none_or(|only| *only == file.file()))
        .map(|file| {
            let held = File::at(&root, &file.file());
            // Noted before custody opens: a pass that another overtakes records nothing.
            let look = held.look();
            (file, held, look)
        })
        .collect();
    let read = with_custody(&home, config, pass, |custody| {
        read_files(custody, &root, &files, pass)
    });
    let read: Vec<Result<Opened, Refusal>> = match read {
        Ok((read, newest)) => read
            .into_iter()
            .map(|opened| {
                opened.map(|mut opened| {
                    opened.newest = newest;
                    opened
                })
            })
            .collect(),
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
            let (recorded, problem) = match held.record(look, recording, pass.now_ms) {
                // The snapshot now holds what this Mac published: its markers say so, and a
                // read stops laying those writes over it.
                Ok(recorded) => (
                    Some(recorded),
                    held.settle_done().err().map(|error| error.to_string()),
                ),
                Err(error) => (None, Some(error.to_string())),
            };
            let detail = detail.or_else(|| match recorded {
                Some(Recorded::Kept(Kept::Behind)) => Some(
                    "the replica read is behind the snapshot: an earlier home, one without a \
                     record the snapshot was projected from, or a fenced home where the \
                     snapshot's frontier is not here to compare"
                        .to_owned(),
                ),
                Some(Recorded::Kept(Kept::TooLarge)) => Some(
                    "the projection is larger than a snapshot a read takes, or its frontier \
                     larger than one a pass reads back"
                        .to_owned(),
                ),
                _ => None,
            });
            let status = held.status();
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
