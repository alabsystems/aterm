// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Capability enums for each subsystem.
//!
//! Each subsystem has a capability level that maps to a containment mode.
//! Higher numeric value = more access. These match the TLA+ spec
//! `tla/Containment.tla` capability encodings exactly.

/// Network capability levels.
///
/// TLA+ encoding: None=0, Allowlist=1, Full=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum NetworkCapability {
    /// No network access. Containment mode.
    None = 0,
    /// Allowlisted destinations only. Safety mode.
    Allowlist = 1,
    /// Unrestricted network. Master/User mode.
    Full = 2,
}

/// Filesystem capability levels.
///
/// TLA+ encoding: TmpOnly=0, ProjectRW=1, HomeRW=2, Full=3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum FsCapability {
    /// Read/write only to `/tmp`. Containment mode.
    TmpOnly = 0,
    /// Read/write to project directory + tmp. Safety mode.
    ProjectReadWrite = 1,
    /// Read/write to home directory. User mode.
    HomeReadWrite = 2,
    /// Full filesystem access. Master mode.
    Full = 3,
}

/// Process capability levels.
///
/// TLA+ encoding: NoFork=0, Restricted=1, Full=2.
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

/// MCP (Model Context Protocol) capability levels.
///
/// TLA+ encoding: Disabled=0, Allowlist=1, Full=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum McpCapability {
    /// MCP disabled entirely. Containment mode.
    Disabled = 0,
    /// Allowlisted MCP tools only. Safety mode.
    Allowlist = 1,
    /// All MCP tools available. Master/User mode.
    Full = 2,
}

/// Plugin capability levels.
///
/// TLA+ encoding: Disabled=0, Allowlist=1, Full=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum PluginCapability {
    /// Plugins disabled entirely. Containment mode.
    Disabled = 0,
    /// Allowlisted plugins only. Safety mode.
    Allowlist = 1,
    /// All plugins available. Master/User mode.
    Full = 2,
}

/// Output handling capability levels.
///
/// TLA+ encoding: Filtered=0, ShadowScanned=1, Unmodified=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum OutputCapability {
    /// All output filtered through LLM before display. Containment mode.
    Filtered = 0,
    /// Output displayed unmodified but shadow-scanned in parallel.
    /// User/Safety mode.
    ShadowScanned = 1,
    /// Output passed through unmodified, no scanning. Master mode.
    Unmodified = 2,
}

/// Input handling capability levels.
///
/// TLA+ encoding: Filtered=0, Scanned=1, Unmodified=2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum InputCapability {
    /// All input filtered before reaching agent. Containment mode.
    Filtered = 0,
    /// Input scanned for injection patterns. User/Safety mode.
    Scanned = 1,
    /// Input passed through unmodified. Master mode.
    Unmodified = 2,
}

/// Command execution capability levels.
///
/// Maps containment modes to a maximum command-execution capability.
/// [`CommandCapability::max_tier_level`] derives the corresponding
/// `aterm-security` `CommandTier` ceiling when commands are allowed.
/// TLA+ encoding: `CmdNone`=0, `CmdTier2`=1, `CmdTier3`=2, `CmdAll`=3.
/// This mirrors `CommandCaps` and `PolicyCommand` in `tla/Containment.tla`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum CommandCapability {
    /// No command execution allowed. Containment mode.
    NoCommands = 0,
    /// Commands up to tier 2 (`MediumRisk`). Safety mode.
    UpToTier2 = 1,
    /// Commands up to tier 3 (`HighRisk`). User mode.
    UpToTier3 = 2,
    /// All tiers including Critical (tier 4). Master mode.
    AllTiers = 3,
}

impl CommandCapability {
    /// Returns the maximum allowed command tier level, or `None` if no commands allowed.
    ///
    /// This is a derived Rust helper over the `PolicyCommand` result, not a
    /// separate policy axis.
    ///
    /// Maps to `CommandTier::level()` in aterm-security:
    /// - `AllTiers` → `Some(4)` (Critical)
    /// - `UpToTier3` → `Some(3)` (`HighRisk`)
    /// - `UpToTier2` → `Some(2)` (`MediumRisk`)
    /// - `NoCommands` → `None`
    #[inline(always)]
    #[must_use]
    pub const fn max_tier_level(self) -> Option<u8> {
        match self {
            Self::AllTiers => Some(4),
            Self::UpToTier3 => Some(3),
            Self::UpToTier2 => Some(2),
            Self::NoCommands => None,
        }
    }
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
    fn test_repr_matches_tla() {
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

        assert_eq!(McpCapability::Disabled as u8, 0);
        assert_eq!(McpCapability::Allowlist as u8, 1);
        assert_eq!(McpCapability::Full as u8, 2);

        assert_eq!(PluginCapability::Disabled as u8, 0);
        assert_eq!(PluginCapability::Allowlist as u8, 1);
        assert_eq!(PluginCapability::Full as u8, 2);

        assert_eq!(OutputCapability::Filtered as u8, 0);
        assert_eq!(OutputCapability::ShadowScanned as u8, 1);
        assert_eq!(OutputCapability::Unmodified as u8, 2);

        assert_eq!(InputCapability::Filtered as u8, 0);
        assert_eq!(InputCapability::Scanned as u8, 1);
        assert_eq!(InputCapability::Unmodified as u8, 2);

        assert_eq!(CommandCapability::NoCommands as u8, 0);
        assert_eq!(CommandCapability::UpToTier2 as u8, 1);
        assert_eq!(CommandCapability::UpToTier3 as u8, 2);
        assert_eq!(CommandCapability::AllTiers as u8, 3);
    }

    #[test]
    fn test_command_ordering() {
        assert!(CommandCapability::AllTiers > CommandCapability::UpToTier3);
        assert!(CommandCapability::UpToTier3 > CommandCapability::UpToTier2);
        assert!(CommandCapability::UpToTier2 > CommandCapability::NoCommands);
    }

    #[test]
    fn test_command_max_tier_level() {
        assert_eq!(CommandCapability::AllTiers.max_tier_level(), Some(4));
        assert_eq!(CommandCapability::UpToTier3.max_tier_level(), Some(3));
        assert_eq!(CommandCapability::UpToTier2.max_tier_level(), Some(2));
        assert_eq!(CommandCapability::NoCommands.max_tier_level(), None);
    }
}
