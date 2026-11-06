// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained CPU units bound to original private cohort custody.
//!
//! This owns the claim and its one-use launch state, rather than interpreting a
//! root outcome or saved closure reference as permission to settle CPU backing.
//! It does not constrain arbitrary threads or descendants that escape the native
//! cohort's group. Reusable GNU credits must not be published against these units:
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

/// A refused bundle binding, retaining every actual settlement capability.
#[derive(Debug)]
pub struct BundleBindError {
    error: io::Error,
    inputs: Box<(Claim, Vec<RetainedLease>)>,
}

impl BundleBindError {
    /// Why the bundle could not be bound to the original claim.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Recover the original claim and all original CPU capabilities.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, Claim, Vec<RetainedLease>) {
        let (claim, cpu) = *self.inputs;
        (self.error, claim, cpu)
    }
}

impl fmt::Display for BundleBindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "retained cohort bundle binding refused: {}", self.error)
    }
}

impl std::error::Error for BundleBindError {
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

/// Read-only identity of a bound reservation. This supplies no child-control,
/// CPU acquisition, transfer or settlement authority.
#[derive(Clone, Debug)]
pub struct Observation {
    /// Actual retained memory declaration, in bytes.
    pub memory_bytes: u64,
    /// Original durable memory claim epoch and guard.
    pub memory: crate::CohortClosureRef,
    /// Validated original CPU debt generations; no ownership is transferred.
    pub cpu: Vec<crate::build_cpu::RetainedLeaseIdentity>,
}

/// Original cohort custody and the sole API capabilities for its retained CPU units.
///
/// Binding does not construct a pool or select a machine budget. The caller must
/// supply already durable retained capabilities for this controlled workload.
/// The claim and CPU descriptor remain private, so this wrapper cannot relaunch
/// a finished claim or release backing on the root's exit alone. Drop retains
/// CPU debt even when dropping an unused claim retires its ledger description.
#[derive(Debug)]
pub struct RetainedCohort {
    binding: Binding,
    claim: Claim,
    cpu: Vec<RetainedLease>,
    cpu_units: usize,
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
                cpu: vec![cpu],
                cpu_units: 1,
                phase: Phase::Ready,
            }),
            Err(error) => Err(BindError {
                error,
                inputs: Box::new((claim, cpu)),
            }),
        }
    }

    /// Bind a nonempty, already-admitted CPU bundle to one original cohort.
    ///
    /// This does not acquire CPU units or select their pool. An external-debt
    /// allocator may supply its actual retained capabilities here; their
    /// declared width remains unavailable until this cohort conclusively drains.
    /// Every failure returns all supplied capabilities without settling any.
    ///
    /// # Errors
    /// Empty or oversized bundle, stale/foreign leadership, or a claim whose
    /// unused original launch custody cannot be established.
    pub fn new_bundle(
        fence: &Fence,
        claim: Claim,
        cpu: Vec<RetainedLease>,
    ) -> Result<Self, BundleBindError> {
        let checked = (|| {
            if cpu.is_empty() || cpu.len() > crate::build_cpu::MAX_SLOTS as usize {
                return Err(refused(
                    "retained cohort needs a bounded nonempty CPU bundle",
                ));
            }
            let binding = fence.capture_binding(&claim)?;
            claim.check_retained_description()?;
            Ok(binding)
        })();
        match checked {
            Ok(binding) => Ok(Self {
                binding,
                claim,
                cpu_units: cpu.len(),
                cpu,
                phase: Phase::Ready,
            }),
            Err(error) => Err(BundleBindError {
                error,
                inputs: Box::new((claim, cpu)),
            }),
        }
    }

    /// Inspect the actual original bound custody under current leadership.
    /// Saved observations, unbound launches and partially settled bundles do
    /// not qualify. The caller separately authenticates the requesting process
    /// against its original actual child; this method accepts no numeric PID.
    /// # Errors
    /// Leadership, physical memory origin, CPU generation or bound phase fails.
    pub fn observation(&self, fence: &Fence) -> io::Result<Observation> {
        self.binding.check(fence)?;
        fence.check_claim(&self.claim)?;
        if self.phase != Phase::Bound || self.cpu.len() != self.cpu_units {
            return Err(refused(
                "inspection requires the complete original bound reservation",
            ));
        }
        let memory = self.claim.cohort_closure_ref()?;
        let cpu = self
            .cpu
            .iter()
            .map(RetainedLease::identity)
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Observation {
            memory_bytes: self.claim.record().need.mem,
            memory,
            cpu,
        })
    }

    /// Original bundle width, not a count of free or reusable CPU credits.
    #[must_use]
    pub const fn cpu_units(&self) -> usize {
        self.cpu_units
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

    /// Durably register and launch another dedicated group under this unit's
    /// original root. The existing CPU bundle and memory claim cover the whole
    /// forest; this grants no additional CPU credit or member entitlement.
    ///
    /// # Errors
    /// Leadership or the bound original root is stale, subgroup admission or
    /// persistence refuses, or native spawn fails. Unbound errors retain the
    /// actual suspended child; no-child errors retain the exact cancellation.
    pub fn launch_subgroup(
        &mut self,
        fence: &Fence,
        root: &mut CohortChild,
        command: &mut Command,
    ) -> Result<CohortChild, LaunchError> {
        self.check_bound(fence).map_err(LaunchError::Refused)?;
        self.claim.launch_subgroup(root, command)
    }

    /// Retry subgroup BIND without discarding its actual owned child on error.
    ///
    /// # Errors
    /// Leadership, the bound unit or durable subgroup ARM does not match.
    pub fn bind_subgroup(
        &mut self,
        fence: &Fence,
        spawned: Spawned,
    ) -> Result<CohortChild, LaunchError> {
        if let Err(error) = self.check_bound(fence) {
            return Err(LaunchError::Unbound {
                spawned: Box::new(spawned),
                error,
            });
        }
        self.claim.bind_subgroup(spawned)
    }

    /// Retire only an actual registered subgroup after its scoped kernel drain.
    ///
    /// # Errors
    /// Leadership, actual receipt or durable subgroup binding is stale, or
    /// closure and its snapshot cannot be established. Other groups stay held.
    pub fn retire_subgroup(&mut self, fence: &Fence, subgroup: &CohortChild) -> io::Result<()> {
        self.check_bound(fence)?;
        self.claim.retire_subgroup(subgroup)
    }

    /// Cancel one exact subgroup no-child receipt while keeping the root bound.
    ///
    /// # Errors
    /// Preflight failures return the actual proof. Once accepted, cancellation
    /// persistence custody stays with the original claim, including on error.
    pub fn cancel_subgroup(&mut self, fence: &Fence, proof: NotSpawned) -> Result<(), CancelError> {
        if let Err(error) = self
            .check_bound(fence)
            .and_then(|()| self.claim.check_subgroup_cancel(&proof))
        {
            return Err(CancelError::Refused {
                error,
                proof: Box::new(proof),
            });
        }
        self.claim
            .cancel_subgroup(proof)
            .map_err(CancelError::Retained)
    }

    fn check_bound(&self, fence: &Fence) -> io::Result<()> {
        self.binding.check(fence)?;
        if self.phase != Phase::Bound {
            return Err(refused("retained unit has no bound subgroup authority"));
        }
        Ok(())
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
    /// capabilities. Once any CPU Free write starts an I/O error has an uncertain
    /// outcome; its consumed capability cannot be retried or reported settled.
    /// Remaining capabilities stay retained. No unit's settlement starts before
    /// the same original cohort's durable closure has been established.
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
        if self.cpu.is_empty() {
            return Err(refused("retained CPU settlement capability is missing"));
        }
        self.phase = Phase::SettlementUncertain;
        while let Some(cpu) = self.cpu.pop() {
            cpu.settle()?;
        }
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
