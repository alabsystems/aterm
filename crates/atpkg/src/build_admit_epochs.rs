// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original epoch custody beside root jobs, never inside their finish/drop path.

use super::{Event, Owner};
use crate::build_admit::{Ended, Held};
use aterm_capacity::{
    build_cpu::RetainedLease,
    cache_work::Collection,
    epoch_controller::{Config, EpochController, RootCancellation, RootOutcome, Status},
    epoch_recipe::{MAX_CAPTURED_ROOTS, RootOutput, RootRecipe},
    epoch_target::TargetLoan,
    host_protocol::{Epoch, Failure, MAX_CACHE_WORK, MAX_JOBS, RootCacheWork, Snapshot},
    leadership::Fence,
    tree_meter::TreeMeter,
};
use aterm_uds::ready::Watch;
use std::{
    collections::{BTreeMap, VecDeque},
    fmt, io,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_EPOCHS: usize = 16;
const FINISH_RETRY: Duration = Duration::from_millis(100);

fn denied(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why)
}

/// A branded local ticket. Decoding a generation, number or observation cannot
/// construct it or acquire process/resource control. History remains read-only.
#[derive(Clone, Debug)]
pub struct EpochId {
    table: Arc<()>,
    sequence: u64,
}

impl EpochId {
    /// Display sequence, unique only inside the actual original owner table.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

impl PartialEq for EpochId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.table, &other.table) && self.sequence == other.sequence
    }
}
impl Eq for EpochId {}

/// The controller is already retained by its owner, including after a failed
/// native launch/BIND/publication. Keep this ticket for recovery and observation.
#[derive(Debug)]
pub struct EpochAccepted {
    pub id: EpochId,
    /// Dispatch/publication outcome. Success acknowledges actual dispatch, not
    /// durable BIND or readiness. No failure retracts stored custody.
    pub result: io::Result<()>,
}

/// Refusal before a child birth, retaining every original admission input.
#[derive(Debug)]
pub struct EpochRefused {
    error: io::Error,
    inputs: Box<(Held, Vec<RetainedLease>, Config)>,
}

impl EpochRefused {
    fn new(error: io::Error, held: Held, cpu: Vec<RetainedLease>, config: Config) -> Self {
        Self {
            error,
            inputs: Box::new((held, cpu, config)),
        }
    }
    /// Why admission refused without consuming original custody.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }
    /// Recover the exact original claim, actual CPU capabilities and configuration.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, Held, Vec<RetainedLease>, Config) {
        let (held, cpu, config) = *self.inputs;
        (self.error, held, cpu, config)
    }
}
impl fmt::Display for EpochRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "host epoch admission refused: {}", self.error)
    }
}
impl std::error::Error for EpochRefused {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

struct Entry {
    controller: EpochController,
    ended: Ended,
    meter: Option<TreeMeter>,
    backing: u32,
    roots: u32,
    reserved_bytes: u64,
    failure: Option<Failure>,
    next_recovery: Option<Instant>,
    next_finish: Option<Instant>,
    quarantined: bool,
}

impl Entry {
    fn retry_binding(&mut self, fence: &Fence) -> io::Result<()> {
        let unbound = self.controller.status().unbound;
        let result = self.controller.retry_binding(fence);
        if !unbound {
            return result;
        }
        let status = self.controller.status();
        self.next_recovery = (result.is_err() && status.unbound && !status.faulted)
            .then(|| Instant::now() + FINISH_RETRY);
        result
    }
    fn cancel_not_spawned(&mut self, fence: &Fence) -> io::Result<()> {
        let status = self.controller.status();
        let cancellable = status.not_spawned || status.cancellation_started;
        let result = self.controller.cancel_not_spawned(fence);
        if !cancellable {
            return result;
        }
        self.next_recovery = None;
        self.next_finish = None;
        if result.is_err() {
            let status = self.controller.status();
            let retry = Instant::now() + FINISH_RETRY;
            if status.cancellation_started {
                // The actual receipt may already belong to Claim even when
                // its first durable write fails. Retry that same closure.
                self.next_finish = Some(retry);
            } else if status.not_spawned && !status.faulted {
                self.next_recovery = Some(retry);
            }
        }
        result
    }
    fn note(&mut self, error: &io::Error) {
        // The most recent closed category explains the current refusal. Actual
        // root failure outcomes and the original accepted launch error survive
        // independently; neither becomes success after resource completion.
        self.failure = Some(Failure::of(error));
    }
    fn row(&self, sequence: u64) -> Epoch {
        row(
            sequence,
            self.backing,
            self.roots,
            self.reserved_bytes,
            self.controller.status(),
            self.failure,
        )
    }
}

struct Completed {
    row: Epoch,
    status: Status,
    outcomes: Vec<Option<RootOutcome>>,
    cancellations: Vec<Option<RootCancellation>>,
    helper: Option<u32>,
    outputs: BTreeMap<u32, RootOutput>,
}

pub(super) struct Table {
    seal: Arc<()>,
    next: u64,
    limit: usize,
    history_limit: usize,
    active: BTreeMap<u64, Entry>,
    history: VecDeque<Completed>,
}

fn row(
    sequence: u64,
    backing: u32,
    roots: u32,
    reserved_bytes: u64,
    status: Status,
    failure: Option<Failure>,
) -> Epoch {
    Epoch {
        sequence,
        backing,
        roots,
        reserved_bytes,
        opened: status.opened,
        retired: status.retired,
        helper_exited: status.helper_exited,
        control_eof: status.control_eof,
        endpoint_cleaned: status.endpoint_cleaned,
        settled_units: status.settled_units as u32,
        uncertain_units: status.uncertain_units as u32,
        remaining_units: status.remaining_units as u32,
        completed_roots: status.completed_roots as u32,
        failed_roots: status.failed_roots as u32,
        faulted: status.faulted,
        unbound: status.unbound,
        not_spawned: status.not_spawned,
        cancellation_started: status.cancellation_started,
        failure,
        cache_work: Vec::new(),
    }
}

impl Table {
    fn captured_roots(&self) -> usize {
        self.active
            .values()
            .map(|entry| entry.controller.captured_root_count())
            .sum::<usize>()
            + self
                .history
                .iter()
                .map(|entry| entry.outputs.len())
                .sum::<usize>()
    }
    pub(super) fn new(max_jobs: usize, history: usize) -> Self {
        Self {
            seal: Arc::new(()),
            next: 1,
            limit: max_jobs.min(MAX_EPOCHS),
            history_limit: history.min(MAX_EPOCHS),
            active: BTreeMap::new(),
            history: VecDeque::new(),
        }
    }
    pub(super) fn active_len(&self) -> usize {
        self.active.len()
    }
    fn check_id(&self, id: &EpochId) -> io::Result<()> {
        if Arc::ptr_eq(&self.seal, &id.table) {
            Ok(())
        } else {
            Err(denied("epoch ticket belongs to another owner table"))
        }
    }
    fn missing(&self, id: &EpochId) -> io::Error {
        if self
            .history
            .iter()
            .any(|entry| entry.row.sequence == id.sequence)
        {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "epoch is settled; retained history is read-only",
            )
        } else {
            io::Error::new(
                io::ErrorKind::NotFound,
                "epoch ticket is no longer retained",
            )
        }
    }
    fn find_mut(&mut self, id: &EpochId) -> io::Result<&mut Entry> {
        self.check_id(id)?;
        if !self.active.contains_key(&id.sequence) {
            return Err(self.missing(id));
        }
        Ok(self
            .active
            .get_mut(&id.sequence)
            .expect("checked original epoch"))
    }
    pub(super) fn rows(&self, budget: usize) -> Vec<Epoch> {
        let retained = budget
            .saturating_sub(self.active.len())
            .min(self.history.len());
        let mut rows: Vec<_> = self
            .history
            .iter()
            .skip(self.history.len() - retained)
            .map(|entry| entry.row.clone())
            .chain(
                self.active
                    .iter()
                    .map(|(&sequence, entry)| entry.row(sequence)),
            )
            .collect();
        rows.sort_by_key(|entry| entry.sequence);
        rows
    }
    pub(super) fn tick(&mut self, fence: &Fence, say: &mut dyn FnMut(&str)) {
        if self.active.is_empty() {
            return;
        }
        let mut complete = Vec::new();
        let leadership = fence.check_current();
        for (&sequence, entry) in &mut self.active {
            if let Err(error) = &leadership {
                entry.quarantined = true;
                entry.next_recovery = None;
                entry.next_finish = None;
                entry.note(error);
            }
            if entry.quarantined {
                // A revoked fence never becomes usable again. Keep original
                // custody and actual-child reap checks, but no permanently
                // writable/buffered waits or futile settlement timer.
                if let Err(error) = entry.controller.poll(fence) {
                    entry.note(&error);
                }
                continue;
            }
            let status = entry.controller.status();
            if (status.not_spawned || status.unbound)
                && !status.faulted
                && entry.next_recovery.is_none_or(|at| Instant::now() >= at)
            {
                let recovery = if status.unbound {
                    entry.retry_binding(fence)
                } else {
                    entry.cancel_not_spawned(fence)
                };
                if let Err(error) = recovery {
                    entry.note(&error);
                    // Observing the original owned child's reap is safe even
                    // while its BIND is blocked. No pipe or finish action runs.
                    if let Err(error) = entry.controller.poll(fence) {
                        entry.note(&error);
                    }
                    // Do not retry a failed cancellation again through try_finish
                    // in the same tick after its receipt changed hands.
                    continue;
                }
            }
            if let Err(error) = entry.controller.poll(fence) {
                entry.note(&error);
            }
            let plan = entry.controller.readiness();
            let now = Instant::now();
            if plan.retry_finish && (plan.immediate || entry.next_finish.is_none_or(|at| now >= at))
            {
                if let Err(error) = entry.controller.try_finish(fence)
                    && error.kind() != io::ErrorKind::WouldBlock
                {
                    entry.note(&error);
                }
                let plan = entry.controller.readiness();
                entry.next_finish =
                    (plan.retry_finish && !plan.immediate).then(|| Instant::now() + FINISH_RETRY);
            } else if !plan.retry_finish {
                entry.next_finish = None;
            }
            if entry.controller.is_settled() && !entry.controller.status().faulted {
                complete.push(sequence);
            }
        }
        for sequence in complete {
            let entry = self
                .active
                .remove(&sequence)
                .expect("fully settled original epoch");
            let final_row = entry.row(sequence);
            let Entry {
                mut controller,
                ended,
                meter,
                roots,
                ..
            } = entry;
            let status = controller.status();
            let outcomes = (0..roots).map(|id| controller.root_outcome(id)).collect();
            let cancellations = (0..roots)
                .map(|id| controller.root_cancellation(id))
                .collect();
            let helper = controller.helper_id();
            let outputs = (0..roots)
                .filter_map(|id| {
                    controller
                        .take_root_output(id)
                        .expect("fully settled original output")
                        .map(|output| (id, output))
                })
                .collect();
            drop(controller);
            let peak = meter.map_or(0, |meter| meter.stop(0));
            let _ = ended.keep(
                crate::build_admit::BuildEnd::of_roots(
                    usize::try_from(roots).unwrap_or(usize::MAX),
                    status.completed_roots,
                    status.failed_roots,
                ),
                peak,
                say,
            );
            if self.history.len() == self.history_limit {
                self.history.pop_front();
            }
            self.history.push_back(Completed {
                row: final_row,
                status,
                outcomes,
                cancellations,
                helper,
                outputs,
            });
        }
    }
    pub(super) fn wait_plan(&self, until: Instant) -> (Vec<Watch<'_>>, bool, Instant) {
        let mut watches = Vec::with_capacity(self.active.len() * 2);
        let mut immediate = false;
        let mut until = until;
        let now = Instant::now();
        for entry in self.active.values() {
            if entry.quarantined {
                continue;
            }
            let plan = entry.controller.readiness();
            watches.extend(plan.interests());
            immediate |= plan.immediate;
            let status = entry.controller.status();
            if (status.not_spawned || status.unbound) && !status.faulted {
                let at = entry.next_recovery.unwrap_or(now);
                immediate |= at <= now;
                until = until.min(at);
            }
            if plan.retry_finish {
                let at = entry.next_finish.unwrap_or(now);
                immediate |= at <= now;
                until = until.min(at);
            }
        }
        (watches, immediate, until)
    }
}

impl Owner {
    /// Accept original native admission and all actual retained CPU capabilities.
    /// Construction refuses with untouched inputs. Once accepted, the controller
    /// is inserted before dispatch and its ticket survives every later error.
    /// Ticks durably bind the original child before observing helper readiness.
    /// This private owner does not create a CPU pool or migrate account supply.
    /// # Errors
    /// Foreign/unfenced/stale owner, invalid inputs, full/exhausted table or
    /// unavailable/oversized publication before any helper birth.
    pub fn submit_epoch(
        &mut self,
        held: Held,
        cpu: Vec<RetainedLease>,
        config: Config,
    ) -> Result<EpochAccepted, EpochRefused> {
        self.submit_epoch_with_recipes(held, cpu, config, Vec::new())
    }

    /// Admit original retained resources with explicit genuine-Targo recipes.
    /// The owner retains at most sixteen roots' output handles across active
    /// epochs and history. Consume history output to make room for later work.
    /// This does not migrate the default build route or create any CPU supply.
    /// # Errors
    /// The same refusals as [`Self::submit_epoch`], invalid recipes or exhausted
    /// output reservations. Every refusal preserves original resource inputs.
    pub fn submit_epoch_with_recipes(
        &mut self,
        held: Held,
        cpu: Vec<RetainedLease>,
        config: Config,
        recipes: Vec<RootRecipe>,
    ) -> Result<EpochAccepted, EpochRefused> {
        let ready = (|| {
            self.check_owner()?;
            RootRecipe::validate_set(&recipes, config.roots)?;
            if self.epochs.captured_roots() + recipes.len() > MAX_CAPTURED_ROOTS {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "host captured-root output reservations are full",
                ));
            }
            let fence = self
                .fence
                .as_ref()
                .ok_or_else(|| denied("epochs require original fenced owner"))?;
            fence.check_claim(held.claim())?;
            if !held.cohort {
                return Err(denied("epoch requires native cohort admission"));
            }
            if self.epochs.active.len() >= self.epochs.limit
                || self.jobs.len() + self.epochs.active.len() >= MAX_JOBS
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "host epoch table is full",
                ));
            }
            self.epochs
                .next
                .checked_add(1)
                .ok_or_else(|| io::Error::other("epoch sequence exhausted"))?;
            let jobs = self.jobs.protocol_rows()?;
            let mut epochs = self.epochs.rows(MAX_JOBS - jobs.len() - 1);
            // Preflight the exact bounded observation shape before consuming
            // original inputs or allowing any native birth.
            let n = u32::try_from(cpu.len()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "epoch CPU count is unrepresentable",
                )
            })?;
            epochs.push(Epoch {
                sequence: self.epochs.next,
                backing: n,
                roots: config.roots,
                reserved_bytes: held.claim().record().need.mem,
                opened: false,
                retired: false,
                helper_exited: false,
                control_eof: false,
                endpoint_cleaned: false,
                settled_units: 0,
                uncertain_units: 0,
                remaining_units: n,
                completed_roots: 0,
                failed_roots: 0,
                faulted: false,
                unbound: false,
                not_spawned: false,
                cancellation_started: false,
                failure: None,
                cache_work: Vec::new(),
            });
            let state = self
                .observer
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.exhausted || state.snapshot.revision == u64::MAX {
                return Err(io::Error::other(
                    "host publication exhausted; epoch refused",
                ));
            }
            Snapshot {
                generation: self.generation,
                revision: state.snapshot.revision,
                closed: false,
                jobs,
                epochs,
            }
            .encode()?;
            Ok(())
        })();
        if let Err(error) = ready {
            return Err(EpochRefused::new(error, held, cpu, config));
        }
        let (claim, ended) = held.into_epoch_parts();
        let backing = cpu.len() as u32;
        let roots = config.roots;
        let controller = match EpochController::new_with_recipes(
            self.fence.as_ref().expect("checked original fence"),
            claim,
            cpu,
            config,
            recipes,
        ) {
            Ok(controller) => controller,
            Err(error) => {
                let (error, claim, cpu, config) = error.into_parts();
                let held = Held {
                    claim,
                    key: ended.key,
                    own: ended.own,
                    cohort: true,
                };
                return Err(EpochRefused::new(error, held, cpu, config));
            }
        };
        let id = EpochId {
            table: Arc::clone(&self.epochs.seal),
            sequence: self.epochs.next,
        };
        self.epochs.next += 1;
        self.epochs.active.insert(
            id.sequence,
            Entry {
                controller,
                reserved_bytes: ended.claimed,
                ended,
                meter: None,
                backing,
                roots,
                failure: None,
                next_recovery: None,
                next_finish: None,
                quarantined: false,
            },
        );
        let entry = self
            .epochs
            .active
            .get_mut(&id.sequence)
            .expect("accepted before dispatch");
        let dispatch = entry
            .controller
            .dispatch(self.fence.as_ref().expect("checked original fence"));
        if let Err(error) = &dispatch {
            entry.note(error);
        }
        entry.meter = entry.controller.helper_id().map(TreeMeter::start);
        let publication = self.publish(Vec::new());
        Ok(EpochAccepted {
            id,
            result: dispatch.and(publication),
        })
    }

    fn epoch_action(
        &mut self,
        id: &EpochId,
        action: impl FnOnce(&mut Entry, &Fence) -> io::Result<()>,
    ) -> io::Result<()> {
        self.check_owner()?;
        let fence = self
            .fence
            .as_ref()
            .ok_or_else(|| denied("epoch owner has no original fence"))?;
        fence.check_current()?;
        let entry = self.epochs.find_mut(id)?;
        let result = action(entry, fence);
        if let Err(error) = &result {
            entry.note(error);
        }
        result.and(self.publish(vec![(None, Event::StateChanged)]))
    }
    /// Queue one root using the actual owner's fence and stable caller target.
    /// # Errors
    /// Foreign/stale/aged-out/settled ticket, unavailable root or protocol queue.
    pub fn epoch_spawn_root(
        &mut self,
        id: &EpochId,
        root: u32,
        manifest: &Path,
        target: &Path,
    ) -> io::Result<()> {
        self.epoch_action(id, |entry, fence| {
            entry.controller.spawn_root(fence, root, manifest, target)
        })
    }
    /// Queue one root while preserving its prepared artifact target exclusion.
    /// # Errors
    /// Foreign/stale/settled ticket, unavailable root, changed loan or full queue.
    pub fn epoch_spawn_root_with_target(
        &mut self,
        id: &EpochId,
        root: u32,
        manifest: &Path,
        target: &Path,
        loan: TargetLoan,
    ) -> io::Result<()> {
        self.epoch_action(id, |entry, fence| {
            entry
                .controller
                .spawn_root_with_target(fence, root, manifest, target, loan)
        })
    }
    /// Seal new roots through the original private helper's control pipe.
    /// # Errors
    /// Foreign/stale/aged-out/settled ticket, unopened/closing/faulted epoch or queue full.
    pub fn epoch_close(&mut self, id: &EpochId) -> io::Result<()> {
        self.epoch_action(id, |entry, fence| entry.controller.close(fence))
    }
    /// Request cooperative root cancellation through this original owner's pipe.
    /// Existing active work drains; no process signal or CPU release is implied.
    /// # Errors
    /// Foreign/stale/settled ticket, unknown/unopened/faulted root or queue failure.
    pub fn epoch_cancel_root(&mut self, id: &EpochId, root: u32) -> io::Result<RootCancellation> {
        let mut state = None;
        self.epoch_action(id, |entry, fence| {
            state = Some(entry.controller.cancel_root(fence, root)?);
            Ok(())
        })?;
        state.ok_or_else(|| io::Error::other("root cancellation state missing after action"))
    }
    /// Complete or retry the original actual child's BIND without new birth.
    /// Ticks also drive this operation; an explicit failure shares their pace.
    /// # Errors
    /// Foreign/stale/aged-out/settled ticket or original binding/pipe failure.
    pub fn epoch_retry_binding(&mut self, id: &EpochId) -> io::Result<()> {
        self.epoch_action(id, Entry::retry_binding)
    }
    /// Transfer the original genuine no-child receipt for durable cancellation.
    /// Ticks also initiate this automatically, retry the original closure and
    /// settle at most one unit. A manual failed attempt shares that retry pace.
    /// # Errors
    /// Foreign/stale/aged-out/settled ticket, absent proof or durable cancellation failure.
    pub fn epoch_cancel_not_spawned(&mut self, id: &EpochId) -> io::Result<()> {
        self.epoch_action(id, Entry::cancel_not_spawned)
    }
    /// Read current or bounded settled-history state, never release authority.
    /// # Errors
    /// Foreign owner/ticket or history no longer retained.
    pub fn epoch_status(&self, id: &EpochId) -> io::Result<Status> {
        self.check_owner()?;
        self.epochs.check_id(id)?;
        if let Some(entry) = self.epochs.active.get(&id.sequence) {
            return Ok(entry.controller.status());
        }
        self.epochs
            .history
            .iter()
            .find(|entry| entry.row.sequence == id.sequence)
            .map(|entry| entry.status)
            .ok_or_else(|| self.epochs.missing(id))
    }
    /// Read one validated root result, including a failure after safe settlement.
    /// # Errors
    /// Foreign owner/ticket or history no longer retained.
    pub fn epoch_root_outcome(&self, id: &EpochId, root: u32) -> io::Result<Option<RootOutcome>> {
        self.check_owner()?;
        self.epochs.check_id(id)?;
        if let Some(entry) = self.epochs.active.get(&id.sequence) {
            return Ok(entry.controller.root_outcome(root));
        }
        self.epochs
            .history
            .iter()
            .find(|entry| entry.row.sequence == id.sequence)
            .map(|entry| entry.outcomes.get(root as usize).copied().flatten())
            .ok_or_else(|| self.epochs.missing(id))
    }

    /// Record one closed cache-work collection for an original completed root.
    /// The bounded summary is observation only; it never changes eligibility,
    /// resource custody, settlement or artifact acceptance. A binding is one-use.
    /// # Errors
    /// Foreign/stale owner or ticket, unresolved/unknown root, duplicate binding
    /// or run, exhausted publication, or an invalid/oversized observation.
    pub fn epoch_record_cache_work(
        &mut self,
        id: &EpochId,
        root: u32,
        collection: &Collection,
    ) -> io::Result<()> {
        self.check_owner()?;
        self.fence
            .as_ref()
            .ok_or_else(|| denied("cache-work publication requires original owner fence"))?
            .check_current()?;
        self.epochs.check_id(id)?;
        if self.epochs.active.contains_key(&id.sequence) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "cache-work root is not settled",
            ));
        }
        let position = self
            .epochs
            .history
            .iter()
            .position(|entry| entry.row.sequence == id.sequence)
            .ok_or_else(|| self.epochs.missing(id))?;
        let entry = &self.epochs.history[position];
        if !entry
            .outcomes
            .get(root as usize)
            .copied()
            .flatten()
            .is_some_and(|outcome| outcome.id == root)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cache-work root has no original completion",
            ));
        }
        if entry
            .row
            .cache_work
            .iter()
            .any(|record| record.root == root)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "cache-work root binding is already recorded",
            ));
        }
        if self
            .epochs
            .history
            .iter()
            .map(|entry| entry.row.cache_work.len())
            .sum::<usize>()
            >= MAX_CACHE_WORK
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "cache-work observation capacity is full",
            ));
        }
        let record = RootCacheWork::from_collection(root, collection);
        if let Some(summary) = &record.summary {
            summary.validate()?;
            if self
                .epochs
                .history
                .iter()
                .flat_map(|entry| &entry.row.cache_work)
                .filter_map(|record| record.summary.as_ref())
                .any(|previous| previous.run_id == summary.run_id)
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cache-work run is already bound",
                ));
            }
        }
        // Preflight the actual bounded observation before touching history.
        // Collection failures and large reports cannot poison resource frames.
        let jobs = self.jobs.protocol_rows()?;
        let mut epochs = self.epochs.rows(MAX_JOBS - jobs.len());
        let row = epochs
            .iter_mut()
            .find(|entry| entry.sequence == id.sequence)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "cache-work epoch is outside published history",
                )
            })?;
        row.cache_work.push(record.clone());
        let state = self
            .observer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.exhausted || state.snapshot.revision == u64::MAX || state.snapshot.closed {
            return Err(io::Error::other("cache-work publication unavailable"));
        }
        Snapshot {
            generation: self.generation,
            revision: state.snapshot.revision,
            closed: false,
            jobs,
            epochs,
        }
        .encode()?;
        drop(state);
        self.epochs.history[position].row.cache_work.push(record);
        if let Err(error) = self.publish(vec![(None, Event::StateChanged)]) {
            self.epochs.history[position].row.cache_work.pop();
            return Err(error);
        }
        Ok(())
    }
    /// Move one settled root's original output handles to its explicit consumer.
    /// History eviction may discard unread output; taking it twice returns None.
    /// Native failure/cancellation remain diagnostic data, not artifact acceptance.
    /// # Errors
    /// Foreign owner/ticket, unresolved original epoch, or history no longer retained.
    pub fn epoch_take_root_output(
        &mut self,
        id: &EpochId,
        root: u32,
    ) -> io::Result<Option<RootOutput>> {
        self.check_owner()?;
        self.epochs.check_id(id)?;
        if let Some(entry) = self.epochs.active.get_mut(&id.sequence) {
            return entry.controller.take_root_output(root);
        }
        if let Some(entry) = self
            .epochs
            .history
            .iter_mut()
            .find(|entry| entry.row.sequence == id.sequence)
        {
            return Ok(entry.outputs.remove(&root));
        }
        Err(self.epochs.missing(id))
    }
    /// Read retained cancellation intent/driver acknowledgement, never authority.
    /// # Errors
    /// Foreign owner/ticket or history no longer retained.
    pub fn epoch_root_cancellation(
        &self,
        id: &EpochId,
        root: u32,
    ) -> io::Result<Option<RootCancellation>> {
        self.check_owner()?;
        self.epochs.check_id(id)?;
        if let Some(entry) = self.epochs.active.get(&id.sequence) {
            return Ok(entry.controller.root_cancellation(root));
        }
        self.epochs
            .history
            .iter()
            .find(|entry| entry.row.sequence == id.sequence)
            .map(|entry| entry.cancellations.get(root as usize).copied().flatten())
            .ok_or_else(|| self.epochs.missing(id))
    }
    /// Root-level acceptance hint; live activation also requires a current-source
    /// generation fence. This is no artifact or resource publication capability.
    /// # Errors
    /// Foreign owner/ticket or history no longer retained.
    pub fn epoch_root_eligible_for_activation(&self, id: &EpochId, root: u32) -> io::Result<bool> {
        Ok(self.epoch_root_cancellation(id, root)?.is_none()
            && self
                .epoch_root_outcome(id, root)?
                .is_some_and(|outcome| outcome.success))
    }
    /// Original actual helper PID for diagnostics, not a signal/release capability.
    /// # Errors
    /// Foreign owner/ticket or history no longer retained.
    pub fn epoch_helper_id(&mut self, id: &EpochId) -> io::Result<Option<u32>> {
        self.check_owner()?;
        self.epochs.check_id(id)?;
        if let Some(entry) = self.epochs.active.get_mut(&id.sequence) {
            return Ok(entry.controller.helper_id());
        }
        self.epochs
            .history
            .iter()
            .find(|entry| entry.row.sequence == id.sequence)
            .map(|entry| entry.helper)
            .ok_or_else(|| self.epochs.missing(id))
    }
}

#[cfg(test)]
mod cache_work_tests {
    use super::*;
    use aterm_capacity::{cache_work::UnavailableReason, cohort_namespace::Namespace};
    use std::os::unix::fs::DirBuilderExt as _;

    fn completed(sequence: u64) -> Completed {
        // A private table fixture tests binding/publication, not native closure.
        // The maintained Owner probe supplies actual child/settlement evidence.
        let status = Status {
            opened: true,
            retired: true,
            helper_exited: true,
            control_eof: true,
            endpoint_cleaned: true,
            settled_units: 2,
            uncertain_units: 0,
            remaining_units: 0,
            completed_roots: 2,
            failed_roots: 0,
            faulted: false,
            unbound: false,
            not_spawned: false,
            cancellation_started: false,
        };
        Completed {
            row: row(sequence, 2, 2, 42, status, None),
            status,
            outcomes: (0..2)
                .map(|id| {
                    Some(RootOutcome {
                        id,
                        pid: 1,
                        code: Some(0),
                        success: true,
                    })
                })
                .collect(),
            cancellations: vec![None, None],
            helper: None,
            outputs: BTreeMap::new(),
        }
    }

    fn owner() -> (aterm_tempfile::TempDir, Owner) {
        let world = aterm_tempfile::tempdir().unwrap();
        let home = world.path().join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(world.path(), &home).unwrap();
        let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
        let owner = Owner::fenced(fence, 1, 16).unwrap();
        (world, owner)
    }

    #[test]
    fn original_completed_binding_publishes_once_without_changing_resource_outcomes() {
        let (_world, mut owner) = owner();
        owner.epochs.history.push_back(completed(1));
        owner.publish(vec![(None, Event::StateChanged)]).unwrap();
        let id = EpochId {
            table: Arc::clone(&owner.epochs.seal),
            sequence: 1,
        };
        let status = owner.epoch_status(&id).unwrap();
        let outcome = owner.epoch_root_outcome(&id, 0).unwrap();
        let before = owner.observer().read(None).unwrap().snapshot;
        let unavailable = Collection::Unavailable(UnavailableReason::MissingSession);
        owner.epoch_record_cache_work(&id, 0, &unavailable).unwrap();
        let after = owner.observer().read(None).unwrap().snapshot;
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.revision, before.revision + 1);
        assert_eq!(
            after.epochs[0].cache_work,
            vec![RootCacheWork::from_collection(0, &unavailable)]
        );
        assert_eq!(owner.epoch_status(&id).unwrap(), status);
        assert_eq!(owner.epoch_root_outcome(&id, 0).unwrap(), outcome);
        assert!(owner.is_empty());
        assert_eq!(
            owner
                .epoch_record_cache_work(&id, 0, &unavailable)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            owner.observer().read(None).unwrap().snapshot.revision,
            after.revision
        );
        let foreign = EpochId {
            table: Arc::new(()),
            sequence: 1,
        };
        assert_eq!(
            owner
                .epoch_record_cache_work(&foreign, 1, &unavailable)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            owner
                .epoch_record_cache_work(&id, 2, &unavailable)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        owner.epochs.history.clear();
        assert_eq!(
            owner
                .epoch_record_cache_work(&id, 1, &unavailable)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn summary_capacity_refusal_preserves_the_exact_last_resource_snapshot() {
        let (_world, mut owner) = owner();
        for sequence in 1..=MAX_CACHE_WORK as u64 {
            let mut entry = completed(sequence);
            entry.row.cache_work.push(RootCacheWork::from_collection(
                0,
                &Collection::Unavailable(UnavailableReason::NotRecorded),
            ));
            owner.epochs.history.push_back(entry);
        }
        owner.publish(vec![(None, Event::StateChanged)]).unwrap();
        let before = owner.observer().read(None).unwrap().snapshot;
        let id = EpochId {
            table: Arc::clone(&owner.epochs.seal),
            sequence: 1,
        };
        let result = owner.epoch_record_cache_work(
            &id,
            1,
            &Collection::Unavailable(UnavailableReason::QueryFailed),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(*owner.observer().read(None).unwrap().snapshot, *before);
        assert!(before.encode().unwrap().len() < aterm_capacity::host_protocol::MAX_FRAME_BYTES);
    }
}
