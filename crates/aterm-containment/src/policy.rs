// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The containment policy: which capabilities each mode allows.

use crate::capability::{FsCapability, NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;

/// The three capability axes resolved for one mode ([`ContainmentPolicy::capabilities`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Capabilities {
    /// Network access level.
    pub network: NetworkCapability,
    /// Filesystem access level.
    pub fs: FsCapability,
    /// Process creation level.
    pub process: ProcessCapability,
}

/// Containment mode to capability mapping.
#[derive(Debug, Clone, Copy)]
pub struct ContainmentPolicy;

impl ContainmentPolicy {
    /// Resolve all capabilities for the given mode.
    #[inline(always)]
    #[must_use]
    pub const fn capabilities(mode: ContainmentMode) -> Capabilities {
        Capabilities {
            network: Self::network(mode),
            fs: Self::fs(mode),
            process: Self::process(mode),
        }
    }

    /// Network capability for `mode`.
    #[inline(always)]
    #[must_use]
    pub const fn network(mode: ContainmentMode) -> NetworkCapability {
        match mode {
            ContainmentMode::User | ContainmentMode::Safety => NetworkCapability::Full,
            ContainmentMode::Containment => NetworkCapability::None,
        }
    }

    /// Filesystem capability for `mode`.
    #[inline(always)]
    #[must_use]
    pub(crate) const fn fs(mode: ContainmentMode) -> FsCapability {
        match mode {
            ContainmentMode::User => FsCapability::HomeReadWrite,
            ContainmentMode::Safety => FsCapability::ProjectReadWrite,
            ContainmentMode::Containment => FsCapability::TmpOnly,
        }
    }

    /// Process capability for `mode`.
    #[inline(always)]
    #[must_use]
    pub const fn process(mode: ContainmentMode) -> ProcessCapability {
        match mode {
            ContainmentMode::User => ProcessCapability::Full,
            ContainmentMode::Safety => ProcessCapability::Restricted,
            ContainmentMode::Containment => ProcessCapability::NoFork,
        }
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
