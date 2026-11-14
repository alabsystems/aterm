// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHARED FILES, aterm-db's side (docs/DESIGN-shared-files-2026-11-11.md): which files this
//! build compiles and the collections they project ([`types`]), the materializer that writes
//! their snapshots and faces from this Mac's replica ([`materialize`]), the publisher that
//! signs the writes this Mac queued ([`publish`]), the write verbs that queue them ([`write`]),
//! and `aterm file` ([`command`]).
//!
//! Reading a shared file is `aterm_file::File::read`, in a crate that cannot reach this one: it
//! answers from the snapshot whatever this Mac's custody is doing. Only the materializer here
//! opens custody, and it turns every refusal into a state word rather than an error.

pub mod command;
pub mod materialize;
pub mod publish;
pub mod types;
pub mod write;

pub use types::{ACCOUNTS, Derived, FILES, LINKS, MACHINES, Type, find};

use aterm_secrets::client::Paths;

/// Where this Mac's shared files live: `~/.aterm/files` of the home `paths` names. A test's
/// stand-in home so keeps its files in its own scratch folder.
#[must_use]
pub fn root(paths: &Paths) -> aterm_file::Root {
    aterm_file::Root::for_home(&paths.home)
}

/// Seams another crate's tests take a stand-in lab's home through: a writer succession its
/// strict readers refuse while a shared file still answers. Absent from release builds.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod fixture {
    use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

    /// Fence `paths` under a writer succession whose candidate is never admitted, so it stays
    /// pending (FL-14, ATERM-KI-1109-1): every strict reader of the home refuses with "Commons
    /// writer succession is still preserving old records", and a materializer pass records its
    /// records `held` (`succession`).
    pub fn fence_pending(paths: &Paths, bootstrap: &Bootstrap) {
        crate::generation::file_seams::fence_pending(paths, bootstrap);
    }
}

/// How many durable appends this process's table stores have made.
static GREW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Note that a table store of this process grew: a record this Mac authored, or one it took from
/// a peer. Called under the store's locks by `crate::table::store`, so it only counts; the
/// process that owns the Commons runtime reads the count ([`growth`]) and runs a materializer
/// pass once it has moved (`crate::alab::upkeep`).
pub(crate) fn grew() {
    GREW.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// How many times [`grew`] was called in this process.
pub(crate) fn growth() -> u64 {
    GREW.load(std::sync::atomic::Ordering::Relaxed)
}

/// Seams of this crate's tests: how often this thread reached for custody, a step run between
/// a pass's first look at its home's standing and its open of custody, and one run between a
/// pass's read and its record.
#[cfg(test)]
#[path = "file_seam_tests.rs"]
pub(crate) mod seam;

#[cfg(test)]
mod model_read_tests;
#[cfg(test)]
pub(crate) mod model_tests;
#[cfg(test)]
mod tests;
