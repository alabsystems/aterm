// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Containment policy — maps mode to allowed capabilities.
//!
//! The policy functions here match the TLA+ spec `tla/Containment.tla`
//! `PolicyNetwork`, `PolicyFs`, `PolicyProcess`, `PolicyMcp`,
//! `PolicyPlugins`, `PolicyOutput`, `PolicyInput`, and `PolicyCommand`
//! operators exactly.

use crate::capability::{
    CommandCapability, FsCapability, InputCapability, McpCapability, NetworkCapability,
    OutputCapability, PluginCapability, ProcessCapability,
};
use crate::mode::ContainmentMode;

/// Complete capability set for a containment mode.
///
/// This is the output of [`ContainmentPolicy::capabilities`] — all 8
/// subsystem capabilities resolved for a given mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Capabilities {
    /// Network access level.
    pub network: NetworkCapability,
    /// Filesystem access level.
    pub fs: FsCapability,
    /// Process creation level.
    pub process: ProcessCapability,
    /// MCP tool access level.
    pub mcp: McpCapability,
    /// Plugin access level.
    pub plugins: PluginCapability,
    /// Output handling level.
    pub output: OutputCapability,
    /// Input handling level.
    pub input: InputCapability,
    /// Command execution level (`CaMeL` tier cap).
    pub command: CommandCapability,
}

/// The containment policy engine.
///
/// Maps each [`ContainmentMode`] to allowed capabilities for each subsystem.
/// All policy functions are pure (no state) and `const`-evaluable.
///
/// The policy is defined once and is immutable — it encodes the security
/// contract between the launcher and aterm. The Rust implementation must
/// match the TLA+ specification.
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
            mcp: Self::mcp(mode),
            plugins: Self::plugins(mode),
            output: Self::output(mode),
            input: Self::input(mode),
            command: Self::command(mode),
        }
    }

    /// Network capability for mode.
    ///
    /// TLA+: `PolicyNetwork(m)`
    /// - Master → Full, User → Full, Safety → Allowlist, Containment → None
    #[inline(always)]
    #[must_use]
    pub const fn network(mode: ContainmentMode) -> NetworkCapability {
        match mode {
            ContainmentMode::Master | ContainmentMode::User => NetworkCapability::Full,
            ContainmentMode::Safety => NetworkCapability::Allowlist,
            ContainmentMode::Containment => NetworkCapability::None,
        }
    }

    /// Filesystem capability for mode.
    ///
    /// TLA+: `PolicyFs(m)`
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
    /// TLA+: `PolicyProcess(m)`
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

    /// MCP capability for mode.
    ///
    /// TLA+: `PolicyMcp(m)`
    /// - Master → Full, User → Full, Safety → Allowlist, Containment → Disabled
    #[inline(always)]
    #[must_use]
    pub(crate) const fn mcp(mode: ContainmentMode) -> McpCapability {
        match mode {
            ContainmentMode::Master | ContainmentMode::User => McpCapability::Full,
            ContainmentMode::Safety => McpCapability::Allowlist,
            ContainmentMode::Containment => McpCapability::Disabled,
        }
    }

    /// Plugin capability for mode.
    ///
    /// TLA+: `PolicyPlugins(m)`
    /// - Master → Full, User → Full, Safety → Allowlist, Containment → Disabled
    #[inline(always)]
    #[must_use]
    pub(crate) const fn plugins(mode: ContainmentMode) -> PluginCapability {
        match mode {
            ContainmentMode::Master | ContainmentMode::User => PluginCapability::Full,
            ContainmentMode::Safety => PluginCapability::Allowlist,
            ContainmentMode::Containment => PluginCapability::Disabled,
        }
    }

    /// Output capability for mode.
    ///
    /// TLA+: `PolicyOutput(m)`
    /// - Master → Unmodified, User → `ShadowScanned`, Safety → `ShadowScanned`,
    ///   Containment → Filtered
    #[inline(always)]
    #[must_use]
    pub(crate) const fn output(mode: ContainmentMode) -> OutputCapability {
        match mode {
            ContainmentMode::Master => OutputCapability::Unmodified,
            ContainmentMode::User | ContainmentMode::Safety => OutputCapability::ShadowScanned,
            ContainmentMode::Containment => OutputCapability::Filtered,
        }
    }

    /// Input capability for mode.
    ///
    /// TLA+: `PolicyInput(m)`
    /// - Master → Unmodified, User → Scanned, Safety → Scanned,
    ///   Containment → Filtered
    #[inline(always)]
    #[must_use]
    pub(crate) const fn input(mode: ContainmentMode) -> InputCapability {
        match mode {
            ContainmentMode::Master => InputCapability::Unmodified,
            ContainmentMode::User | ContainmentMode::Safety => InputCapability::Scanned,
            ContainmentMode::Containment => InputCapability::Filtered,
        }
    }

    /// Command execution capability for mode.
    ///
    /// TLA+: `PolicyCommand(m)`
    /// - Master → `AllTiers` (`CmdAll`, tier 4, `Critical`)
    /// - User → `UpToTier3` (`CmdTier3`, tier 3, `HighRisk`)
    /// - Safety → `UpToTier2` (`CmdTier2`, tier 2, `MediumRisk`)
    /// - Containment → `NoCommands` (`CmdNone`)
    #[inline(always)]
    #[must_use]
    pub const fn command(mode: ContainmentMode) -> CommandCapability {
        match mode {
            ContainmentMode::Master => CommandCapability::AllTiers,
            ContainmentMode::User => CommandCapability::UpToTier3,
            ContainmentMode::Safety => CommandCapability::UpToTier2,
            ContainmentMode::Containment => CommandCapability::NoCommands,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify all policy mappings match the TLA+ spec table exactly.
    ///
    /// TLA+ Containment.tla policy table:
    /// | Mode        | Net       | Fs        | Proc      | MCP       | Plug      | Out          | In        |
    /// |-------------|-----------|-----------|-----------|-----------|-----------|--------------|-----------|
    /// | Master(3)   | Full(2)   | Full(3)   | Full(2)   | Full(2)   | Full(2)   | Unmodified(2)| Unmod(2)  |
    /// | User(2)     | Full(2)   | HomeRW(2) | Full(2)   | Full(2)   | Full(2)   | Shadow(1)    | Scan(1)   |
    /// | Safety(1)   | Allow(1)  | ProjRW(1) | Restr(1)  | Allow(1)  | Allow(1)  | Shadow(1)    | Scan(1)   |
    /// | Contain(0)  | None(0)   | TmpOnly(0)| NoFork(0) | Disabled(0)| Disabled(0)| Filtered(0) | Filt(0)  |
    #[test]
    fn test_master_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::Master);
        assert_eq!(c.network, NetworkCapability::Full);
        assert_eq!(c.fs, FsCapability::Full);
        assert_eq!(c.process, ProcessCapability::Full);
        assert_eq!(c.mcp, McpCapability::Full);
        assert_eq!(c.plugins, PluginCapability::Full);
        assert_eq!(c.output, OutputCapability::Unmodified);
        assert_eq!(c.input, InputCapability::Unmodified);
        assert_eq!(c.command, CommandCapability::AllTiers);
    }

    #[test]
    fn test_user_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::User);
        assert_eq!(c.network, NetworkCapability::Full);
        assert_eq!(c.fs, FsCapability::HomeReadWrite);
        assert_eq!(c.process, ProcessCapability::Full);
        assert_eq!(c.mcp, McpCapability::Full);
        assert_eq!(c.plugins, PluginCapability::Full);
        assert_eq!(c.output, OutputCapability::ShadowScanned);
        assert_eq!(c.input, InputCapability::Scanned);
        assert_eq!(c.command, CommandCapability::UpToTier3);
    }

    #[test]
    fn test_safety_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::Safety);
        assert_eq!(c.network, NetworkCapability::Allowlist);
        assert_eq!(c.fs, FsCapability::ProjectReadWrite);
        assert_eq!(c.process, ProcessCapability::Restricted);
        assert_eq!(c.mcp, McpCapability::Allowlist);
        assert_eq!(c.plugins, PluginCapability::Allowlist);
        assert_eq!(c.output, OutputCapability::ShadowScanned);
        assert_eq!(c.input, InputCapability::Scanned);
        assert_eq!(c.command, CommandCapability::UpToTier2);
    }

    #[test]
    fn test_containment_policy() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::Containment);
        assert_eq!(c.network, NetworkCapability::None);
        assert_eq!(c.fs, FsCapability::TmpOnly);
        assert_eq!(c.process, ProcessCapability::NoFork);
        assert_eq!(c.mcp, McpCapability::Disabled);
        assert_eq!(c.plugins, PluginCapability::Disabled);
        assert_eq!(c.output, OutputCapability::Filtered);
        assert_eq!(c.input, InputCapability::Filtered);
        assert_eq!(c.command, CommandCapability::NoCommands);
    }

    /// TLA+ NonEscalation: mode can NEVER increase in capability.
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
                assert!(cl.mcp <= ch.mcp, "mcp: {lower} should <= {higher}");
                assert!(
                    cl.plugins <= ch.plugins,
                    "plugins: {lower} should <= {higher}"
                );
                assert!(cl.output <= ch.output, "output: {lower} should <= {higher}");
                assert!(cl.input <= ch.input, "input: {lower} should <= {higher}");
                assert!(
                    cl.command <= ch.command,
                    "command: {lower} should <= {higher}"
                );
            }
        }
    }

    /// TLA+ ContainmentMinimal: Containment mode is maximally restrictive.
    /// Every capability at its minimum value.
    #[test]
    fn test_containment_is_minimal() {
        let c = ContainmentPolicy::capabilities(ContainmentMode::Containment);
        assert_eq!(c.network as u8, 0, "ContainmentHasNoNetwork");
        assert_eq!(c.fs as u8, 0, "Containment fs = TmpOnly");
        assert_eq!(c.process as u8, 0, "Containment process = NoFork");
        assert_eq!(c.mcp as u8, 0, "ContainmentNoMcpNoPlugins (mcp)");
        assert_eq!(c.plugins as u8, 0, "ContainmentNoMcpNoPlugins (plugins)");
        assert_eq!(c.output as u8, 0, "ContainmentOutputFiltered");
        assert_eq!(c.input as u8, 0, "ContainmentInputFiltered");
        assert_eq!(c.command as u8, 0, "Containment command = NoCommands");
    }

    /// Exhaustive numeric cross-check against TLA+ Containment.tla policy table.
    ///
    /// Encodes the exact TLA+ numeric values as raw u8 constants and verifies
    /// each Rust policy function returns the matching capability. This catches
    /// any drift between the TLA+ spec and the Rust implementation.
    ///
    /// TLA+ encoding reference (Containment.tla lines 54-95):
    ///   Network: None=0, Allowlist=1, Full=2
    ///   Fs:      TmpOnly=0, ProjectRW=1, HomeRW=2, Full=3
    ///   Process: NoFork=0, Restricted=1, Full=2
    ///   MCP:     Disabled=0, Allowlist=1, Full=2
    ///   Plugins: Disabled=0, Allowlist=1, Full=2
    ///   Output:  Filtered=0, ShadowScan=1, Unmodified=2
    ///   Input:   Filtered=0, Scanned=1, Unmodified=2
    ///   Command: NoCommands=0, UpToTier2=1, UpToTier3=2, AllTiers=3
    #[test]
    fn test_tla_numeric_cross_check() {
        // TLA+ policy table: [mode_level] -> (net, fs, proc, mcp, plug, out, in, cmd)
        let tla_table: [(u8, [u8; 8]); 4] = [
            // Containment(0): Net=0, Fs=0, Proc=0, MCP=0, Plug=0, Out=0, In=0, Cmd=0
            (0, [0, 0, 0, 0, 0, 0, 0, 0]),
            // Safety(1):      Net=1, Fs=1, Proc=1, MCP=1, Plug=1, Out=1, In=1, Cmd=1
            (1, [1, 1, 1, 1, 1, 1, 1, 1]),
            // User(2):        Net=2, Fs=2, Proc=2, MCP=2, Plug=2, Out=1, In=1, Cmd=2
            (2, [2, 2, 2, 2, 2, 1, 1, 2]),
            // Master(3):      Net=2, Fs=3, Proc=2, MCP=2, Plug=2, Out=2, In=2, Cmd=3
            (3, [2, 3, 2, 2, 2, 2, 2, 3]),
        ];

        let modes = [
            ContainmentMode::Containment,
            ContainmentMode::Safety,
            ContainmentMode::User,
            ContainmentMode::Master,
        ];

        for (mode, &(expected_level, ref expected_caps)) in modes.iter().zip(tla_table.iter()) {
            assert_eq!(mode.level(), expected_level, "mode {mode} level mismatch");

            let caps = ContainmentPolicy::capabilities(*mode);
            assert_eq!(
                caps.network as u8, expected_caps[0],
                "TLA+ PolicyNetwork({mode}) = {}, Rust = {}",
                expected_caps[0], caps.network as u8
            );
            assert_eq!(
                caps.fs as u8, expected_caps[1],
                "TLA+ PolicyFs({mode}) = {}, Rust = {}",
                expected_caps[1], caps.fs as u8
            );
            assert_eq!(
                caps.process as u8, expected_caps[2],
                "TLA+ PolicyProcess({mode}) = {}, Rust = {}",
                expected_caps[2], caps.process as u8
            );
            assert_eq!(
                caps.mcp as u8, expected_caps[3],
                "TLA+ PolicyMcp({mode}) = {}, Rust = {}",
                expected_caps[3], caps.mcp as u8
            );
            assert_eq!(
                caps.plugins as u8, expected_caps[4],
                "TLA+ PolicyPlugins({mode}) = {}, Rust = {}",
                expected_caps[4], caps.plugins as u8
            );
            assert_eq!(
                caps.output as u8, expected_caps[5],
                "TLA+ PolicyOutput({mode}) = {}, Rust = {}",
                expected_caps[5], caps.output as u8
            );
            assert_eq!(
                caps.input as u8, expected_caps[6],
                "TLA+ PolicyInput({mode}) = {}, Rust = {}",
                expected_caps[6], caps.input as u8
            );
            assert_eq!(
                caps.command as u8, expected_caps[7],
                "TLA+ PolicyCommand({mode}) = {}, Rust = {}",
                expected_caps[7], caps.command as u8
            );
        }
    }

    /// Verify strict monotonicity: downgrading mode ALWAYS reduces
    /// or maintains every capability. No single capability may increase
    /// when the mode decreases.
    #[test]
    fn test_downgrade_never_increases_any_capability() {
        let modes = [
            ContainmentMode::Master,
            ContainmentMode::User,
            ContainmentMode::Safety,
            ContainmentMode::Containment,
        ];

        for i in 0..modes.len() {
            for j in (i + 1)..modes.len() {
                let higher = modes[i];
                let lower = modes[j];
                let ch = ContainmentPolicy::capabilities(higher);
                let cl = ContainmentPolicy::capabilities(lower);

                assert!(
                    cl.network <= ch.network
                        && cl.fs <= ch.fs
                        && cl.process <= ch.process
                        && cl.mcp <= ch.mcp
                        && cl.plugins <= ch.plugins
                        && cl.output <= ch.output
                        && cl.input <= ch.input
                        && cl.command <= ch.command,
                    "downgrade from {higher} to {lower} increased a capability"
                );
            }
        }
    }
}
