// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Embedded replicated application database over astream. Every replica owns its
//! local strict journal; authenticated immutable operations merge after partitions.
//! One replica needs no service. Any two may exchange directly. See the database
//! design for offline revocation, rollback and compromised-writer assumptions.
#![forbid(unsafe_code)]
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

use std::fmt;

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

pub mod accounts;
#[cfg(unix)]
pub mod admission;
pub mod alab;
#[cfg(unix)]
pub mod att;
#[cfg(unix)]
pub mod catalog;
#[cfg(unix)]
pub mod cli;
#[cfg(unix)]
pub mod clock;
pub mod contacts;
#[cfg(unix)]
pub mod control;
#[cfg(unix)]
pub mod credentials;
pub mod crypto;
pub mod database;
mod documents;
#[cfg(unix)]
pub mod fleet_db;
#[cfg(unix)]
mod generation;
#[cfg(unix)]
pub mod import;
#[cfg(unix)]
pub mod local;
pub mod machines;
#[cfg(unix)]
pub mod network;
#[cfg(unix)]
pub mod network_state;
#[cfg(unix)]
pub mod replica;
#[cfg(unix)]
pub mod resources;
#[cfg(unix)]
pub mod runtime;
#[cfg(unix)]
pub mod service_state;
#[cfg(unix)]
pub mod table;
pub mod typed;

/// Resolve application storage to an already admitted, fully preserved writer
/// generation. Call before opening its secrets bootstrap or table directory.
/// This starts no network work and neither enrolls nor replaces storage.
///
/// # Errors
/// Returns an error if retained selection or custody state is unavailable,
/// incomplete, changed or no longer authorized.
#[cfg(unix)]
pub fn application_paths(
    paths: &aterm_secrets::client::Paths,
) -> Result<aterm_secrets::client::Paths> {
    generation::selected(paths)
}

/// Resolve existing admitted application custody under one caller-owned
/// deadline. This changes no enrollment, storage or caller configuration.
#[cfg(unix)]
pub fn application_paths_until(
    paths: &aterm_secrets::client::Paths,
    deadline: std::time::Instant,
) -> Result<aterm_secrets::client::Paths> {
    generation::selected_until(paths, deadline)
}

/// Observe authenticated predecessor and selected Writer identities under one
/// deadline. These metadata identities grant no publication or key authority.
#[cfg(unix)]
pub fn application_lineage_until(
    paths: &aterm_secrets::client::Paths,
    deadline: std::time::Instant,
) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
    generation::lineage_until(paths, deadline)
}

/// Complete a real writer-successor fixture from two already admitted private
/// homes beneath one owned temporary directory. No CLI exposes this test seam.
#[doc(hidden)]
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn test_complete_writer_successor(
    fixture: &std::path::Path,
    original: &aterm_secrets::client::Paths,
    successor: &aterm_secrets::client::Paths,
    config: &std::path::Path,
    deadline: std::time::Instant,
) -> Result<aterm_secrets::client::Paths> {
    generation::test_complete_successor(fixture, original, successor, config, deadline)
}

/// Authenticate preserved writer-recovery prefixes without importing records.
/// Development observations expose counts and bounded refusals, never values.
///
/// # Errors
/// Recovery selection, policy, source custody or the admitted candidate cannot
/// be opened. Individual table refusals remain in the returned observation.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn inspect_writer_recovery(paths: &aterm_secrets::client::Paths) -> Result<aterm_json::Value> {
    generation::inspect(paths)
}

/// A refusal that identifies the failed boundary without quoting payloads or keys.
pub struct Error(pub String, FailureKind);

#[derive(Clone, Copy)]
enum FailureKind {
    Other,
    Busy,
    SnapshotChanged,
    Repair(aterm_types::network_state::RepairCause),
    #[cfg(unix)]
    EnrollmentPending,
    /// A record names a writer grant or a roster this Mac's membership view does not hold yet.
    #[cfg(unix)]
    AwaitingMembership,
    /// A table write a held `Remove` keeps pending until its rotation arrives (one-mesh M4).
    #[cfg(unix)]
    WritePending,
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Error").field(&self.0).finish()
    }
}
/// What [`Error::busy`] says.
const BUSY: &str = "another local writer holds this table; nothing changed, retry shortly";
const SNAPSHOT_CHANGED: &str = "table witness advanced during observation; reopen and verify";
#[cfg(unix)]
const ENROLLMENT_PENDING: &str = "table enrollment is not yet complete";
impl Error {
    pub(crate) fn repair(reason: &str, cause: aterm_types::network_state::RepairCause) -> Self {
        Self(reason.into(), FailureKind::Repair(cause))
    }
    pub(crate) fn repair_cause(&self) -> aterm_types::network_state::RepairCause {
        use aterm_types::network_state::RepairCause;
        match self.1 {
            FailureKind::Repair(cause) => cause,
            FailureKind::Busy | FailureKind::SnapshotChanged => RepairCause::NotReady,
            #[cfg(unix)]
            FailureKind::EnrollmentPending
            | FailureKind::AwaitingMembership
            | FailureKind::WritePending => RepairCause::NotReady,
            FailureKind::Other => RepairCause::Other,
        }
    }
    pub(crate) fn new(reason: &str) -> Self {
        Self(reason.into(), FailureKind::Other)
    }
    /// Another local writer held the store this needed, and nothing changed.
    pub(crate) fn busy() -> Self {
        Self(BUSY.into(), FailureKind::Busy)
    }
    /// A newer witness needs fresh verification; the changed handle refuses reads.
    pub(crate) fn snapshot_changed() -> Self {
        Self(SNAPSHOT_CHANGED.into(), FailureKind::SnapshotChanged)
    }
    /// A valid first enrollment has not published ready storage yet.
    #[cfg(unix)]
    pub(crate) fn enrollment_pending() -> Self {
        Self(ENROLLMENT_PENDING.into(), FailureKind::EnrollmentPending)
    }
    /// Whether an observation met a validated enrollment still in progress.
    #[cfg(unix)]
    pub(crate) fn is_enrollment_pending(&self) -> bool {
        matches!(self.1, FailureKind::EnrollmentPending)
    }
    /// A record whose writer grant or roster is not in this Mac's membership view yet: it may
    /// authenticate after a membership change. Inside `untyped_membership` on this thread
    /// (tests), it is an untyped refusal, as before the classification existed.
    #[cfg(unix)]
    pub(crate) fn awaiting_membership(reason: &str) -> Self {
        #[cfg(test)]
        if UNTYPED_MEMBERSHIP.with(std::cell::Cell::get) {
            return Self::new(reason);
        }
        Self(reason.into(), FailureKind::AwaitingMembership)
    }
    /// A table write kept pending, sealed to this Mac, until the removal's rotation it waits on
    /// arrives (one-mesh M4): nothing is lost, and nothing was published.
    #[cfg(unix)]
    pub(crate) fn write_pending() -> Self {
        Self(
            "the table write is kept pending until the removal's rotation arrives".into(),
            FailureKind::WritePending,
        )
    }
    /// Whether a table write was kept pending for a removal's rotation ([`Self::write_pending`]):
    /// the store seals it under the new epoch once that arrives.
    #[must_use]
    pub fn is_write_pending(&self) -> bool {
        #[cfg(unix)]
        {
            matches!(self.1, FailureKind::WritePending)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
    /// Whether a record waits for this Mac's membership view: its writer is not a seat it knows
    /// at the record's epoch, or its roster is one this Mac has not seen. A later membership
    /// change can admit it; nothing about the record itself is refused.
    #[must_use]
    pub fn is_awaiting_membership(&self) -> bool {
        #[cfg(unix)]
        {
            matches!(self.1, FailureKind::AwaitingMembership)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
    /// Whether local contention or a newer snapshot requires a fresh observation.
    pub(crate) fn is_busy(&self) -> bool {
        matches!(self.1, FailureKind::Busy | FailureKind::SnapshotChanged)
    }
    /// Whether what a read observed moved on while it read: nothing is wrong with the store, and
    /// a fresh observation is the answer.
    pub(crate) fn is_snapshot_changed(&self) -> bool {
        matches!(self.1, FailureKind::SnapshotChanged)
    }

    /// Add local diagnostic context without changing the failure's classification.
    pub(crate) fn context(self, context: &str) -> Self {
        Self(format!("{context}: {}", self.0), self.1)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        // An I/O WouldBlock can be a network timeout, not local writer contention.
        Self(
            value.to_string(),
            FailureKind::Repair(aterm_types::network_state::RepairCause::Io),
        )
    }
}
impl From<aterm_secrets::Error> for Error {
    fn from(value: aterm_secrets::Error) -> Self {
        let kind = match value.kind() {
            aterm_secrets::ErrorKind::JournalBusy
            | aterm_secrets::ErrorKind::CustodyBusy
            | aterm_secrets::ErrorKind::VaultBusy
            | aterm_secrets::ErrorKind::VaultChanged => FailureKind::Busy,
            aterm_secrets::ErrorKind::MembershipCapacity => {
                FailureKind::Repair(aterm_types::network_state::RepairCause::Quota)
            }
            aterm_secrets::ErrorKind::MembershipHistoryUnavailable => {
                FailureKind::Repair(aterm_types::network_state::RepairCause::NotReady)
            }
            _ => FailureKind::Other,
        };
        Self(value.to_string(), kind)
    }
}
#[cfg(all(unix, test))]
std::thread_local! {
    static UNTYPED_MEMBERSHIP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `run` with [`Error::awaiting_membership`] untyped on this thread: the negative control of
/// a test that a waiting record is told apart from a refused one.
#[cfg(all(unix, test))]
pub(crate) fn untyped_membership<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNTYPED_MEMBERSHIP.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(UNTYPED_MEMBERSHIP.with(|slot| slot.replace(true)));
    run()
}

/// Database boundary result.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, unix))]
mod error_tests;
