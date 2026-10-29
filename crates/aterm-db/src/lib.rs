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
pub mod catalog;
#[cfg(unix)]
pub mod cli;
pub mod contacts;
#[cfg(unix)]
pub mod control;
#[cfg(unix)]
pub mod credentials;
pub mod crypto;
pub mod database;
mod documents;
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

/// A refusal that identifies the failed boundary without quoting payloads or keys.
#[derive(Debug)]
pub struct Error(pub String);
/// What [`Error::busy`] says.
const BUSY: &str = "another local writer holds this table; nothing changed, retry shortly";
const SNAPSHOT_CHANGED: &str = "table witness advanced during observation; reopen and verify";
impl Error {
    pub(crate) fn new(reason: &str) -> Self {
        Self(reason.into())
    }
    /// Another local writer held the store this needed, and nothing changed.
    pub(crate) fn busy() -> Self {
        Self(BUSY.into())
    }
    /// A newer witness needs fresh verification; the changed handle refuses reads.
    pub(crate) fn snapshot_changed() -> Self {
        Self(SNAPSHOT_CHANGED.into())
    }
    /// Whether local contention or a newer snapshot requires a fresh observation.
    pub(crate) fn is_busy(&self) -> bool {
        self.0 == BUSY || self.0 == SNAPSHOT_CHANGED
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
        Self(value.to_string())
    }
}
/// Database boundary result.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(all(test, unix))]
mod tests;
