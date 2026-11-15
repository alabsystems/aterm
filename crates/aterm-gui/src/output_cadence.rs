// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The UI-owned ordered frontier of output holds. Enrollment is part of the
//! latch store, and each session owns at most one entry in each index.

use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::SessionPool;
use crate::spawn::{HoldRelease, WAKE_LATCH_HELD, WAKE_LATCH_STAMP};

#[derive(Default)]
pub(crate) struct Frontier {
    ordered: BTreeMap<(u64, u64), Arc<AtomicU64>>,
    deadlines: HashMap<u64, u64>,
    #[cfg(test)]
    visits: usize,
    #[cfg(test)]
    passes: usize,
}

pub(crate) enum Work {
    Due(u64),
    Pending(u64),
    Idle,
}

impl Frontier {
    pub(crate) fn forget(&mut self, session: u64) {
        if let Some(deadline) = self.deadlines.remove(&session) {
            self.ordered.remove(&(deadline, session));
        }
    }

    fn remember(&mut self, session: u64, latch: &Arc<AtomicU64>, word: u64) {
        self.forget(session);
        if word & WAKE_LATCH_HELD != 0 {
            let deadline = word & WAKE_LATCH_STAMP;
            self.deadlines.insert(session, deadline);
            self.ordered.insert((deadline, session), Arc::clone(latch));
        }
    }

    /// The only production UI hold-creation/rearm store. Reader edges may dirty
    /// or clear this word; they cannot create an unindexed cadence hold.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OutputCadenceFrontier",
            action = "Hold",
            project = "aterm_gui::output_cadence::tests::project"
        )
    )]
    fn after_pass(&mut self, session: u64, latch: &Arc<AtomicU64>, now_ns: u64) {
        #[cfg(test)]
        {
            self.passes += 1;
        }
        let word = crate::spawn::output_latch_after_pass(latch.load(Ordering::Relaxed), now_ns);
        latch.store(word, Ordering::Relaxed);
        self.remember(session, latch, word);
    }

    #[cfg(test)]
    fn next(&self) -> Option<u64> {
        self.ordered
            .first_key_value()
            .map(|((deadline, _), _)| *deadline)
    }
}

impl SessionPool {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "OutputCadenceFrontier",
            action = "Dirty",
            reason = "Environment: the existing PTY reader atomic protocol dirties a held latch; production conformance feeds it through gated_output_wake. This finite episode does not prove reader interleavings."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "OutputCadenceFrontier",
            action = "Clear",
            reason = "Environment: reader foreground transitions may concurrently clear a held word. Due service validates the current atomic state; tests retain the real foreground_edge path."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "OutputCadenceFrontier",
            action = "Retire",
            reason = "Scope: session-pool last detach and identity replacement own lifetime retirement outside this held episode; conformance uses those production transitions. This is not a model proof of pool lifetime management."
        )
    )]
    pub(crate) fn output_pass(&mut self, session: u64, now_ns: u64) {
        if let Some(pooled) = self.sessions.get(&session) {
            self.output_holds
                .after_pass(session, &pooled.session.output_wake_pending, now_ns);
        } else {
            self.output_holds.forget(session);
        }
    }

    /// Validate the relevant frontier head before either dispatching output or
    /// returning its actual pending deadline. Reader clears before that deadline
    /// retire the candidate now, rather than arming an obsolete timer.
    pub(crate) fn output_hold_work(&mut self, now_ns: u64) -> Work {
        loop {
            let Some((&(deadline, session), latch)) = self.output_holds.ordered.first_key_value()
            else {
                return Work::Idle;
            };
            let latch = Arc::clone(latch);
            let Some(live) = self.sessions.get(&session) else {
                self.output_holds.forget(session);
                continue;
            };
            if !Arc::ptr_eq(&live.session.output_wake_pending, &latch) {
                self.output_holds.forget(session);
                continue;
            }
            #[cfg(test)]
            {
                self.output_holds.visits += 1;
            }
            match crate::spawn::release_output_hold(&latch, now_ns) {
                HoldRelease::Due => {
                    self.output_holds.forget(session);
                    return Work::Due(session);
                }
                HoldRelease::Pending { due_ns } if due_ns == deadline => {
                    return Work::Pending(due_ns);
                }
                HoldRelease::Pending { due_ns } => {
                    self.output_holds
                        .remember(session, &latch, WAKE_LATCH_HELD | due_ns);
                    if self
                        .output_holds
                        .ordered
                        .first_key_value()
                        .is_some_and(|((_, first), _)| *first == session)
                    {
                        return Work::Pending(due_ns);
                    }
                }
                HoldRelease::NotHeld | HoldRelease::Released => self.output_holds.forget(session),
            }
        }
    }
}

#[cfg(test)]
#[path = "output_cadence_tests.rs"]
mod tests;
