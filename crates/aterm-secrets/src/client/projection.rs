// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One subscriber's ordinary converge job, shared by HTTP rounds and local member progress.

use super::{Converging, SourceSaid, SyncOutcome, converge_after_pass};
use crate::converge::{Due, Last, RefreshObservation, RepositoryRequest, RepositoryWork};
use aterm_uds::ready::Doorbell;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The automatic runtime that owns a local readiness observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvergenceRole {
    /// The vault subscriber also owns executable-local worker upkeep.
    Subscriber,
    /// The node service already owns its worker upkeep for its whole lifetime.
    Node,
    /// The GUI already owns its workers and wakes its existing convergence lane on input events.
    Window,
}

/// Canonical identity input debt, independent of repository admission and success receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IdentityDebt {
    #[default]
    None,
    Changed,
}

/// One input observation; none of these facts grants the runner an effect permission.
pub struct ConvergenceSnapshot {
    pub refresh: RefreshObservation,
    pub repositories: Option<RepositoryRequest>,
    pub identity: IdentityDebt,
}

struct IdentitySignal {
    generation: u64,
    wake: Option<Arc<aterm_uds::stop::Stop>>,
}
static IDENTITY_CHANGED: Mutex<IdentitySignal> = Mutex::new(IdentitySignal {
    generation: 0,
    wake: None,
});

/// A consuming view of the fixed identity-input signal. Existing file readers notify it;
/// no watcher, worker or target payload is registered here.
pub struct ConvergenceWake {
    seen: u64,
}
impl Default for ConvergenceWake {
    fn default() -> Self {
        Self {
            seen: IDENTITY_CHANGED
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .generation,
        }
    }
}
impl ConvergenceWake {
    /// Arm atomically with the generation check. Err supplies pending input, not cancellation.
    pub fn arm(&mut self) -> Result<Arc<aterm_uds::stop::Stop>, u64> {
        let mut signal = IDENTITY_CHANGED
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if signal.generation != self.seen {
            return Err(signal.generation);
        }
        Ok(Arc::clone(signal.wake.get_or_insert_with(|| {
            Arc::new(aterm_uds::stop::Stop::new())
        })))
    }
    /// The owner retained this generation's debt; a newer event remains pending.
    pub fn acknowledge(&mut self, generation: u64) {
        self.seen = generation;
    }
}

/// An executable's readiness observer and optional local worker upkeep, on an
/// automatic runtime's existing lane. Readiness starts no helper or network request and
/// grants no authority to the resulting job; upkeep never admits vault work.
pub struct ConvergenceInputs {
    observe: Box<dyn FnMut(RefreshObservation) -> Option<RepositoryRequest> + Send>,
    refresh: Box<dyn FnMut() -> RefreshObservation + Send>,
    maintenance: Option<Box<dyn FnMut() -> Option<Instant> + Send>>,
    next_look: Box<dyn FnMut() -> Option<Instant> + Send>,
    identity: Box<dyn FnMut() -> IdentityDebt + Send>,
}

impl ConvergenceInputs {
    /// Readiness observation grants no authority to the converge job it requests.
    pub fn new(
        observe: Box<dyn FnMut(RefreshObservation) -> Option<RepositoryRequest> + Send>,
    ) -> Self {
        Self {
            observe,
            refresh: Box::new(RefreshObservation::default),
            maintenance: None,
            next_look: Box::new(|| None),
            identity: Box::new(|| IdentityDebt::None),
        }
    }

    /// Read the account's shared converge receipt once per observation. These facts pace
    /// requests; the runner independently admits its source and every effect.
    pub fn with_refresh(mut self, refresh: Box<dyn FnMut() -> RefreshObservation + Send>) -> Self {
        self.refresh = refresh;
        self
    }

    /// Add the canonical identity subject without borrowing repository holds or receipts.
    pub fn with_identity(mut self, identity: Box<dyn FnMut() -> IdentityDebt + Send>) -> Self {
        self.identity = identity;
        self
    }

    /// An existing canonical-file reader heard an input event. Consumers re-read their own
    /// semantic source; this signal carries no generated name or publication authority.
    pub fn identity_changed() {
        let mut signal = IDENTITY_CHANGED
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        signal.generation = signal.generation.wrapping_add(1);
        if let Some(wake) = signal.wake.take() {
            wake.set();
        }
    }

    pub fn snapshot(&mut self) -> ConvergenceSnapshot {
        let refresh = (self.refresh)();
        ConvergenceSnapshot {
            refresh,
            repositories: (self.observe)(refresh),
            identity: (self.identity)(),
        }
    }

    /// Add nonblocking local upkeep to the same host lifetime. It runs while
    /// sharing is off too, independently of vault/network work, and returns when it is next
    /// owed with nothing rung (`None`: only a ring owes it).
    pub fn with_maintenance(
        mut self,
        maintenance: Box<dyn FnMut() -> Option<Instant> + Send>,
    ) -> Self {
        self.maintenance = Some(maintenance);
        self
    }

    /// When these inputs owe a look though nothing rang their doorbell: an input no watch
    /// covers, read at its own pace. Without one an observer owes a look only at a ring, so an
    /// observer that reads an input must either ring for it or name this.
    pub fn with_next_look(mut self, next_look: Box<dyn FnMut() -> Option<Instant> + Send>) -> Self {
        self.next_look = next_look;
        self
    }

    /// When these inputs next owe a look with nothing rung ([`Self::with_next_look`]).
    pub(super) fn next_look(&mut self) -> Option<Instant> {
        (self.next_look)()
    }
}

/// Makes a runtime's inputs, handing them the doorbell its lane hears (`None`: a lane that looks
/// at its own pace), which they ring when an input they read changes.
type MakeInputs = fn(&super::Paths, ConvergenceRole, Option<Arc<Doorbell>>) -> ConvergenceInputs;
static INPUTS: std::sync::OnceLock<MakeInputs> = std::sync::OnceLock::new();

/// Supply an account-scoped local-input observer without coupling the vault library to the
/// executable's package and repository policy. The first registration wins.
pub fn on_convergence_inputs(make: MakeInputs) {
    let _ = INPUTS.set(make);
}

/// Construct the registered observer for this runtime only when its automatic work is allowed,
/// ringing `rung` when an input it reads changes.
pub fn convergence_inputs(
    paths: &super::Paths,
    role: ConvergenceRole,
    rung: Option<Arc<Doorbell>>,
) -> ConvergenceInputs {
    INPUTS.get().map_or_else(
        || ConvergenceInputs::new(Box::new(|_| None)),
        |make| make(paths, role, rung),
    )
}

struct Running {
    due: Due,
    child: Option<aterm_uds::spawnfd::Child>,
    /// Reads ready once `child` has exited ([`aterm_uds::exitwatch::exit_fd`]); `None` with no
    /// child, or where none could be made.
    exit: Option<Arc<OwnedFd>>,
    sharing: crate::swarm::Switch,
    held: bool,
}

impl Running {
    /// What a worker waiting on this runner waits for: its exit's descriptor and when time
    /// alone next starts one ([`Due::next_at`]).
    fn awaited(&self) -> (Option<*const OwnedFd>, Option<u64>) {
        (self.exit.as_ref().map(Arc::as_ptr), self.due.next_at())
    }

    /// Take the runner's outcome, and its exit's descriptor with it once it has ended.
    fn last(&mut self) -> Last {
        let last = Last::of(&mut self.child);
        if self.child.is_none() {
            self.exit = None;
        }
        last
    }
}

impl Running {
    fn allowed(&mut self, log: &dyn Fn(&str)) -> bool {
        let allowed =
            super::swarm_lets(&mut self.sharing, &mut self.held, "local convergence", log);
        if !allowed {
            self.due.paused();
        }
        allowed
    }
}

pub(super) struct Projection<'a> {
    running: Mutex<Running>,
    program: crate::program_watch::ProgramWatch,
    clock: Box<dyn Fn() -> u64 + Send + Sync + 'a>,
    ports: &'a Converging<'a>,
    inputs: Mutex<ConvergenceInputs>,
    log: &'a (dyn Fn(&str) + Sync),
    /// Rung when a call moved what the readiness worker waits on ([`Running::awaited`]): a
    /// runner started or reaped, or a retry or recheck moved, by whichever thread made it.
    rung: Option<Arc<Doorbell>>,
}

impl<'a> Projection<'a> {
    #[cfg(test)]
    pub(super) fn new(paths: &super::Paths, ports: &'a Converging<'a>) -> Self {
        Self::observing(paths, ports, ConvergenceInputs::new(Box::new(|_| None)))
    }

    pub(super) fn observing(
        paths: &super::Paths,
        ports: &'a Converging<'a>,
        inputs: ConvergenceInputs,
    ) -> Self {
        let begun = Instant::now();
        Self {
            running: Mutex::new(Running {
                due: Due::default(),
                child: None,
                exit: None,
                sharing: crate::swarm::Switch::new(&paths.home),
                held: false,
            }),
            program: crate::program_watch::ProgramWatch::current(),
            clock: Box::new(move || u64::try_from(begun.elapsed().as_millis()).unwrap_or(u64::MAX)),
            ports,
            inputs: Mutex::new(inputs),
            log: &|_| {},
            rung: None,
        }
    }

    /// Ring `rung` whenever a call moves what the readiness worker waits on.
    pub(super) fn ringing(mut self, rung: Option<Arc<Doorbell>>) -> Self {
        self.rung = rung;
        self
    }

    /// Run `change` on the runner, ringing [`Self::rung`] when it moved what a worker awaits.
    fn changing<T>(&self, running: &mut Running, change: impl FnOnce(&mut Running) -> T) -> T {
        let before = running.awaited();
        let changed = change(running);
        if running.awaited() != before
            && let Some(rung) = &self.rung
        {
            rung.ring();
        }
        changed
    }

    pub(super) fn logging(mut self, log: &'a (dyn Fn(&str) + Sync)) -> Self {
        self.log = log;
        self
    }

    pub(super) fn replaced(&self) -> bool {
        self.program.replaced()
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    #[cfg(test)]
    pub(super) fn at_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'a) -> Self {
        self.clock = Box::new(clock);
        self
    }

    fn update(&self, read: impl FnOnce(&mut Due, u64, crate::converge::Routes, Last) -> bool) {
        if !self.allowed() {
            return;
        }
        let snapshot = self
            .inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot();
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if !running.allowed(self.log) {
            return;
        }
        let now = self.now();
        self.changing(&mut running, |running| {
            let last = running.last();
            running.due.observe(snapshot.refresh);
            if let Some(request) = snapshot.repositories {
                running.due.request(request);
            }
            if snapshot.identity == IdentityDebt::Changed {
                running.due.identity_changed();
            }
            let due = read(&mut running.due, now, (self.ports.routes)(), last);
            if due
                && !self.replaced()
                && let Some(exe) = self.program.path()
            {
                running.child = (self.ports.start)(exe, running.due.repository_request());
                running.exit = running
                    .child
                    .as_ref()
                    .and_then(|child| aterm_uds::exitwatch::exit_fd(child.id()))
                    .map(Arc::new);
                if running.child.is_none() {
                    running.due.reaped(Last::Failed, now);
                }
            }
        });
    }

    pub(super) fn start(&self) {
        self.update(Due::at_start);
    }

    pub(super) fn poll(&self) {
        self.poll_with(false);
    }

    /// All hold observers serialize their fresh sharing read with the retained debt.
    /// A delayed caller cannot re-arm an old Off observation after another caller resumed.
    pub(super) fn allowed(&self) -> bool {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .allowed(self.log)
    }

    /// The HTTP owner also services a hold the listener missed. Healthy rounds do not
    /// create another input observation or refresh boundary.
    pub(super) fn resume(&self) {
        let pending = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .due
            .activation_pending();
        if pending {
            self.poll();
        }
    }

    /// An interrupted observation can have consumed local changes. Request ordinary
    /// readback once after recovery; the job still admits every effect from current facts.
    pub(super) fn recovered(&self) {
        self.poll_with(true);
    }

    fn poll_with(&self, recovering: bool) {
        self.update(|due, now, routes, last| {
            if recovering {
                due.after_local_progress(RepositoryWork::Refresh, now, routes, last)
            } else {
                due.poll(now, routes, last)
            }
        });
    }

    /// The inputs' upkeep: when it is next owed with nothing rung.
    pub(super) fn maintain(&self) -> Option<Instant> {
        let mut inputs = self.inputs.lock().unwrap_or_else(PoisonError::into_inner);
        inputs.maintenance.as_mut().and_then(|upkeep| upkeep())
    }

    /// When the projection next owes a look with nothing rung: its runner's retry or recheck
    /// ([`Due::next_at`]) and an input no watch covers. A runner still held owes its look at
    /// its exit ([`Self::wakes`]), or at once where no descriptor tells of that exit; a hold
    /// owes none, its switch's change being an input's.
    pub(super) fn next_look(&self) -> Option<Instant> {
        let inputs = self
            .inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next_look();
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.held {
            return None;
        }
        let due = match (&running.child, &running.exit) {
            (Some(_), Some(_)) => None,
            (Some(_), None) => Some(Instant::now()),
            (None, _) => running
                .due
                .next_at()
                .map(|at| Instant::now() + Duration::from_millis(at.saturating_sub(self.now()))),
        };
        inputs.into_iter().chain(due).min()
    }

    /// Descriptors whose readiness owes a look: the held runner's exit.
    pub(super) fn wakes(&self) -> Vec<Arc<OwnedFd>> {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exit
            .clone()
            .into_iter()
            .collect()
    }

    pub(super) fn admitted(&self) {
        self.update(|due, now, routes, last| {
            due.after_local_progress(RepositoryWork::Refresh, now, routes, last)
        });
    }

    pub(super) fn after_pass(&self, outcome: &SyncOutcome, source: SourceSaid) {
        self.update(|due, now, routes, last| {
            converge_after_pass(due, outcome, now, routes, last, source)
        });
    }

    pub(super) fn failed(&self) {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .due
            .failed();
    }

    pub(super) fn reap(&self) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.now();
        self.changing(&mut running, |running| {
            let last = running.last();
            running.due.reaped(last, now);
        });
    }
}
