// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Capability enums ([`crate::sbpl::profile_for`], [`crate::actuator::decide`]).

/// Network capability levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum NetworkCapability {
    /// No network access (containment mode).
    None = 0,
    /// Unrestricted network (user and safety mode).
    Full = 1,
}

/// Filesystem capability levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum FsCapability {
    /// Containment mode ([`crate::actuator`]).
    TmpOnly = 0,
    /// Safety mode.
    ProjectReadWrite = 1,
    /// Read/write to home directory (user mode).
    HomeReadWrite = 2,
}

/// Process capability levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum ProcessCapability {
    /// No fork — exec only for initial shell (containment mode).
    NoFork = 0,
    /// Restricted process creation (safety mode).
    Restricted = 1,
    /// Unrestricted process creation (user mode).
    Full = 2,
}
