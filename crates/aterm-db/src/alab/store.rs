// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `alab/registry` on Commons: declared in its reserved catalog slot (`catalog::reserved`),
//! read as ATT/1 with the per-Mac quota applied, seeded after a complete scan, and given its
//! schema rows by an owner verb only.

use super::{
    checks::Finding,
    discover::{self, Listing},
    register::{self, Input, Me, Outcome, Probe, Registered},
    registry::{APPLICATION, MACHINE_FACTS, MAILBOX_OBS, REGISTRY},
    seeds,
    view::LabView,
};
use crate::{
    Error, Result,
    att::{Compiled, View, decode, pack},
    catalog::Descriptor,
    database::Command,
    documents::{application, log::Log},
    machines::Label,
    table::{Scope, Session},
    typed::{self, keys},
};
use astream_wire::sync::Digest;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::{collections::BTreeSet, path::Path};

/// The SHA-256 fingerprint of the registry operations a [`LabView`] was projected from
/// (`replica::InventorySummary`'s encoding), so a cache can tell whether anything arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inventory(pub Digest);

/// What [`Registry::lab`] read.
#[derive(Clone, Debug)]
pub enum LabRead {
    /// This Mac's history of the registry still waits on a record (an operation whose
    /// predecessor or parent has not arrived, or a sealed table's new generation before its cover
    /// row): its rows are not all here yet, so this says nothing of the lab and the caller keeps
    /// what it holds.
    Partial,
    /// The registry's operations are the ones the caller's view was projected from: nothing is
    /// projected again.
    Unchanged,
    /// The view of the registry as it is now, and the inventory it was projected from.
    Projected(Box<LabView>, Inventory),
}

/// What one read of the registry gives the harness ([`Registry::lab`]): a partial history where
/// it is not `complete`, nothing new where its operations' `inventory` is the caller's `held`,
/// else the view `project` makes of it. Each is read only where it decides.
pub(super) fn lab_read(
    complete: bool,
    inventory: impl FnOnce() -> Inventory,
    held: Option<Inventory>,
    project: impl FnOnce() -> Result<LabView>,
) -> Result<LabRead> {
    if !complete {
        return Ok(LabRead::Partial);
    }
    let inventory = inventory();
    if held == Some(inventory) {
        return Ok(LabRead::Unchanged);
    }
    Ok(LabRead::Projected(Box::new(project()?), inventory))
}

/// The declaration of the registry generation `compiled`.
fn descriptor(
    compiled: &'static Compiled,
) -> impl FnOnce(&Bootstrap) -> Result<Descriptor> + use<> {
    move |bootstrap| compiled.descriptor(bootstrap.trust())
}

/// Whether this Mac's quota of automatic records, less `reserve` records kept for another writer
/// path, can take one more record and the causal joins it may need, joins an interrupted append
/// left included (design §3.9).
fn quota_room(session: &Session<'_>, log: &Log, view: &View, reserve: usize) -> bool {
    let actor = session.actor();
    let spent = view.quota.spent.get(&actor).copied().unwrap_or(0);
    let last_business = view
        .ops
        .iter()
        .filter(|op| op.actor == actor)
        .map(|op| op.sequence)
        .max()
        .unwrap_or(0);
    let pending = log.pending_merges_after(actor, last_business);
    log.append_records_bound().saturating_add(pending)
        <= REGISTRY
            .self_quota
            .saturating_sub(reserve)
            .saturating_sub(spent)
}

/// The Commons uuids of the current members.
fn live_uuids(session: &Session<'_>) -> Result<BTreeSet<String>> {
    Ok(session
        .members()?
        .iter()
        .map(|binding| keys::uuid32(binding.machine()))
        .collect())
}

/// The migration label a row of this Mac carries: its historical roster label, else the log's
/// Mac's own recipient, the first that is a label. Nothing else names a Mac here: a host name is
/// not an identity, however it reads.
pub(super) fn chosen_label(logged: Option<String>, roster: Option<String>) -> Option<String> {
    [roster, logged]
        .into_iter()
        .flatten()
        .find_map(|label| Label::parse(&label).ok())
        .map(|label| label.canonical())
}

/// The non-denied roster label and positive admission timestamp of `recipient`, when `roster`
/// holds one.
pub(super) fn roster_identity(
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
    recipient: &str,
) -> (Option<String>, Option<u64>) {
    let Some(member) = roster.and_then(|r| {
        r.member_by_recipient(recipient)
            .filter(|m| !r.is_member_denied(m))
    }) else {
        return (None, None);
    };
    (
        Some(member.label.clone()),
        Some(member.admitted_ms).filter(|ms| *ms > 0),
    )
}

/// Who this enrollment is: its uuid from its binding, its label ([`chosen_label`]), and when the
/// roster says it was admitted. Never `machine.toml`.
fn me(
    session: &Session<'_>,
    trust: aterm_secrets::commons::journal::Trust,
    probe: &Probe,
) -> Result<Me> {
    let members = session.members()?;
    let actor = session.actor();
    let binding = members
        .iter()
        .find(|binding| binding.actor() == actor)
        .ok_or_else(|| Error::new("this Mac is not in its own Commons roster"))?;
    let paths = session.paths();
    let logged = aterm_secrets::client::trusted_commons_log(paths, trust)
        .ok()
        .flatten()
        .and_then(|(log, _)| log.membership_ref().label(&actor).map(str::to_owned));
    let recipient = binding.recipient().to_bech32();
    let roster = aterm_secrets::client::swarm_roster_until(paths, crate::admission::deadline())
        .map_err(crate::table::failure)?;
    let (rostered, enrolled_ms) = roster_identity(roster.as_ref(), &recipient);
    Ok(Me {
        uuid: keys::uuid32(binding.machine()),
        label: chosen_label(logged, rostered),
        hw_hint: keys::hw_hint(&trust.network, &probe.platform_uuid),
        enrolled_ms,
    })
}

fn quiet(outcome: Outcome) -> Registered {
    Registered {
        outcome,
        findings: Vec::new(),
        records: 0,
    }
}

/// Copy the legacy label snapshot once. The source marker is appended last, so a quota stop
/// can resume the existing idempotent importer; after completion even owner edits stay put.
fn roster_migration(
    session: &Session<'_>,
    scope: Scope,
    table: &typed::Table,
    view: &View,
    now_ms: u64,
) -> Result<Vec<Vec<crate::database::Change>>> {
    use super::{
        import,
        registry::{MACHINE, SOURCE},
    };
    use crate::typed::cell::{self, CellKey, CellValue};
    use aterm_json::json;
    const MARKER: &[u8] = b"aterm/alab/roster-label-migration/1";
    let marker = keys::source_pk(MARKER);
    if cell::project(&SOURCE, &view.cells)
        .rows
        .contains_key(&marker)
    {
        return Ok(Vec::new());
    }
    let Some(roster) = aterm_secrets::client::historical_swarm_roster(session.paths())
        .map_err(crate::table::failure)?
    else {
        return Ok(Vec::new());
    };
    if roster.members.is_empty() {
        return Ok(Vec::new());
    }
    struct LabelsOnly;
    impl import::Vendors for LabelsOnly {
        fn mark(&self, _provider: &str, _cell: &str, _notes: &str) -> import::Mark {
            import::Mark::Absent
        }
    }
    let macs = super::command::known_macs_from_roster(&roster);
    let name = format!("{}/{}", table.application, table.role);
    let sealer = crate::att::sealer(&name, session, scope)?;
    let input = import::Input {
        sheets: &[],
        macs: &macs,
        vendors: &LabelsOnly,
        sealer: &sealer,
        now_ms,
        births: true,
        unread: &[],
    };
    let plan = import::plan(&input, &view.cells)?;
    // The importer leaves owner corrections intact and records source disagreements. A
    // disputed label never grants membership and must not stop unrelated Macs registering.
    let mut rows = plan.rows;
    let machines = view.project(&MACHINE);
    for change in rows.iter_mut().flatten() {
        if change.collection == MACHINE.name
            && let Ok(key) = CellKey::parse(&change.key)
            && key.col == "status"
            && !machines.rows.contains_key(key.pk)
            && let Some(value) = change
                .value
                .as_mut()
                .and_then(aterm_json::Value::as_object_mut)
        {
            value.insert("x".into(), json!("planned"));
        }
    }
    let mut completed = Vec::new();
    for (col, x) in [
        ("kind", json!("swarm-roster")),
        ("name", json!("one-mesh roster label migration")),
        (
            "sha256",
            json!(aterm_codec::hex::encode_lower(
                &aterm_digest::Sha256::digest(MARKER)
            )),
        ),
        ("rows", json!(macs.len())),
        ("at", json!(now_ms)),
        (typed::LIFECYCLE, json!("live")),
    ] {
        completed.push(cell::change(
            &SOURCE,
            CellKey {
                pk: &marker,
                col,
                elem: None,
            },
            &CellValue::automatic(SOURCE.version, x),
        )?);
    }
    rows.push(completed);
    Ok(rows)
}

/// The plan for this Mac over a complete read of the registry.
fn planned(
    session: &Session<'_>,
    trust: aterm_secrets::commons::journal::Trust,
    view: &View,
    probe: &Probe,
    heard: bool,
    now_ms: u64,
) -> Result<register::Plan> {
    let me = me(session, trust, probe)?;
    let live = live_uuids(session)?;
    let input = Input {
        me: &me,
        probe,
        live: &live,
        heard,
        now_ms,
    };
    let empty = crate::typed::owned::OwnedRead::default();
    let facts = view.owned.get(MACHINE_FACTS.name).unwrap_or(&empty);
    register::plan(&input, &view.cells, facts)
}

/// What one discovery pass decided and wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Discovered {
    pub outcome: discover::Outcome,
    /// By key, never by value.
    pub findings: Vec<Finding>,
    /// Records appended: 0 or 1.
    pub records: usize,
}

/// What a seed pass wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seeded {
    /// Records appended now.
    pub records: usize,
    /// Records an earlier pass of this Mac already wrote with the same content.
    pub retried: usize,
}

/// `alab/registry` on this Mac's Commons.
pub struct Registry {
    paths: Paths,
}

impl Registry {
    pub fn at(paths: Paths) -> Self {
        Self { paths }
    }

    /// This Mac's vault paths.
    pub(crate) const fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The registry generation this Mac reads and writes now (`att::current`): `alab/registry`
    /// until a roll seals it, then `alab/registry.2`.
    pub fn generation(&self) -> &'static Compiled {
        crate::att::current(&self.paths, &crate::att::REGISTRY)
    }

    /// Declare the registry, whose catalog slot is reserved, and enroll it under this Mac's
    /// writer. Reopening coalesces with an existing declaration.
    ///
    /// # Errors
    /// No Commons writer, no storage room at all, or an incompatible declaration.
    pub fn open(&self) -> Result<Scope> {
        let compiled = self.generation();
        application::with_store(&self.paths, true, descriptor(compiled), |_, _, scope| {
            Ok(scope)
        })
    }

    /// Read the registry this Mac holds, without enrolling it.
    ///
    /// # Errors
    /// No Commons, the registry is not declared or not subscribed here, or its storage is
    /// damaged.
    pub fn view(&self) -> Result<View> {
        crate::att::Handle::at(self.paths.clone(), &crate::att::REGISTRY).view()
    }

    /// [`Self::view`] for a read a person waits on, which may read until `read_by` while each wait
    /// for a holder stays the ordinary admission's: the registry's generation
    /// ([`crate::att::Handle::at_until`]) and then its rows ([`crate::att::Handle::view_until`]).
    ///
    /// # Errors
    /// As [`Self::view`], or the read ran out of time at `read_by`.
    pub fn view_until(&self, read_by: std::time::Instant) -> Result<View> {
        crate::att::Handle::at_until(self.paths.clone(), &crate::att::REGISTRY, read_by)?
            .view_until(read_by)
    }

    /// Write the compiled provider, CLI and plan rows ([`seeds`]) into every cell a complete
    /// scan of the registry finds absent, under this Mac's quota of automatic records: each
    /// row in one record, each record with a content-derived request id, so a re-run writes
    /// nothing and a retry returns its first receipt.
    ///
    /// # Errors
    /// No Commons writer; the registry cannot be read or written; retained history could still
    /// reveal a held cell; or the next seed and its causal joins exceed this Mac's remaining
    /// automatic-record quota.
    pub fn seed(&self) -> Result<Seeded> {
        self.seed_until(crate::admission::deadline())
    }

    /// [`Self::seed`] held to `deadline`, for a command a person runs and waits on.
    ///
    /// # Errors
    /// As [`Self::seed`], and `deadline` passes first.
    pub fn seed_until(&self, deadline: std::time::Instant) -> Result<Seeded> {
        let compiled = self.generation();
        application::with_store_until(
            &self.paths,
            true,
            descriptor(compiled),
            deadline,
            |session, store, scope| {
                session.writer()?;
                let mut seeded = Seeded::default();
                let mut log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let mut view = View::read(compiled.table, session, &log);
                if !view.complete {
                    return Err(Error::new(
                        "the registry's history here is incomplete; seeds wait for a complete scan",
                    ));
                }
                let packed = pack(seeds::rows_for(&view.cells)?)?;
                let packed_len = packed.len();
                for (index, changes) in packed.into_iter().enumerate() {
                    let request = typed::request_id(APPLICATION, compiled.table.role, &changes)?;
                    let command = Command::Write { request, changes };
                    if log.retry(session.actor(), &command)?.is_some() {
                        seeded.retried += 1;
                        continue;
                    }
                    if !quota_room(session, &log, &view, 0) {
                        return Err(Error::new(
                            "this Mac's remaining automatic quota cannot fit the next registry seed \
                         and its causal joins",
                        ));
                    }
                    application::append(session, store, &command.encode()?)?;
                    seeded.records += 1;
                    if index + 1 < packed_len {
                        // Re-read before admitting another append (`Log::append_records_bound`).
                        log = Log::read_with(session, store, scope, decode)?;
                        view = View::read(compiled.table, session, &log);
                    }
                }
                Ok(seeded)
            },
        )
    }

    /// The registry's table id here: what a repair round and `[commons] exclude_tables` name it
    /// by. `None` with no Commons at all.
    ///
    /// # Errors
    /// This Mac's custody cannot be read.
    pub fn table(&self) -> Result<Option<Digest>> {
        let compiled = self.generation();
        let Some(bootstrap) = Bootstrap::cached(&self.paths).map_err(crate::table::failure)? else {
            return Ok(None);
        };
        Ok(Some(compiled.descriptor(bootstrap.trust())?.scope.digest()))
    }

    /// Whether `scope` is the table of one of the registry's generations under its own trust.
    /// The registry's table here ([`Self::table`]) is one of these under this Mac's trust, so a
    /// scope that is none of them is not that table under any trust, which a round's witness of
    /// it says without the reads of custody and the catalog [`Self::table`] makes.
    #[must_use]
    pub fn is_generation(scope: Scope) -> bool {
        crate::att::REGISTRY.chain().into_iter().any(|compiled| {
            compiled
                .descriptor(scope.trust())
                .is_ok_and(|descriptor| descriptor.scope == scope)
        })
    }

    /// What this Mac's catalog says of the registry: the catalog's table id, and whether a Mac
    /// other than this one declared the registry in it. Only an owner's import declares it, so a
    /// declaration from another Mac says the lab holds the registry. A Mac that holds it holds
    /// that declaration, so where none is here a peer's fresh round over the catalog that brought
    /// nothing says that peer holds none of the registry (the import's empty-view guard,
    /// `alab::command`). `None` where there is no Commons here. `deadline` bounds the read, an
    /// import's own pass rather than an admission's, since a loaded Mac's catalog read can outrun
    /// the latter.
    ///
    /// # Errors
    /// Custody, membership or the catalog's storage cannot be read by `deadline`.
    pub fn catalog_word(&self, deadline: std::time::Instant) -> Result<Option<(Digest, bool)>> {
        Ok(
            application::peek_declaration(&self.paths, descriptor(self.generation()), deadline)?
                .map(|declaration| (declaration.catalog, declaration.elsewhere)),
        )
    }

    /// Whether this Mac's catalog declares the registry: what "imported" means to a Mac whose
    /// read of it failed. `false` where there is no Commons here.
    ///
    /// # Errors
    /// Custody, membership or the catalog's storage cannot be read.
    pub fn declared(&self) -> Result<bool> {
        Ok(application::peek_declaration(
            &self.paths,
            descriptor(self.generation()),
            crate::admission::deadline(),
        )?
        .is_some_and(|declaration| declaration.declared))
    }

    /// The Commons uuids of the current members, what `check` calls live.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn live_uuids(&self) -> Result<BTreeSet<String>> {
        let compiled = self.generation();
        application::observe_store(&self.paths, descriptor(compiled), |session, _, _| {
            live_uuids(session)
        })
    }

    /// One self-registration pass (design §6): write this Mac's facts row, and the machine and
    /// model rows it needs in the same record, only on a change or an old `seen`, within this
    /// Mac's quota. It reads `config` first and writes nothing when `[commons]` turns sharing off or
    /// excludes the registry or the catalog, and it never declares the registry, which an owner's
    /// import does and a repair round brings. `heard` is that a repair round left nothing to fetch
    /// from a peer holding the registry ([`crate::runtime::RepairRound::drained_table`]).
    ///
    /// # Errors
    /// `config` is not a valid file; this Mac holds no catalog yet or is no Writer; the registry
    /// cannot be read or written; or a reading breaks its column's rules.
    pub fn register(
        &self,
        config: &Path,
        probe: &Probe,
        heard: bool,
        now_ms: u64,
    ) -> Result<Registered> {
        let compiled = self.generation();
        let trust = match self.admitted(config)? {
            Ok(trust) => trust,
            Err(outcome) => return Ok(quiet(outcome)),
        };
        application::with_store(
            &self.paths,
            false,
            descriptor(compiled),
            |session, store, scope| {
                session.writer()?;
                let mut log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let mut view = View::read(compiled.table, session, &log);
                if !view.complete {
                    return Ok(quiet(Outcome::Incomplete));
                }
                let mut migrated = 0;
                if heard {
                    let rows = roster_migration(session, scope, compiled.table, &view, now_ms)?;
                    for changes in pack(rows)? {
                        if !quota_room(session, &log, &view, 0) {
                            return Ok(Registered {
                                outcome: Outcome::QuotaSpent,
                                findings: Vec::new(),
                                records: migrated,
                            });
                        }
                        let request =
                            typed::request_id(APPLICATION, compiled.table.role, &changes)?;
                        let command = Command::Write { request, changes };
                        if log.retry(session.actor(), &command)?.is_none() {
                            application::append(session, store, &command.encode()?)?;
                            migrated += 1;
                            log = Log::read_with(session, store, scope, decode)?;
                            view = View::read(compiled.table, session, &log);
                        }
                    }
                }
                let plan = planned(session, trust, &view, probe, heard, now_ms)?;
                if plan.changes.is_empty() {
                    return Ok(Registered {
                        outcome: plan.outcome,
                        findings: plan.findings,
                        records: migrated,
                    });
                }
                let request = typed::request_id(APPLICATION, compiled.table.role, &plan.changes)?;
                let command = Command::Write {
                    request,
                    changes: plan.changes,
                };
                if log.retry(session.actor(), &command)?.is_some() {
                    return Ok(Registered {
                        outcome: plan.outcome,
                        findings: plan.findings,
                        records: migrated,
                    });
                }
                if !quota_room(session, &log, &view, 0) {
                    return Ok(Registered {
                        outcome: Outcome::QuotaSpent,
                        findings: plan.findings,
                        records: migrated,
                    });
                }
                application::append(session, store, &command.encode()?)?;
                Ok(Registered {
                    outcome: plan.outcome,
                    findings: plan.findings,
                    records: migrated + 1,
                })
            },
        )
    }

    /// One discovery pass over an open mailbox's address list (design §4.10, step 10): this Mac's
    /// `mailbox-obs` row and the birth of the address being signed in, in one record within
    /// discovery's share of this Mac's quota ([`discover::REGISTRATION_RESERVE`]), only where the
    /// row's content changed ([`discover::plan`]). It reads `[commons]` first, as a registration pass does, and never
    /// declares the registry. The mailbox is the one keyed from the row whose `addr` says its
    /// login now, or from the key the login derives where no row says it and none holds that key
    /// ([`keys::mailbox_pk`]).
    ///
    /// # Errors
    /// `config` is not a valid file; this Mac holds no catalog yet or is no Writer; or the
    /// registry cannot be read or written.
    pub fn observe_mailbox(
        &self,
        config: &Path,
        listing: &Listing,
        now_ms: u64,
    ) -> Result<Discovered> {
        let compiled = self.generation();
        let quiet = |outcome| Discovered {
            outcome,
            findings: Vec::new(),
            records: 0,
        };
        if let Err(outcome) = self.admitted(config)? {
            return Ok(quiet(match outcome {
                Outcome::Excluded => discover::Outcome::Excluded,
                _ => discover::Outcome::NotHeld,
            }));
        }
        application::with_store(
            &self.paths,
            false,
            descriptor(compiled),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let view = View::read(compiled.table, session, &log);
                if !view.complete {
                    return Ok(quiet(discover::Outcome::Incomplete));
                }
                let actor = session.actor();
                let uuid = session
                    .members()?
                    .iter()
                    .find(|binding| binding.actor() == actor)
                    .map(|binding| keys::uuid32(binding.machine()))
                    .ok_or_else(|| Error::new("this Mac is not in its own Commons roster"))?;
                let empty = crate::typed::owned::OwnedRead::default();
                let obs = view.owned.get(MAILBOX_OBS.name).unwrap_or(&empty);
                let input = discover::Input {
                    uuid: &uuid,
                    listing,
                    now_ms,
                };
                let discover::Plan {
                    outcome,
                    findings,
                    changes,
                } = discover::plan(&input, &view.cells, obs)?;
                if changes.is_empty() {
                    return Ok(Discovered {
                        outcome,
                        findings,
                        records: 0,
                    });
                }
                let request = typed::request_id(APPLICATION, compiled.table.role, &changes)?;
                let command = Command::Write { request, changes };
                if log.retry(session.actor(), &command)?.is_some() {
                    return Ok(Discovered {
                        outcome,
                        findings,
                        records: 0,
                    });
                }
                if !quota_room(session, &log, &view, discover::REGISTRATION_RESERVE) {
                    return Ok(Discovered {
                        outcome: discover::Outcome::QuotaSpent,
                        findings,
                        records: 0,
                    });
                }
                application::append(session, store, &command.encode()?)?;
                Ok(Discovered {
                    outcome,
                    findings,
                    records: 1,
                })
            },
        )
    }

    /// What a pass that has heard a repair round would decide now, writing nothing: how
    /// `aterm db alab check` answers why this Mac has no row, which a held Mac's registry cannot
    /// show because there is no row to read. It reads as an observer, which cannot test a writer's
    /// standing, so a Mac that is no Writer reads what it would decide; the pass's own refusal is
    /// in its log line.
    ///
    /// # Errors
    /// As [`Self::register`], but for being no Writer.
    pub fn preview(&self, config: &Path, probe: &Probe, now_ms: u64) -> Result<Registered> {
        let compiled = self.generation();
        let trust = match self.admitted_policy(config)? {
            Ok(trust) => trust,
            Err(outcome) => return Ok(quiet(outcome)),
        };
        let seen = application::observe_store_if_declared(
            &self.paths,
            descriptor(compiled),
            |session, store, scope| {
                let log = Log::read_with(session, store, scope, decode)?;
                let view = View::read(compiled.table, session, &log);
                if !view.complete {
                    return Ok(quiet(Outcome::Incomplete));
                }
                let plan = planned(session, trust, &view, probe, true, now_ms)?;
                Ok(Registered {
                    outcome: plan.outcome,
                    findings: plan.findings,
                    records: 0,
                })
            },
        )?;
        Ok(seen.unwrap_or_else(|| quiet(Outcome::NotHeld)))
    }

    /// The registry as the harness reads it ([`LabView`]), with the inventory it was projected
    /// from, as an observer that writes nothing: `None` where there is no Commons, `[commons]`
    /// excludes the registry or the catalog, or the registry is not declared here, and
    /// [`LabRead::Partial`] where this Mac's history of it still waits on a record (a partial
    /// history reads as rows that are not there yet). Where the registry's inventory is still
    /// `held`, the one the caller's view was projected from, it reads the log and its view to
    /// learn so and projects nothing ([`LabRead::Unchanged`]).
    ///
    /// # Errors
    /// This Mac's custody, its `[commons]` policy or the registry's storage cannot be read.
    pub fn lab(&self, config: &Path, held: Option<Inventory>) -> Result<Option<LabRead>> {
        let compiled = self.generation();
        if self.admitted_policy(config)?.is_err() {
            return Ok(None);
        }
        let seen = application::observe_store_if_declared(
            &self.paths,
            descriptor(compiled),
            |session, store, scope| {
                let log = Log::read_with(session, store, scope, decode)?;
                let view = View::read(compiled.table, session, &log);
                let inventory = || {
                    let ids: BTreeSet<Digest> = view.ops.iter().map(|op| op.id).collect();
                    Inventory(crate::replica::InventorySummary::of(&ids).fingerprint)
                };
                let project = || {
                    let actor = session.actor();
                    let uuid = session
                        .members()?
                        .iter()
                        .find(|binding| binding.actor() == actor)
                        .map(|binding| keys::uuid32(binding.machine()));
                    let empty = crate::typed::owned::OwnedRead::default();
                    let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
                    Ok(LabView::read(
                        &view.cells,
                        owned(MACHINE_FACTS.name),
                        owned(MAILBOX_OBS.name),
                        uuid.as_deref(),
                    ))
                };
                lab_read(view.complete, inventory, held, project)
            },
        )?;
        Ok(seen)
    }

    /// This Mac's trust, or the outcome that ends a pass when there is no Commons or `[commons]`
    /// turns sharing off or excludes the registry or the catalog.
    fn admitted_policy(
        &self,
        config: &Path,
    ) -> Result<std::result::Result<aterm_secrets::commons::journal::Trust, Outcome>> {
        let compiled = self.generation();
        let Some(bootstrap) = Bootstrap::cached(&self.paths).map_err(crate::table::failure)? else {
            return Ok(Err(Outcome::NotHeld));
        };
        let trust = bootstrap.trust();
        let table = compiled.descriptor(bootstrap.trust())?.scope.digest();
        let catalog = crate::catalog::Directory::new(&self.paths, trust)
            .catalog_scope()
            .digest();
        let policy = crate::catalog::policy::Policy::read(config)?;
        if !policy.enabled || policy.excluded.contains(&table) || policy.excluded.contains(&catalog)
        {
            return Ok(Err(Outcome::Excluded));
        }
        Ok(Ok(trust))
    }

    /// What every pass reads first: this Mac's trust, or the outcome that ends the pass, when
    /// there is no Commons, `[commons]` turns sharing off or excludes the registry or the catalog,
    /// or the registry is not declared here.
    fn admitted(
        &self,
        config: &Path,
    ) -> Result<std::result::Result<aterm_secrets::commons::journal::Trust, Outcome>> {
        let compiled = self.generation();
        let trust = match self.admitted_policy(config)? {
            Ok(trust) => trust,
            Err(outcome) => return Ok(Err(outcome)),
        };
        if application::observe_store_if_declared(&self.paths, descriptor(compiled), |_, _, _| {
            Ok(())
        })?
        .is_none()
        {
            return Ok(Err(Outcome::NotHeld));
        }
        Ok(Ok(trust))
    }

    /// Publish the compiled schema rows (design §3.5), an owner verb: only where the stored
    /// row is absent or below the compiled version (`att::Handle::publish_schema`).
    ///
    /// # Errors
    /// No Commons writer, or the registry cannot be read or written.
    pub fn publish_schema(&self) -> Result<usize> {
        crate::att::Handle::at(self.paths.clone(), &crate::att::REGISTRY).publish_schema()
    }

    /// Append `records` as this Mac, one record each (tests drive owner edits, births and
    /// owned rows with it).
    #[cfg(any(test, debug_assertions))]
    pub fn write(&self, records: Vec<Vec<crate::database::Change>>) -> Result<()> {
        let compiled = self.generation();
        application::with_store(
            &self.paths,
            true,
            descriptor(compiled),
            |session, store, _| {
                session.writer()?;
                for changes in records {
                    let command = Command::Write {
                        request: crate::crypto::random()?,
                        changes,
                    };
                    application::append(session, store, &command.encode()?)?;
                }
                Ok(())
            },
        )
    }
}
