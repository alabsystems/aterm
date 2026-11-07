// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One subscriber's ordinary converge job, shared by HTTP rounds and local member progress.

use super::{Converging, SourceSaid, SyncOutcome, converge_after_pass};
use crate::converge::{Due, Last, RepositoryWork};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// An executable's readiness observer and optional local worker upkeep, on the
/// subscriber's existing lane. Readiness starts no helper or network request and
/// grants no authority to the resulting job; upkeep never admits vault work.
pub struct SubscriberInputs {
    observe: Box<dyn FnMut() -> Option<RepositoryWork> + Send>,
    maintenance: Option<Box<dyn FnMut() + Send>>,
}

impl SubscriberInputs {
    /// Readiness observation grants no authority to the converge job it requests.
    pub fn new(observe: Box<dyn FnMut() -> Option<RepositoryWork> + Send>) -> Self {
        Self {
            observe,
            maintenance: None,
        }
    }

    /// Add nonblocking local upkeep to the same host lifetime. It runs while
    /// sharing is off too, independently of vault/network work.
    pub fn with_maintenance(mut self, maintenance: Box<dyn FnMut() + Send>) -> Self {
        self.maintenance = Some(maintenance);
        self
    }
}

type MakeInputs = fn(&super::Paths) -> SubscriberInputs;
static INPUTS: std::sync::OnceLock<MakeInputs> = std::sync::OnceLock::new();

/// Supply an account-scoped local-input observer without coupling the vault library to the
/// executable's package and repository policy. The first registration wins.
pub fn on_subscriber_inputs(make: MakeInputs) {
    let _ = INPUTS.set(make);
}

pub(super) fn inputs(paths: &super::Paths) -> SubscriberInputs {
    INPUTS.get().map_or_else(
        || SubscriberInputs::new(Box::new(|| None)),
        |make| make(paths),
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
    inputs: Mutex<SubscriberInputs>,
}

impl<'a> Projection<'a> {
    #[cfg(test)]
    pub(super) fn new(ports: &'a Converging<'a>) -> Self {
        Self::observing(ports, SubscriberInputs::new(Box::new(|| None)))
    }

    pub(super) fn observing(ports: &'a Converging<'a>, inputs: SubscriberInputs) -> Self {
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
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.now();
        let last = Last::of(&mut running.child);
        let due = read(&mut running.due, now, (self.ports.routes)(), last);
        if due
            && !self.replaced()
            && let Some(exe) = self.program.path()
        {
            running.child = (self.ports.start)(exe, running.due.repository_work());
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
        let work = {
            let mut inputs = self.inputs.lock().unwrap_or_else(PoisonError::into_inner);
            (inputs.observe)()
        };
        self.update(|due, now, routes, last| {
            match work.max(recovering.then_some(RepositoryWork::Refresh)) {
                Some(work) => due.after_local_progress(work, now, routes, last),
                None => due.poll(now, routes, last),
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
