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

    /// Ask this original dedicated group to terminate. The unreaped original
    /// leader pins its group number, including after the leader has exited.
    /// This proves neither group drain nor release of any resource.
    /// # Errors
    /// The original owner/scope or child custody changed, the leader was reaped,
    /// or the kernel refused the request.
    pub fn terminate_group(&mut self) -> io::Result<()> {
        self.signal_group(libc::SIGTERM)
    }

    /// Continue this original dedicated group, under the same original-child
    /// requirements as [`Self::terminate_group`]. This is no execution receipt.
    /// # Errors
    /// Original custody is unavailable or the kernel refused the request.
    pub fn continue_group(&mut self) -> io::Result<()> {
        self.signal_group(libc::SIGCONT)
    }

    /// Request uncatchable termination of this original dedicated group, under
    /// the same original-child requirements as [`Self::terminate_group`].
    /// Kernel acceptance is never a group-drain or resource-release receipt.
    /// # Errors
    /// Original custody is unavailable or the kernel refused the request.
    pub fn kill_group(&mut self) -> io::Result<()> {
        self.signal_group(libc::SIGKILL)
    }

    /// Observe complete native member populations while the same original
    /// unreaped leader still pins its group. This only chooses when an owning
    /// wait may run: exited members still keep the kernel group present, and
    /// actual kernel absence remains mandatory for resource retirement.
    ///
    /// # Errors
    /// Original child, birth or scope custody changed. An incomplete or
    /// unavailable member observation returns false, never drain authority.
    pub fn group_members_ended(&mut self) -> io::Result<bool> {
        let group = self.original_group()?;
        let ended = members_ended(group as u32);
        if self.original_group()? != group {
            return Err(io::Error::other(
                "original group changed during observation",
            ));
        }
        Ok(ended)
    }

    fn original_group(&mut self) -> io::Result<i32> {
        let group = i32::try_from(self.child_mut().id())
            .ok()
            .filter(|group| *group > 1)
            .ok_or_else(|| io::Error::other("invalid original dedicated group"))?;
        if !self.key().is_current()
            || !self.is_original_child()
            || !self.child_mut().owns_unreaped()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "group control requires its original current owner and unreaped dedicated birth",
            ));
        }
        Ok(group)
    }

    fn signal_group(&mut self, signal: i32) -> io::Result<()> {
        let group = self.original_group()?;
        // SAFETY: only the controlled dedicated-group spawn creates Spawned.
        // Its same original Child is exclusively borrowed, remains unreaped,
        // and pins group > 1 against number reuse even after terminal exit.
        // The actual owner, host, boot and namespace were checked above. No
        // caller-supplied pid, decoded record or replacement Child is accepted.
        if unsafe { libc::kill(-group, signal) } == 0 {
            return Ok(());
        }
        let refused = io::Error::last_os_error();
        let errno = refused.raw_os_error();
        process::settle_group_signal(group, refused)
            .map_err(|settled| errno.map_or(settled, io::Error::from_raw_os_error))
    }

    /// Surrender the receipt while retaining the actual child.
    #[must_use]
    pub fn into_child(self) -> Child {
        self.observed.into_child()
    }
}

// DATA for choosing the owning reap, moved from the existing admitted
// controller. A second complete listing cannot gain a new member; this is
// never kernel absence, resource settlement or permission to signal a record.
fn members_ended(group: u32) -> bool {
    let Some(first) = process::group_members(group).filter(|m| m.complete) else {
        return false;
    };
    let ended = |pid| {
        matches!(
            process::process_presence(pid),
            process::ProcessPresence::Exited | process::ProcessPresence::Absent
        )
    };
    if !first.pids.iter().all(|&pid| ended(pid)) {
        return false;
    }
    let Some(second) = process::group_members(group).filter(|m| m.complete) else {
        return false;
    };
    second
        .pids
        .iter()
        .all(|pid| first.pids.contains(pid) && ended(*pid))
}

/// This exact one-use attempt did not start a target. Its original process cancelled it
/// before native birth, or native spawn failed while that process and scope stayed current.
/// A postbirth target-exec or observation error cannot construct this receipt.
#[derive(Debug)]
pub struct NotSpawned {
    key: BirthKey,
    error: io::Error,
}

/// A controlled no-child outcome or a refusal carrying no cancellation proof. A stale copy
/// of an attempt cannot certify that its original owner did not start a child.
#[derive(Debug)]
pub enum SpawnFailure<R = Spawned> {
    /// This exact attempt completed native spawn without starting a child.
    NotSpawned(Box<NotSpawned>),
    /// No native child was returned and no authoritative no-child conclusion was produced.
    Refused(io::Error),
    /// Native birth occurred, but target exec observation failed. This retains the same
    /// actual owned receipt; neither the diagnostic nor leader exit proves cohort closure.
    Unbound {
        /// The actual child sealed before observation, never reconstructed from a PID.
        spawned: Box<R>,
        /// The original exec-status observation or reported target-exec error.
        error: io::Error,
    },
}

impl<R> SpawnFailure<R> {
    /// Keep a genuine no-child proof, or preserve the entire unresolved failure owner.
    ///
    /// # Errors
    /// The original failure, including actual postbirth custody when present.
    pub fn into_not_spawned(self) -> Result<NotSpawned, Self> {
        match self {
            Self::NotSpawned(proof) => Ok(*proof),
            other => Err(other),
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
    spawn: impl FnOnce() -> io::Result<super::no_fork::PendingChild>,
) -> Result<ObservedSpawned, SpawnFailure<ObservedSpawned>> {
    observe_as(attempt, spawn, |observed| observed)
}

pub(super) fn observe_cohort(
    attempt: BirthAttempt,
    spawn: impl FnOnce() -> io::Result<super::no_fork::PendingChild>,
) -> Result<Spawned, SpawnFailure<Spawned>> {
    observe_as(attempt, spawn, |observed| Spawned { observed })
}

fn observe_as<R>(
    attempt: BirthAttempt,
    spawn: impl FnOnce() -> io::Result<super::no_fork::PendingChild>,
    seal: impl FnOnce(ObservedSpawned) -> R,
) -> Result<R, SpawnFailure<R>> {
    if !attempt.key.is_current() {
        return Err(SpawnFailure::Refused(io::Error::other(
            "spawn attempt belongs to another process or kernel scope",
        )));
    }
    match spawn() {
        Ok(pending) => pending
            .observe(|child| {
                seal(ObservedSpawned {
                    custody: child.custody_key(),
                    key: attempt.key,
                    child,
                })
            })
            .map_err(|(spawned, error)| SpawnFailure::Unbound {
                spawned: Box::new(spawned),
                error,
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

#[cfg(test)]
#[path = "spawnfd_birth_tests.rs"]
mod tests;
