// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One-use launch attempts and actual spawn outcomes, never reconstructed from records.

use std::io;

use super::Child;
use crate::process::{self, Identity};

/// An observation key, not permission to spawn or proof of a failed spawn.
#[derive(Clone, PartialEq, Eq)]
pub struct BirthKey {
    nonce: String,
    owner: Identity,
    host: String,
    boot: String,
    namespace: Option<String>,
}

impl std::fmt::Debug for BirthKey {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("BirthKey")
            .field("pid", &self.owner.pid)
            .finish_non_exhaustive()
    }
}

impl BirthKey {
    /// The random identifier for this one attempt, suitable for its durable arm record.
    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// The original process that created the attempt.
    #[must_use]
    pub const fn owner(&self) -> Identity {
        self.owner
    }

    /// The captured OS machine identity.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The captured boot.
    #[must_use]
    pub fn boot(&self) -> &str {
        &self.boot
    }

    /// The captured pid namespace (none on Darwin).
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// Whether this process still supplies the exact captured kernel identity and scope.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.owner.pid == std::process::id() as i32
            && process::read_identity(self.owner.pid) == Some(self.owner)
            && process::host_id().as_deref() == Some(self.host.as_str())
            && process::boot_id().as_deref() == Some(self.boot.as_str())
            && process::pid_namespace() == self.namespace
    }
}

/// A one-use actual spawn attempt. Only its read-only key can be cloned or persisted.
/// Consuming it in a controlled spawn prevents a prior failure from cancelling a later birth.
#[derive(Debug)]
pub struct BirthAttempt {
    key: BirthKey,
}

impl BirthAttempt {
    /// Capture this actual process and a fresh nonce before durably arming a launch.
    ///
    /// # Errors
    /// The actual process, OS machine, boot or required pid namespace is unreadable, changes,
    /// or the kernel random source fails. No child is started.
    pub fn new() -> io::Result<Self> {
        let unknown = || io::Error::other("spawn attempt kernel scope is unknown or changed");
        let owner = process::read_identity(std::process::id() as i32).ok_or_else(unknown)?;
        let host = process::host_id().ok_or_else(unknown)?;
        let boot = process::boot_id().ok_or_else(unknown)?;
        let namespace = process::pid_namespace();
        if cfg!(target_os = "linux") && namespace.is_none() {
            return Err(unknown());
        }
        let key = BirthKey {
            nonce: crate::rand::hex_token::<16>()?,
            owner,
            host,
            boot,
            namespace,
        };
        if !key.is_current() {
            return Err(unknown());
        }
        Ok(Self { key })
    }

    /// The observation to record durably before passing this attempt to a spawn.
    #[must_use]
    pub const fn key(&self) -> &BirthKey {
        &self.key
    }

    /// Surrender this unused attempt in its original process before calling any spawn.
    /// A forked process may hold a memory copy but cannot prove the original did not launch.
    ///
    /// # Errors
    /// The original process identity or kernel scope is no longer readable and current.
    pub fn cancel(self) -> io::Result<NotSpawned> {
        if !self.key.is_current() {
            return Err(io::Error::other(
                "only the original current process can cancel an unused spawn attempt",
            ));
        }
        Ok(NotSpawned {
            key: self.key,
            error: io::Error::new(
                io::ErrorKind::Interrupted,
                "spawn attempt cancelled before execution",
            ),
        })
    }
}

/// The actual owned child and its exact attempt, with the command's group/session policy
/// preserved. This is not proof that the child started a dedicated process group.
/// Neither this proof nor a [`NotSpawned`] can be manufactured by decoding a journal.
///
/// A generic observed child cannot become a dedicated-group receipt, including after exit:
///
/// ```compile_fail,E0308
/// use aterm_uds::spawnfd::{ObservedSpawned, Spawned};
/// fn dedicated(_: Spawned) {}
/// fn observed(child: ObservedSpawned) { dedicated(child); }
/// ```
#[derive(Debug)]
pub struct ObservedSpawned {
    key: BirthKey,
    child: Child,
    custody: std::sync::Arc<()>,
}

impl ObservedSpawned {
    /// Whether mutable borrowing still holds this receipt's exact original
    /// Child handle without a kernel-reported loss of wait custody. Moving it preserves
    /// identity; replacing it does not, even
    /// if the replacement later obtains the same numeric pid.
    #[must_use]
    pub fn is_original_child(&self) -> bool {
        self.child.owns_custody(&self.custody)
    }

    /// The actual attempt observation, for comparison with its durable arm record.
    #[must_use]
    pub const fn key(&self) -> &BirthKey {
        &self.key
    }

    /// The actual owned child; no caller-supplied numeric pid is accepted.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Hand the child to its supervisor while surrendering the attempt receipt.
    #[must_use]
    pub fn into_child(self) -> Child {
        self.child
    }
}

/// The actual owned child and the exact attempt that started its dedicated process group.
/// Only the controlled cohort spawn constructs this receipt. A generic [`ObservedSpawned`]
/// cannot supply the dedicated birth fact after its child exits.
#[derive(Debug)]
pub struct Spawned {
    observed: ObservedSpawned,
}

impl Spawned {
    /// Whether this receipt still holds the original native child handle without
    /// a kernel-reported loss of wait custody.
    #[must_use]
    pub fn is_original_child(&self) -> bool {
        self.observed.is_original_child()
    }

    /// The actual attempt observation, for comparison with its durable arm record.
    #[must_use]
    pub const fn key(&self) -> &BirthKey {
        self.observed.key()
    }

    /// Borrow the actual child owned by this controlled cohort launch.
    pub fn child_mut(&mut self) -> &mut Child {
        self.observed.child_mut()
    }

    /// Surrender the receipt while retaining the actual child.
    #[must_use]
    pub fn into_child(self) -> Child {
        self.observed.into_child()
    }
}

/// This exact one-use attempt did not start a target. Its original process cancelled it
/// before execution, or the controlled spawn failed while that process and scope stayed current.
/// The public command has no caller pre-exec callbacks; spawn errors precede target execution.
#[derive(Debug)]
pub struct NotSpawned {
    key: BirthKey,
    error: io::Error,
}

/// A controlled no-child outcome or a refusal carrying no cancellation proof. A stale copy
/// of an attempt cannot certify that its original owner did not start a child.
#[derive(Debug)]
pub enum SpawnFailure {
    /// This exact attempt completed its controlled spawn without starting the target.
    NotSpawned(Box<NotSpawned>),
    /// No authoritative no-child conclusion was produced.
    Refused(io::Error),
}

impl SpawnFailure {
    /// Keep a genuine no-child proof, or the refusal that could not supply one.
    ///
    /// # Errors
    /// The original refusal when this failure contains no cancellation proof.
    pub fn into_not_spawned(self) -> io::Result<NotSpawned> {
        match self {
            Self::NotSpawned(proof) => Ok(*proof),
            Self::Refused(error) => Err(error),
        }
    }

    /// Discard any cancellation proof and retain the diagnostic error.
    #[must_use]
    pub fn into_error(self) -> io::Error {
        match self {
            Self::NotSpawned(proof) => proof.into_error(),
            Self::Refused(error) => error,
        }
    }
}

impl NotSpawned {
    /// The attempt that did not start, for comparison with a claim's durable arm record.
    #[must_use]
    pub const fn key(&self) -> &BirthKey {
        &self.key
    }

    /// Why the controlled spawn did not start a child.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Keep the error while surrendering this attempt's cancellation proof.
    #[must_use]
    pub fn into_error(self) -> io::Error {
        self.error
    }
}

// Only spawnfd's controlled command implementations can produce these outcomes; they expose
// no arbitrary caller pre-exec callback. The cohort entry additionally forces a dedicated group.
pub(super) fn observe(
    attempt: BirthAttempt,
    spawn: impl FnOnce() -> io::Result<Child>,
) -> Result<ObservedSpawned, SpawnFailure> {
    if !attempt.key.is_current() {
        return Err(SpawnFailure::Refused(io::Error::other(
            "spawn attempt belongs to another process or kernel scope",
        )));
    }
    match spawn() {
        Ok(child) => Ok(ObservedSpawned {
            custody: child.custody_key(),
            key: attempt.key,
            child,
        }),
        Err(error) if attempt.key.is_current() => {
            Err(SpawnFailure::NotSpawned(Box::new(NotSpawned {
                key: attempt.key,
                error,
            })))
        }
        Err(error) => Err(SpawnFailure::Refused(io::Error::other(format!(
            "spawn scope changed before its failure could be certified: {error}"
        )))),
    }
}

pub(super) fn observe_cohort(
    attempt: BirthAttempt,
    spawn: impl FnOnce() -> io::Result<Child>,
) -> Result<Spawned, SpawnFailure> {
    observe(attempt, spawn).map(|observed| Spawned { observed })
}

#[cfg(test)]
#[path = "spawnfd_birth_tests.rs"]
mod tests;
