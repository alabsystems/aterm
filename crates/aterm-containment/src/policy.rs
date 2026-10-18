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

/// The mode → capabilities mapping: pure `const` functions, specified by this module's tests
/// over every mode and every ordered pair of modes.
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
mod tests {
    use super::*;

    /// For every pair of modes, the lower never holds more of any capability.
    #[test]
    fn test_monotonic_capabilities() {
        let modes = [
            ContainmentMode::Containment,
            ContainmentMode::Safety,
            ContainmentMode::User,
        ];
        for (i, &lower) in modes.iter().enumerate() {
            for &higher in &modes[i..] {
                let cl = ContainmentPolicy::capabilities(lower);
                let ch = ContainmentPolicy::capabilities(higher);
                assert!(
                    cl.network <= ch.network,
                    "network: {lower} should <= {higher}"
                );
                assert!(cl.fs <= ch.fs, "fs: {lower} should <= {higher}");
                assert!(
                    cl.process <= ch.process,
                    "process: {lower} should <= {higher}"
                );
            }
        }
    }

    /// The policy table as raw discriminants, so a drift in the encoding or the mapping fails.
    /// Safety narrows nothing on the network: no level promises filtering that nothing does.
    #[test]
    fn test_numeric_policy_table() {
        // [mode_level] -> (net, fs, proc)
        let table: [(u8, [u8; 3]); 3] = [
            (0, [0, 0, 0]), // Containment
            (1, [1, 1, 1]), // Safety
            (2, [1, 2, 2]), // User
        ];

        let modes = [
            ContainmentMode::Containment,
            ContainmentMode::Safety,
            ContainmentMode::User,
        ];

        for (mode, &(expected_level, ref expected_caps)) in modes.iter().zip(table.iter()) {
            assert_eq!(mode.level(), expected_level, "mode {mode} level mismatch");

            let caps = ContainmentPolicy::capabilities(*mode);
            assert_eq!(caps.network as u8, expected_caps[0], "network({mode})");
            assert_eq!(caps.fs as u8, expected_caps[1], "fs({mode})");
            assert_eq!(caps.process as u8, expected_caps[2], "process({mode})");
        }
    }
}
