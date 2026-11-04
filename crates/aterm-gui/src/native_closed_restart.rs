// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained closed-attempt evidence. This type grants no mutation: a later
//! sealed service must separately admit a new operation against current policy.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub(super) struct ClosedRestart {
    pub(super) predecessor: Prepare,
    source: Arc<SharedDeath>,
    children: Vec<(Process, Arc<SharedDeath>)>,
    drain: Option<NativeDrain>,
    service_owned: bool,
    retired: AtomicBool,
    successor: std::sync::OnceLock<Prepare>,
}

impl ClosedRestart {
    pub(super) fn capture(op: &Operation, reason: Retirement, kernel: &dyn Kernel) -> Option<Self> {
        let NativePlan::Restart { process, .. } = &op.request.plan else {
            return None;
        };
        let source = op.source_death.as_ref()?;
        if !source.exited()
            || kernel.group_gone(process.group) != Some(true)
            || op
                .resume_attempt
                .as_ref()
                .is_some_and(|attempt| !attempt.settled())
        {
            return None;
        }
        let mut children = Vec::new();
        for child in op
            .children
            .iter()
            .flatten()
            .filter(|child| child.execute_sent)
        {
            if !child.death.exited() || kernel.group_gone(child.process.group) != Some(true) {
                return None;
            }
            children.push((child.process.clone(), Arc::clone(&child.death)));
        }
        if (reason == Retirement::NotGranted) != children.is_empty() {
            return None;
        }
        Some(Self {
            predecessor: op.request.clone(),
            source: Arc::clone(source),
            children,
            drain: op
                .custody
                .as_ref()
                .map(|custody| custody.drain())
                .or_else(|| op.completion_drain.clone()),
            service_owned: op
                .authority
                .renewal
                .as_ref()
                .is_some_and(|seal| seal.intent_active()),
            retired: AtomicBool::new(false),
            successor: std::sync::OnceLock::new(),
        })
    }

    pub(super) fn pinned(&self) -> bool {
        self.service_owned
            && self.successor.get().is_none()
            && !self.retired.load(Ordering::Acquire)
    }

    pub(super) fn source_watch(&self) -> Arc<SharedDeath> {
        Arc::clone(&self.source)
    }

    pub(super) fn available(&self) -> bool {
        !self.retired.load(Ordering::Acquire) && self.successor.get().is_none()
    }

    /// Called only at the matched terminal operation's admission boundary.
    /// Exact replays use the existing child operation; absence never remints it.
    pub(super) fn claim_successor(&self, request: &Prepare) -> bool {
        self.available() && self.successor.set(request.clone()).is_ok()
    }

    pub(super) fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    pub(super) fn cancelled_without_successor(
        &self,
        prepare: &Prepare,
        kernel: &dyn Kernel,
    ) -> bool {
        self.predecessor == *prepare
            && self.retired.load(Ordering::Acquire)
            && self.successor.get().is_none()
            && self.children.is_empty()
            && self.quiescent(kernel)
    }

    /// Recheck retained kernel objects and the exact drained input boundary.
    /// Source/child absence without those objects never passes this predicate.
    pub(super) fn quiescent(&self, kernel: &dyn Kernel) -> bool {
        let NativePlan::Restart { process, .. } = &self.predecessor.plan else {
            return false;
        };
        self.source.exited()
            && kernel.group_gone(process.group) == Some(true)
            && self.drain.as_ref().is_none_or(NativeDrain::is_drained)
            && self.children.iter().all(|(process, watch)| {
                watch.exited() && kernel.group_gone(process.group) == Some(true)
            })
    }
}
