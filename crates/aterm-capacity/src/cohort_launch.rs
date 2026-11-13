// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native launch handoff: durable ARM, actual owned birth, then durable BIND.
//!
//! Only a bound launch returns success. Error paths retain the real child or exact no-child
//! receipt for the controller; an I/O error never supplies evidence that capacity is free.
//! The existing private namespace and kernel group-drain contracts remain authoritative.

use super::{Cohort, Scope};
use crate::Claim;
use aterm_uds::spawnfd::{Child, Command, NotSpawned, SpawnFailure, Spawned};
use std::{fmt, io};

/// An explicit controller request, never evidence of group drain or capacity release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CohortSignal {
    /// Ask every current member to terminate (SIGTERM).
    Terminate,
    /// Explicitly continue stopped members (SIGCONT).
    Continue,
    /// Request uncatchable termination of current members (SIGKILL).
    Kill,
}

/// An actual owned child whose exact birth was durably bound to its claim.
/// This proves neither descendant containment nor permission to release capacity on exit.
#[derive(Debug)]
pub struct CohortChild {
    spawned: Spawned,
    cohort: Cohort,
}

impl CohortChild {
    // Observation only, used to bind a separately owned original epoch loan to
    // this actual launch before GO. It is never a constructor or release proof.
    pub(crate) fn birth_key(&self) -> &aterm_uds::spawnfd::BirthKey {
        self.spawned.key()
    }

    /// The owned child, retained even when it has exited but has not been reaped.
    pub fn child_mut(&mut self) -> &mut Child {
        self.spawned.child_mut()
    }

    /// Its actual bound cohort, not one decoded from a saved observation.
    #[must_use]
    pub const fn cohort(&self) -> &Cohort {
        &self.cohort
    }

    /// Request a signal for this actual launch's original dedicated group. The
    /// exclusive unreaped-child borrow pins its original group number through
    /// the syscall, even if its leader has exited. A decoded observation, reaped
    /// leader or copied foreign-owner receipt supplies no authority.
    ///
    /// Success means only kernel acceptance, or a group whose members have all
    /// ended ([`aterm_uds::process::settle_group_signal`]), never termination,
    /// execution or drain. No reservation is changed. Members that escape remain outside this
    /// original group's contract; no signal is sent to a newly observed group.
    /// # Errors
    /// The original owner/scope changed or is unreadable, the leader was reaped,
    /// the original dedicated birth does not match, or the kernel refused the signal.
    pub fn request_group(&mut self, signal: CohortSignal) -> io::Result<()> {
        let group = self.cohort.group();
        if !self.spawned.key().is_current()
            || !self.spawned.is_original_child()
            || Scope::of_key(self.spawned.key()) != self.cohort.scope
            || self.spawned.child_mut().is_reaped()
            || group <= 1
            || i32::try_from(self.spawned.child_mut().id()).ok() != Some(group)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "group control requires its original current owner and unreaped dedicated birth",
            ));
        }
        // Keep admission's original bound-cohort checks above. The native
        // birth owner additionally checks its exact unreaped Child before the
        // syscall; no observation or reservation creates signalling authority.
        match signal {
            CohortSignal::Terminate => self.spawned.terminate_group(),
            CohortSignal::Continue => self.spawned.continue_group(),
            CohortSignal::Kill => self.spawned.kill_group(),
        }
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
    /// [`Claim::cancel_cohort`] or [`Claim::cancel_subgroup`] for the corresponding launch
    /// before treating its durable ARM as cancelled.
    NotSpawned(Box<NotSpawned>),
    /// Native birth occurred, but exec observation or BIND did not complete. Keep the owned child,
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
pub(crate) fn launch_with(
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
            SpawnFailure::Unbound { spawned, error } => LaunchError::Unbound { spawned, error },
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

pub(crate) fn launch_subgroup_with(
    claim: &mut Claim,
    root: &mut CohortChild,
    command: &mut Command,
    bind: impl FnOnce(&mut Claim, &mut Spawned) -> io::Result<Cohort>,
) -> Result<CohortChild, LaunchError> {
    // An actual live original root supplies launch authority. A numeric observation, a
    // subgroup masquerading as the root, or a replaced child cannot register new work.
    if !root.spawned.key().is_current() || !root.spawned.is_original_child() {
        return Err(LaunchError::Refused(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "subgroup launch needs the original current root child",
        )));
    }
    let current = Cohort::of_child(root.spawned.child_mut()).map_err(LaunchError::Refused)?;
    if current != root.cohort {
        return Err(LaunchError::Refused(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "subgroup root changed",
        )));
    }
    let attempt = claim
        .arm_subgroup(&root.cohort)
        .map_err(LaunchError::Refused)?;
    let mut spawned = command
        .spawn_cohort(attempt)
        .map_err(|failure| match failure {
            SpawnFailure::NotSpawned(proof) => LaunchError::NotSpawned(proof),
            SpawnFailure::Refused(error) => LaunchError::Refused(error),
            SpawnFailure::Unbound { spawned, error } => LaunchError::Unbound { spawned, error },
        })?;
    match bind(claim, &mut spawned) {
        Ok(cohort) => Ok(CohortChild { spawned, cohort }),
        Err(error) => Err(LaunchError::Unbound {
            spawned: Box::new(spawned),
            error,
        }),
    }
}

pub(crate) fn launch_subgroup(
    claim: &mut Claim,
    root: &mut CohortChild,
    command: &mut Command,
) -> Result<CohortChild, LaunchError> {
    launch_subgroup_with(claim, root, command, Claim::bind_subgroup_actual)
}

pub(crate) fn bind_subgroup(
    claim: &mut Claim,
    mut spawned: Spawned,
) -> Result<CohortChild, LaunchError> {
    match claim.bind_subgroup_actual(&mut spawned) {
        Ok(cohort) => Ok(CohortChild { spawned, cohort }),
        Err(error) => Err(LaunchError::Unbound {
            spawned: Box::new(spawned),
            error,
        }),
    }
}

pub(crate) fn retire_subgroup(claim: &mut Claim, subgroup: &CohortChild) -> io::Result<()> {
    if !subgroup.spawned.key().is_current() || !subgroup.spawned.is_original_child() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "subgroup retirement needs the original actual child",
        ));
    }
    claim.retire_subgroup_actual(subgroup.spawned.key(), &subgroup.cohort)
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
    if !claim.cohort_bound(&leader.cohort)
        || !leader.spawned.key().is_current()
        || !leader.spawned.is_original_child()
    {
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

#[cfg(test)]
#[path = "cohort_subgroup_tests.rs"]
mod subgroup_tests;
