// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Capability enums for the three axes.
//!
//! A higher discriminant means more access; the order is stated by the
//! `policy-monotone-minimal` spec (`SPECS.md`) and the discriminants are pinned
//! by `policy.rs`'s `test_numeric_policy_table`.
//!
//! Readers: `network` selects the Seatbelt profile
//! ([`crate::sbpl::profile_for`]); `fs` scopes its file rules and moves
//! `aterm-shell-integration`'s cache under `/tmp` for the confined modes;
//! `process` is checked by the spawn gate ([`crate::actuator::decide`]).

/// Network capability levels.
///
/// Discriminants: None=0, Full=1. There is no destination allowlist level
/// (owner, 2026-09-27):
/// Seatbelt filters only by port or localhost, not by host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum NetworkCapability {
    /// No network access. Containment mode — enforced by the macOS Seatbelt
    /// `(deny network*)`.
    None = 0,
    /// Unrestricted network. User and Safety mode.
    Full = 1,
}

/// Filesystem capability levels.
///
/// Discriminants: TmpOnly=0, ProjectRW=1, HomeRW=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum FsCapability {
    /// Containment mode. On macOS the Seatbelt profile confines writes to the
    /// temp roots and `/dev` (plus the shell's history files) and denies reads
    /// and writes of the credential and private-data stores; see
    /// [`crate::actuator`].
    TmpOnly = 0,
    /// Safety mode. Not enforced by any OS mechanism.
    ProjectReadWrite = 1,
    /// Read/write to home directory. User mode.
    HomeReadWrite = 2,
}

/// Process capability levels.
///
/// Discriminants: NoFork=0, Restricted=1, Full=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum ProcessCapability {
    /// No fork — exec only (for initial shell). Containment mode.
    NoFork = 0,
    /// Restricted process creation. Safety mode.
    Restricted = 1,
    /// Unrestricted process creation. User mode.
    Full = 2,
}
