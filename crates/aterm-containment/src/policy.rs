// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Containment policy — maps mode to allowed capabilities.
//!
//! These functions and the tests below are the specification; the opt-in
//! `kani_proofs` harnesses re-check the same table.

use crate::capability::{FsCapability, NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;

/// Complete capability set for a containment mode.
///
/// This is the output of [`ContainmentPolicy::capabilities`] — the three
/// capability axes resolved for a given mode.
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

/// The containment policy engine.
///
/// Maps each [`ContainmentMode`] to allowed capabilities for each subsystem.
/// All policy functions are pure (no state) and `const`-evaluable.
///
/// The policy is defined once and is immutable — it encodes the security
/// contract between the launcher and aterm. This Rust implementation IS the
/// specification, pinned by the tests in this module and re-checked by the
/// opt-in, `#[cfg(kani)]`-gated `kani_proofs` harnesses.
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

    /// Network capability for mode.
    ///
    /// - Master, User, Safety → Full; Containment → None
    #[inline(always)]
    #[must_use]
    pub const fn network(mode: ContainmentMode) -> NetworkCapability {
        match mode {
            ContainmentMode::Master | ContainmentMode::User | ContainmentMode::Safety => {
                NetworkCapability::Full
            }
            ContainmentMode::Containment => NetworkCapability::None,
        }
    }

    /// Filesystem capability for mode.
    ///
    /// - Master → Full, User → `HomeRW`, Safety → `ProjectRW`, Containment → `TmpOnly`
    #[inline(always)]
    #[must_use]
    pub(crate) const fn fs(mode: ContainmentMode) -> FsCapability {
        match mode {
            ContainmentMode::Master => FsCapability::Full,
            ContainmentMode::User => FsCapability::HomeReadWrite,
            ContainmentMode::Safety => FsCapability::ProjectReadWrite,
            ContainmentMode::Containment => FsCapability::TmpOnly,
        }
    }

    /// Process capability for mode.
    ///
    /// - Master → Full, User → Full, Safety → Restricted, Containment → `NoFork`
    #[inline(always)]
    #[must_use]
    pub const fn process(mode: ContainmentMode) -> ProcessCapability {
        match mode {
            ContainmentMode::Master | ContainmentMode::User => ProcessCapability::Full,
            ContainmentMode::Safety => ProcessCapability::Restricted,
            ContainmentMode::Containment => ProcessCapability::NoFork,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify all policy mappings match the policy table exactly.
    ///
    /// | Mode        | Net       | Fs        | Proc      |
    /// |-------------|-----------|-----------|-----------|
    /// | Master(3)   | Full(1)   | Full(3)   | Full(2)   |
    /// | User(2)     | Full(1)   | HomeRW(2) | Full(2)   |
    /// | Safety(1)   | Full(1)   | ProjRW(1) | Restr(1)  |
    /// | Contain(0)  | None(0)   | TmpOnly(0)| NoFork(0) |
    #[test]
    fn test_master_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::Master);
        assert_eq!(c.network, NetworkCapability::Full);
        assert_eq!(c.fs, FsCapability::Full);
        assert_eq!(c.process, ProcessCapability::Full);
    }

    #[test]
    fn test_user_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::User);
        assert_eq!(c.network, NetworkCapability::Full);
        assert_eq!(c.fs, FsCapability::HomeReadWrite);
        assert_eq!(c.process, ProcessCapability::Full);
    }

    #[test]
    fn test_safety_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::Safety);
        // Safety narrows nothing on the network: no level promises filtering
        // that nothing does.
        assert_eq!(c.network, NetworkCapability::Full);
        assert_eq!(c.fs, FsCapability::ProjectReadWrite);
        assert_eq!(c.process, ProcessCapability::Restricted);
    }

    #[test]
    fn test_containment_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::Containment);
        assert_eq!(c.network, NetworkCapability::None);
        assert_eq!(c.fs, FsCapability::TmpOnly);
        assert_eq!(c.process, ProcessCapability::NoFork);
    }

    /// NonEscalation: mode can NEVER increase in capability.
    /// Verify that for all modes m1 < m2, every capability of m1 <= m2.
    #[test]
    fn test_monotonic_capabilities() {
        let modes = [
            ContainmentMode::Containment,
            ContainmentMode::Safety,
            ContainmentMode::User,
            ContainmentMode::Master,
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

    /// Exhaustive numeric cross-check against the policy table: encodes the
    /// intended discriminants as raw u8 constants and verifies each policy
    /// function returns the matching capability, catching any drift between the
    /// documented encoding and the implementation.
    ///
    ///   Network: None=0, Full=1
    ///   Fs:      TmpOnly=0, ProjectRW=1, HomeRW=2, Full=3
    ///   Process: NoFork=0, Restricted=1, Full=2
    #[test]
    fn test_numeric_policy_table() {
        // [mode_level] -> (net, fs, proc)
        let table: [(u8, [u8; 3]); 4] = [
            (0, [0, 0, 0]), // Containment
            (1, [1, 1, 1]), // Safety
            (2, [1, 2, 2]), // User
            (3, [1, 3, 2]), // Master
        ];

        let modes = [
            ContainmentMode::Containment,
            ContainmentMode::Safety,
            ContainmentMode::User,
            ContainmentMode::Master,
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
