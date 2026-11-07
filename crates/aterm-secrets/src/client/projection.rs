// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One subscriber's ordinary converge job, shared by HTTP rounds and local member progress.

use super::{Converging, SourceSaid, SyncOutcome, converge_after_pass};
use crate::converge::{Due, Last, RefreshObservation, RepositoryRequest, RepositoryWork};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// The automatic runtime that owns a local readiness observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvergenceRole {
    /// The vault subscriber also owns executable-local worker upkeep.
    Subscriber,
    /// The node service already owns its worker upkeep for its whole lifetime.
    Node,
}

/// An executable's readiness observer and optional local worker upkeep, on an
/// automatic runtime's existing lane. Readiness starts no helper or network request and
/// grants no authority to the resulting job; upkeep never admits vault work.
pub struct ConvergenceInputs {
    observe: Box<dyn FnMut(RefreshObservation) -> Option<RepositoryRequest> + Send>,
    refresh: Box<dyn FnMut() -> RefreshObservation + Send>,
    maintenance: Option<Box<dyn FnMut() + Send>>,
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
        }
    }

    /// Read the account's shared converge receipt once per observation. These facts pace
    /// requests; the runner independently admits its source and every effect.
    pub fn with_refresh(mut self, refresh: Box<dyn FnMut() -> RefreshObservation + Send>) -> Self {
        self.refresh = refresh;
        self
    }

    pub(super) fn snapshot(&mut self) -> (RefreshObservation, Option<RepositoryRequest>) {
        let refresh = (self.refresh)();
        (refresh, (self.observe)(refresh))
    }

    /// Add nonblocking local upkeep to the same host lifetime. It runs while
    /// sharing is off too, independently of vault/network work.
    pub fn with_maintenance(mut self, maintenance: Box<dyn FnMut() + Send>) -> Self {
        self.maintenance = Some(maintenance);
        self
    }
}

type MakeInputs = fn(&super::Paths, ConvergenceRole) -> ConvergenceInputs;
static INPUTS: std::sync::OnceLock<MakeInputs> = std::sync::OnceLock::new();

/// Supply an account-scoped local-input observer without coupling the vault library to the
/// executable's package and repository policy. The first registration wins.
pub fn on_convergence_inputs(make: MakeInputs) {
    let _ = INPUTS.set(make);
}

/// Construct the registered observer for this runtime only when its automatic work is allowed.
pub fn convergence_inputs(paths: &super::Paths, role: ConvergenceRole) -> ConvergenceInputs {
    INPUTS.get().map_or_else(
        || ConvergenceInputs::new(Box::new(|_| None)),
        |make| make(paths, role),
    )
}

struct Running {
    due: Due,
    child: Option<aterm_uds::spawnfd::Child>,
}

pub(super) struct Projection<'a> {
    running: Mutex<Running>,
    program: crate::program_watch::ProgramWatch,
    clock: Box<dyn Fn() -> u64 + Send + Sync + 'a>,
    ports: &'a Converging<'a>,
    inputs: Mutex<ConvergenceInputs>,
}

impl<'a> Projection<'a> {
    #[cfg(test)]
    pub(super) fn new(ports: &'a Converging<'a>) -> Self {
        Self::observing(ports, ConvergenceInputs::new(Box::new(|_| None)))
    }

    pub(super) fn observing(ports: &'a Converging<'a>, inputs: ConvergenceInputs) -> Self {
        let begun = Instant::now();
        Self {
            running: Mutex::new(Running {
                due: Due::default(),
                child: None,
            }),
            program: crate::program_watch::ProgramWatch::current(),
            clock: Box::new(move || u64::try_from(begun.elapsed().as_millis()).unwrap_or(u64::MAX)),
            ports,
            inputs: Mutex::new(inputs),
        }
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
        let (refresh, request) = self
            .inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot();
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.now();
        let last = Last::of(&mut running.child);
        running.due.observe(refresh);
        if let Some(request) = request {
            running.due.request(request);
        }
        let due = read(&mut running.due, now, (self.ports.routes)(), last);
        if due
            && !self.replaced()
            && let Some(exe) = self.program.path()
        {
            running.child = (self.ports.start)(exe, running.due.repository_request());
            if running.child.is_none() {
                running.due.reaped(Last::Failed, now);
            }
        }
    }

    pub(super) fn start(&self) {
        self.update(Due::at_start);
    }

    pub(super) fn poll(&self) {
        self.poll_with(false);
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

    pub(super) fn maintain(&self) {
        let mut inputs = self.inputs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(maintain) = &mut inputs.maintenance {
            maintain();
        }
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
        let last = Last::of(&mut running.child);
        running.due.reaped(last, self.now());
    }
}
