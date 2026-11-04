// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native launch handoff: durable ARM, actual owned birth, then durable BIND.
//!
//! Only a bound launch returns success. Error paths retain the real child or exact no-child
//! receipt for the controller; an I/O error never supplies evidence that capacity is free.
//! The existing private namespace and kernel group-drain contracts remain authoritative.

use super::Cohort;
use crate::Claim;
use aterm_uds::spawnfd::{Child, Command, NotSpawned, SpawnFailure, Spawned};
use std::{fmt, io};

/// An actual owned child whose exact birth was durably bound to its claim.
/// This proves neither descendant containment nor permission to release capacity on exit.
#[derive(Debug)]
pub struct CohortChild {
    spawned: Spawned,
    cohort: Cohort,
}

impl CohortChild {
    /// The owned child, retained even when it has exited but has not been reaped.
    pub fn child_mut(&mut self) -> &mut Child {
        self.spawned.child_mut()
    }

    /// Its actual bound cohort, not one decoded from a saved observation.
    #[must_use]
    pub const fn cohort(&self) -> &Cohort {
        &self.cohort
    }

    /// Transfer the actual child receipt and bound cohort to the native controller.
    #[must_use]
    pub fn into_parts(self) -> (Spawned, Cohort) {
        (self.spawned, self.cohort)
    }
}

/// A launch that cannot acknowledge bound work. No variant authorizes freeing a reservation.
#[derive(Debug)]
pub enum LaunchError {
    /// No authoritative no-child receipt was obtained. Retain uncertain custody.
    Refused(io::Error),
    /// This exact controlled attempt started no target. Pass the receipt to
    /// [`Claim::cancel_cohort`] before treating its durable ARM as cancelled.
    NotSpawned(Box<NotSpawned>),
    /// The target actually started, but BIND did not complete. Keep the owned child,
    /// retry its binding or supervise drain, and retain the original reservation.
    Unbound {
        /// The actual child and one-use birth receipt, never a caller-selected pid.
        spawned: Box<Spawned>,
        /// Why the durable binding could not be acknowledged.
        error: io::Error,
    },
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(error) => write!(f, "cohort launch refused: {error}"),
            Self::NotSpawned(proof) => write!(f, "cohort target did not start: {}", proof.error()),
            Self::Unbound { error, .. } => {
                write!(f, "cohort child started but remains unbound: {error}")
            }
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Refused(error) | Self::Unbound { error, .. } => error,
            Self::NotSpawned(proof) => proof.error(),
        })
    }
}

// The private binder seam lets runtime controls fail after real birth, without installing a
// callback in the public launch API or a pre-exec callback in the child's command.
fn launch_with(
    claim: &mut Claim,
    command: &mut Command,
    bind: impl FnOnce(&mut Claim, &mut Spawned) -> io::Result<Cohort>,
) -> Result<CohortChild, LaunchError> {
    let attempt = claim.arm_cohort().map_err(LaunchError::Refused)?;
    let mut spawned = command
        .spawn_cohort(attempt)
        .map_err(|failure| match failure {
            SpawnFailure::NotSpawned(proof) => LaunchError::NotSpawned(proof),
            SpawnFailure::Refused(error) => LaunchError::Refused(error),
        })?;
    match bind(claim, &mut spawned) {
        Ok(cohort) => Ok(CohortChild { spawned, cohort }),
        Err(error) => Err(LaunchError::Unbound {
            spawned: Box::new(spawned),
            error,
        }),
    }
}

pub(crate) fn launch(claim: &mut Claim, command: &mut Command) -> Result<CohortChild, LaunchError> {
    launch_with(claim, command, Claim::bind_cohort)
}

pub(crate) fn launch_member(
    claim: &Claim,
    leader: &mut CohortChild,
    command: &mut Command,
) -> io::Result<Child> {
    let refused = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cohort member needs its original live owned leader and current durable binding",
        )
    };
    if !claim.cohort_bound(&leader.cohort) || !leader.spawned.key().is_current() {
        return Err(refused());
    }
    // Require a live self-led child, rather than the fast-exit observation accepted by BIND.
    // The mutable owned-child borrow pins its unreaped pid through the native group join.
    let current = Cohort::of_child(leader.spawned.child_mut())?;
    if current != leader.cohort || !claim.cohort_bound(&current) {
        return Err(refused());
    }
    // BIND already durably reserves this entire group's lifetime. Joining another member
    // changes neither journal state nor release authority; an error supplies no cancellation.
    command.spawn_in_process_group(current.group())
}

#[cfg(test)]
#[path = "cohort_launch_tests.rs"]
mod tests;
