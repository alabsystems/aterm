// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! In-place account handoff progression. Every method is used only after the
//! native host rechecks policy, claim, process image and the shared pool receipt.
//! A generation is the kernel's exec generation, not a caller's sequence number.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Armed,
    Reserved,
    Executing,
    Loaded(i32),
    Consumed(i32),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Transaction {
    source_generation: i32,
    stage: Stage,
    execution_granted: bool,
}

impl Transaction {
    #[must_use]
    pub fn new(source_generation: i32) -> Self {
        Self {
            source_generation,
            stage: Stage::Armed,
            execution_granted: false,
        }
    }

    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Reserve the shared account while the original runtime can still wait.
    pub fn reserve(&mut self, generation: i32) -> bool {
        if generation != self.source_generation
            || !matches!(self.stage, Stage::Armed | Stage::Reserved)
        {
            return false;
        }
        self.stage = Stage::Reserved;
        true
    }

    /// A lost execute reply can reconcile only in the original process image.
    pub fn execute(&mut self, generation: i32) -> bool {
        if generation != self.source_generation
            || !matches!(self.stage, Stage::Reserved | Stage::Executing)
        {
            return false;
        }
        self.stage = Stage::Executing;
        self.execution_granted = true;
        true
    }

    /// Only the first successor image may bind this operation. Another re-exec
    /// cannot inherit an earlier image's permission or successful consume.
    pub fn load(&mut self, generation: i32) -> bool {
        if generation == self.source_generation {
            return false;
        }
        match self.stage {
            Stage::Executing => {
                self.stage = Stage::Loaded(generation);
                true
            }
            Stage::Loaded(bound) | Stage::Consumed(bound) => bound == generation,
            Stage::Armed | Stage::Reserved | Stage::Cancelled => false,
        }
    }

    pub fn consume(&mut self, generation: i32) -> bool {
        match self.stage {
            Stage::Loaded(bound) | Stage::Consumed(bound) if bound == generation => {
                self.stage = Stage::Consumed(bound);
                true
            }
            _ => false,
        }
    }

    /// Completed admission belongs to the running turn's normal cancellation.
    pub fn cancel(&mut self) -> bool {
        if matches!(self.stage, Stage::Consumed(_)) {
            return false;
        }
        self.stage = Stage::Cancelled;
        true
    }

    /// Cancel new admission and report whether Go may already have been sent.
    /// A receipt's prospective write marker is not this live grant history.
    /// The history survives cancellation and a lost reply; this result alone
    /// proves neither process termination nor reclaimable account rights.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ExecRevocation",
            action = "NextCase",
            reason = "The conformance fixture chooses reachable transaction and prospective receipt states."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ExecRevocation",
            action = "Revoke",
            project = "aterm_agent::harness::codex_exec_handoff::transaction::revocation_tests::revocation_transition"
        )
    )]
    pub fn revoke(&mut self) -> bool {
        self.cancel() && self.execution_granted
    }
}

#[cfg(test)]
#[path = "codex_exec_revocation_tests.rs"]
mod revocation_tests;

#[cfg(test)]
#[path = "codex_exec_transaction_tests.rs"]
mod tests;
