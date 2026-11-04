// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One retained CPU unit bound to original private cohort custody.
//!
//! This owns the claim and its one-use launch state, rather than interpreting a
//! root outcome or saved closure reference as permission to settle CPU backing.
//! It does not constrain arbitrary threads or descendants that escape the native
//! cohort's group. A reusable GNU credit must not be published against this unit:
//! its transport epoch and outstanding borrows require separate lifetime custody.

use crate::Claim;
use crate::build_cpu::RetainedLease;
use crate::cohort::Cohort;
use crate::cohort::launch::{CohortChild, LaunchError};
use crate::leadership::{Binding, Fence};
use aterm_uds::spawnfd::{Child, Command, NotSpawned, Spawned};
use std::{fmt, io};

fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// A rejected binding that preserves both original inputs for its controller.
/// Dropping it leaves retained CPU debt; it never settles the supplied unit.
#[derive(Debug)]
pub struct BindError {
    error: io::Error,
    inputs: Box<(Claim, RetainedLease)>,
}

impl BindError {
    /// Why the actual leadership or claim could not be bound.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Recover the exact supplied inputs, without minting release authority.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, Claim, RetainedLease) {
        let (claim, cpu) = *self.inputs;
        (self.error, claim, cpu)
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "retained cohort binding refused: {}", self.error)
    }
}

impl std::error::Error for BindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// A no-child cancellation refusal that preserves the receipt's actual custody.
#[derive(Debug)]
pub enum CancelError {
    /// The receipt was not passed to the claim and remains available to retry.
    Refused {
        /// Why leadership, wrapper state or the original attempt refused.
        error: io::Error,
        /// The actual supplied receipt, not one decoded or recreated from a key.
        proof: Box<NotSpawned>,
    },
    /// The original claim retains cancellation custody. Its exact receipt or
    /// pending durable cancellation is retried by `try_finish`; CPU debt remains.
    Retained(io::Error),
}

impl CancelError {
    /// Why cancellation could not complete.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        match self {
            Self::Refused { error, .. } | Self::Retained(error) => error,
        }
    }

    /// Recover the receipt when preflight refused. `None` means its cancellation
    /// custody was transferred to the privately retained original claim.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, Option<NotSpawned>) {
        match self {
            Self::Refused { error, proof } => (error, Some(*proof)),
            Self::Retained(error) => (error, None),
        }
    }
}

impl fmt::Display for CancelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "retained cohort cancellation refused: {}", self.error())
    }
}

impl std::error::Error for CancelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ready,
    Bound,
    Unbound,
    NoChild,
    Cancelling,
    Unknown,
    Settled,
    SettlementUncertain,
}

/// Original cohort custody and the sole API capability for one retained CPU unit.
///
/// Binding does not construct a pool or select a machine budget. The caller must
/// supply an already durable retained capability for this one controlled unit.
/// The claim and CPU descriptor remain private, so this wrapper cannot relaunch
/// a finished claim or release backing on the root's exit alone. Drop retains
/// CPU debt even when dropping an unused claim retires its ledger description.
#[derive(Debug)]
pub struct RetainedCohort {
    binding: Binding,
    claim: Claim,
    cpu: Option<RetainedLease>,
    phase: Phase,
}

impl RetainedCohort {
    /// Bind an original unused admitted description to actual current leadership
    /// and an already retained CPU unit. No child or reusable credit may already
    /// be using this unit outside the new wrapper's custody.
    ///
    /// # Errors
    /// Leadership is stale or foreign, the claim is adopted, legacy, unadmitted,
    /// previously bound, armed or uncertain, or its physical custody refuses.
    /// The error preserves the actual claim and CPU capability for recovery.
    pub fn new(fence: &Fence, claim: Claim, cpu: RetainedLease) -> Result<Self, BindError> {
        let checked = (|| {
            let binding = fence.capture_binding(&claim)?;
            claim.check_retained_description()?;
            Ok(binding)
        })();
        match checked {
            Ok(binding) => Ok(Self {
                binding,
                claim,
                cpu: Some(cpu),
                phase: Phase::Ready,
            }),
            Err(error) => Err(BindError {
                error,
                inputs: Box::new((claim, cpu)),
            }),
        }
    }

    /// Launch this wrapper's single actual child through durable ARM and BIND.
    /// A failed launch is still an attempt; this wrapper cannot launch again.
    ///
    /// # Errors
    /// The actual leadership differs or is stale, this unit was already used,
    /// or the controlled launch refuses. Unbound errors preserve the actual
    /// child; no-child errors preserve the exact cancellation receipt.
    pub fn launch(
        &mut self,
        fence: &Fence,
        command: &mut Command,
    ) -> Result<CohortChild, LaunchError> {
        self.launch_via(fence, command, Claim::launch_cohort)
    }

    fn launch_via(
        &mut self,
        fence: &Fence,
        command: &mut Command,
        launch: impl FnOnce(&mut Claim, &mut Command) -> Result<CohortChild, LaunchError>,
    ) -> Result<CohortChild, LaunchError> {
        self.binding.check(fence).map_err(LaunchError::Refused)?;
        if self.phase != Phase::Ready {
            return Err(LaunchError::Refused(refused(
                "retained cohort launch is one-use",
            )));
        }
        self.phase = Phase::Unknown;
        let result = launch(&mut self.claim, command);
        self.phase = match &result {
            Ok(_) => Phase::Bound,
            Err(LaunchError::Unbound { .. }) => Phase::Unbound,
            Err(LaunchError::NotSpawned(_)) => Phase::NoChild,
            Err(LaunchError::Refused(_)) => Phase::Unknown,
        };
        result
    }

    /// Start another owned member while this unit's original leader is still
    /// live. All members share the original group reservation until drain.
    /// This does not grant an additional CPU unit or arbitrary thread confinement.
    ///
    /// # Errors
    /// Leadership, actual leader or durable binding is stale, missing or foreign,
    /// the one-use unit is not bound, or the controlled member spawn refuses.
    pub fn launch_member(
        &self,
        fence: &Fence,
        leader: &mut CohortChild,
        command: &mut Command,
    ) -> io::Result<Child> {
        self.binding.check(fence)?;
        if self.phase != Phase::Bound {
            return Err(refused(
                "retained cohort has no bound member-launch authority",
            ));
        }
        self.claim.launch_cohort_member(leader, command)
    }

    /// Retry binding the actual child returned by this launch's Unbound error.
    /// Matching uses its original one-use birth, never a caller-selected PID.
    ///
    /// # Errors
    /// Leadership differs or is stale, this wrapper has no unbound attempt, the
    /// actual receipt is foreign, or durable binding still cannot complete.
    pub fn retry_bind(&mut self, fence: &Fence, spawned: &mut Spawned) -> io::Result<Cohort> {
        self.binding.check(fence)?;
        if self.phase != Phase::Unbound {
            return Err(refused("retained cohort has no unbound launch to retry"));
        }
        let cohort = self.claim.bind_cohort(spawned)?;
        self.phase = Phase::Bound;
        Ok(cohort)
    }

    /// Cancel only this launch's genuine controlled no-child receipt. CPU debt
    /// remains held until `try_finish` confirms its durable cancellation.
    ///
    /// # Errors
    /// Leadership differs or is stale, this was not a no-child attempt, the
    /// actual receipt does not match, or cancellation persistence fails. Preflight
    /// errors return the actual receipt. Once accepted, the original claim keeps
    /// its cancellation custody across I/O failure for `try_finish` to retry.
    pub fn cancel(&mut self, fence: &Fence, proof: NotSpawned) -> Result<(), CancelError> {
        let preflight = (|| {
            self.binding.check(fence)?;
            if !matches!(self.phase, Phase::NoChild | Phase::Cancelling) {
                return Err(refused(
                    "retained cohort requires its exact no-child cancellation",
                ));
            }
            self.claim.check_cohort_cancel(&proof)
        })();
        if let Err(error) = preflight {
            return Err(CancelError::Refused {
                error,
                proof: Box::new(proof),
            });
        }
        self.phase = Phase::Cancelling;
        self.claim
            .cancel_cohort(proof)
            .map_err(CancelError::Retained)
    }

    /// Finish once the original group conclusively drains, or cancel this
    /// never-launched unit. Root exit alone supplies no completion authority.
    /// This preserves the original claim's pending-event retry path.
    ///
    /// # Errors
    /// Actual leadership differs or is stale, a no-child receipt has not been
    /// cancelled, launch custody is unknown, group drain is not conclusive, or
    /// journal/CPU settlement fails. Journal errors retain the original CPU
    /// capability. Once the CPU Free write starts an I/O error has an uncertain
    /// outcome; its consumed capability cannot be retried or reported settled.
    pub fn try_finish(&mut self, fence: &Fence) -> io::Result<()> {
        self.binding.check(fence)?;
        match self.phase {
            Phase::Settled => return Ok(()),
            Phase::NoChild | Phase::Unknown => {
                return Err(refused(
                    "retained cohort still has uncertain launch custody",
                ));
            }
            Phase::SettlementUncertain => {
                return Err(refused("retained CPU settlement outcome remains uncertain"));
            }
            Phase::Ready | Phase::Bound | Phase::Unbound | Phase::Cancelling => {}
        }
        // check_claim/cohort_bound would reject a legitimate locally pending
        // finish. This privately owned original claim performs retry_event and
        // exact durable closure validation itself before CPU settlement begins.
        self.claim.finish_cohort()?;
        let cpu = self
            .cpu
            .take()
            .ok_or_else(|| refused("retained CPU settlement capability is missing"))?;
        self.phase = Phase::SettlementUncertain;
        cpu.settle()?;
        self.phase = Phase::Settled;
        Ok(())
    }

    /// Whether this controller confirmed durable CPU settlement successfully.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.phase == Phase::Settled
    }

    #[cfg(test)]
    pub(crate) fn launch_with(
        &mut self,
        fence: &Fence,
        command: &mut Command,
        bind: impl FnOnce(&mut Claim, &mut Spawned) -> io::Result<Cohort>,
    ) -> Result<CohortChild, LaunchError> {
        self.launch_via(fence, command, |claim, command| {
            crate::cohort::launch::launch_with(claim, command, bind)
        })
    }
}
