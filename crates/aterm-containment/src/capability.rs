// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Capability enums for the three axes something reads.
//!
//! Each axis has a capability level that maps to a containment mode; a higher
//! discriminant means more access. The discriminants are pinned by the tests
//! below and relied on by the `kani_proofs` harnesses.
//!
//! Who reads each axis: `network` selects the Seatbelt profile
//! ([`crate::sbpl::profile_for`]); `fs` scopes its file rules and moves
//! `aterm-shell-integration`'s cache under `/tmp` for the confined modes;
//! `process` is checked by the spawn gate ([`crate::actuator::decide`]). The MCP,
//! plugin, output, input and command axes that used to sit here had no reader
//! anywhere and were deleted, 2026-09-25.

/// Network capability levels.
///
/// Discriminants: None=0, Allowlist=1, Full=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum NetworkCapability {
    /// No network access. Containment mode — enforced by the macOS Seatbelt
    /// `(deny network*)`.
    None = 0,
    /// Safety mode. Nothing narrows the network for it: Seatbelt can filter only
    /// by port or localhost, not by host, so a destination allowlist would need a
    /// proxy nobody is building. At runtime this behaves exactly like `Full`.
    Allowlist = 1,
    /// Unrestricted network. Master/User mode.
    Full = 2,
}

/// Filesystem capability levels.
///
/// Discriminants: TmpOnly=0, ProjectRW=1, HomeRW=2, Full=3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum FsCapability {
    /// Containment mode. On macOS the Seatbelt profile confines WRITES to the
    /// temp roots and `/dev` (plus the shell's own history files) and denies
    /// reads and writes of the credential and private-data stores; see
    /// [`crate::actuator`].
    TmpOnly = 0,
    /// Safety mode. Not enforced by any OS mechanism.
    ProjectReadWrite = 1,
    /// Read/write to home directory. User mode.
    HomeReadWrite = 2,
    /// Full filesystem access. Master mode.
    Full = 3,
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
    /// Unrestricted process creation. Master/User mode.
    Full = 2,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_ordering() {
        assert!(NetworkCapability::Full > NetworkCapability::Allowlist);
        assert!(NetworkCapability::Allowlist > NetworkCapability::None);
    }

    #[test]
    fn test_fs_ordering() {
        assert!(FsCapability::Full > FsCapability::HomeReadWrite);
        assert!(FsCapability::HomeReadWrite > FsCapability::ProjectReadWrite);
        assert!(FsCapability::ProjectReadWrite > FsCapability::TmpOnly);
    }

    #[test]
    fn test_repr_encoding() {
        assert_eq!(NetworkCapability::None as u8, 0);
        assert_eq!(NetworkCapability::Allowlist as u8, 1);
        assert_eq!(NetworkCapability::Full as u8, 2);

        assert_eq!(FsCapability::TmpOnly as u8, 0);
        assert_eq!(FsCapability::ProjectReadWrite as u8, 1);
        assert_eq!(FsCapability::HomeReadWrite as u8, 2);
        assert_eq!(FsCapability::Full as u8, 3);

        assert_eq!(ProcessCapability::NoFork as u8, 0);
        assert_eq!(ProcessCapability::Restricted as u8, 1);
        assert_eq!(ProcessCapability::Full as u8, 2);
    }

    #[test]
    fn test_process_ordering() {
        assert!(ProcessCapability::Full > ProcessCapability::Restricted);
        assert!(ProcessCapability::Restricted > ProcessCapability::NoFork);
    }
}
