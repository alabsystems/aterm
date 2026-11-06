// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One subscriber's ordinary converge job, shared by HTTP rounds and local member progress.

use super::{Converging, SourceSaid, SyncOutcome, converge_after_pass};
use crate::converge::{Due, Last};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// An executable's local readiness observer. It runs on the subscriber's existing polling
/// lane, starts no helper or network request, and grants no authority to the resulting job.
pub type SubscriberInputs = Box<dyn FnMut() -> bool + Send>;

type MakeInputs = fn(&super::Paths) -> SubscriberInputs;
static INPUTS: std::sync::OnceLock<MakeInputs> = std::sync::OnceLock::new();

/// Supply an account-scoped local-input observer without coupling the vault library to the
/// executable's package and repository policy. The first registration wins.
pub fn on_subscriber_inputs(make: MakeInputs) {
    let _ = INPUTS.set(make);
}

pub(super) fn inputs(paths: &super::Paths) -> SubscriberInputs {
    INPUTS.get().map_or_else(
        || Box::new(|| false) as SubscriberInputs,
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
        Self::observing(ports, Box::new(|| false))
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
            running.child = (self.ports.start)(exe);
            if running.child.is_none() {
                running.due.reaped(Last::Failed, now);
            }
        }
    }

    pub(super) fn start(&self) {
        self.update(Due::at_start);
    }

    pub(super) fn poll(&self) {
        let changed = {
            let mut inputs = self.inputs.lock().unwrap_or_else(PoisonError::into_inner);
            inputs()
        };
        self.update(if changed {
            Due::after_local_progress
        } else {
            Due::poll
        });
    }

    pub(super) fn admitted(&self) {
        self.update(Due::after_local_progress);
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
