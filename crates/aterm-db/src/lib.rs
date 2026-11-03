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
#[cfg(test)]
aterm_secrets::memoized_x25519_in_tests!();

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
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Error").field(&self.0).finish()
    }
}
/// What [`Error::busy`] says.
const BUSY: &str = "another local writer holds this table; nothing changed, retry shortly";
const SNAPSHOT_CHANGED: &str = "table witness advanced during observation; reopen and verify";
impl Error {
    pub(crate) fn new(reason: &str) -> Self {
        Self(reason.into(), FailureKind::Other)
    }
    /// Another local writer held the store this needed, and nothing changed.
    pub(crate) fn busy() -> Self {
        Self(BUSY.into(), FailureKind::Busy)
    }
    /// A newer witness needs fresh verification; the changed handle refuses reads.
    pub(crate) fn snapshot_changed() -> Self {
        Self(SNAPSHOT_CHANGED.into(), FailureKind::Busy)
    }
    /// Whether local contention or a newer snapshot requires a fresh observation.
    pub(crate) fn is_busy(&self) -> bool {
        matches!(self.1, FailureKind::Busy)
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
        Self(value.to_string(), FailureKind::Other)
    }
}
impl From<aterm_secrets::Error> for Error {
    fn from(value: aterm_secrets::Error) -> Self {
        let kind = match value.kind() {
            aterm_secrets::ErrorKind::JournalBusy
            | aterm_secrets::ErrorKind::CustodyBusy
            | aterm_secrets::ErrorKind::VaultBusy
            | aterm_secrets::ErrorKind::VaultChanged => FailureKind::Busy,
            _ => FailureKind::Other,
        };
        Self(value.to_string(), kind)
    }
}
/// Database boundary result.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, unix))]
mod error_tests;
