// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only observations of repeated Git pack files. A plan is never deletion,
//! writer-custody, Git-object-validity, or physical-recovery authority.

use serde::Serialize;
use std::io;
use std::path::Path;
use std::time::Duration;

pub mod cli;
#[cfg(unix)]
mod unix;

const MAX_ROOT_COMPONENTS: usize = 128;
const MAX_ROOT_PATH_BYTES: usize = 4096;

/// Finite work limits; elapsed time is checked between filesystem operations,
/// and cannot preempt a stalled operating-system call.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_entries: usize,
    pub max_depth: usize,
    pub max_packs: usize,
    pub max_pack_bytes: u64,
    /// Actual bytes read across BOTH files, including the EOF lookahead.
    /// Zero deliberately selects metadata-only planning, including empty packs.
    pub verify_io_bytes: u64,
    pub max_elapsed: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_depth: 32,
            max_packs: 8_192,
            max_pack_bytes: 64 * 1024 * 1024 * 1024,
            verify_io_bytes: 0,
            max_elapsed: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Pack {
    pub relative_path: String,
    pub bytes: u64,
    pub device: u64,
    pub inode: u64,
    pub links: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairState {
    Candidate,
    ByteEqual,
    ByteMismatch,
    Deferred,
    Refused,
}

#[derive(Clone, Debug, Serialize)]
pub struct Pair {
    pub source: String,
    pub target: String,
    pub bytes: u64,
    pub state: PairState,
    pub reason: String,
    /// Present only after complete exact comparison and streamed SHA-256.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub schema: &'static str,
    pub git_common_dir: String,
    pub observation_complete: bool,
    pub limits: PlanLimits,
    pub observation_scope: &'static str,
    pub mutation_available: bool,
    pub mutation_reason: &'static str,
    /// Always unknown: logical files can already share APFS extents.
    pub physical_savings_bytes: Option<u64>,
    pub entries_observed: usize,
    pub unique_pack_inodes: usize,
    pub hardlink_alias_paths: usize,
    pub unique_logical_bytes: u64,
    pub candidate_duplicate_logical_bytes: u64,
    pub byte_equal_duplicate_logical_bytes: u64,
    pub verification_io_bytes: u64,
    pub packs: Vec<Pack>,
    pub pairs: Vec<Pair>,
    pub issues: Vec<String>,
}

/// The exact limits used for this observation, in reportable scalar units.
#[derive(Clone, Debug, Serialize)]
pub struct PlanLimits {
    pub max_root_components: usize,
    pub max_root_path_bytes: usize,
    pub max_entries: usize,
    pub max_depth: usize,
    pub max_packs: usize,
    pub max_pack_bytes: u64,
    pub verify_io_bytes: u64,
    pub max_elapsed_ms: u128,
}

impl From<&Limits> for PlanLimits {
    fn from(limits: &Limits) -> Self {
        Self {
            max_root_components: MAX_ROOT_COMPONENTS,
            max_root_path_bytes: MAX_ROOT_PATH_BYTES,
            max_entries: limits.max_entries,
            max_depth: limits.max_depth,
            max_packs: limits.max_packs,
            max_pack_bytes: limits.max_pack_bytes,
            verify_io_bytes: limits.verify_io_bytes,
            max_elapsed_ms: limits.max_elapsed.as_millis(),
        }
    }
}

/// Observe the primary store, nested common/modules stores, and linked-worktree
/// module stores. Never traverses a checkout, invokes Git, or changes a file.
///
/// # Errors
/// Invalid/nonabsolute/aliased root, missing primary objects directory or HEAD,
/// and unavailable strict platform support refuse before planning. Later
/// traversal/verification failures produce an explicitly incomplete plan.
pub fn plan(git_common_dir: &Path, limits: &Limits) -> io::Result<Plan> {
    #[cfg(unix)]
    {
        unix::plan(git_common_dir, limits)
    }
    #[cfg(not(unix))]
    {
        let _ = (git_common_dir, limits);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "strict no-follow repository storage planning is unavailable on this platform",
        ))
    }
}
