// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Spawn-seam actuator turning [`Capabilities`](crate::Capabilities) into a [`SpawnDecision`].

use crate::audit::{log_denial, log_posture};
use crate::capability::{NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;
use crate::policy::ContainmentPolicy;

const SUBSYSTEM: &str = "spawn";

/// Refusal reason returned by [`decide`] for `Containment` on platforms without an OS sandbox.
pub const NO_OS_SANDBOX_REASON: &str = "the sandbox exists only on macOS, so no shell was started";

/// Whether this platform can actuate a real OS sandbox at the spawn seam (`true` on macOS).
#[must_use]
pub const fn os_sandbox_actuated() -> bool {
    cfg!(target_os = "macos")
}

/// Whether the OS network sandbox is active for a spawn in `mode`.
#[must_use]
pub fn network_sandbox_actuated(mode: ContainmentMode) -> bool {
    os_sandbox_actuated() && ContainmentPolicy::network(mode) == NetworkCapability::None
}

/// Actuated decision at the PTY spawn seam for a [`ContainmentMode`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnDecision {
    /// Spawning the initial shell is permitted.
    Permit {
        /// Mode this decision was made for.
        mode: ContainmentMode,
        /// Whether a real OS sandbox backs this permit ([`network_sandbox_actuated`]).
        os_sandbox: bool,
        /// SBPL profile applied via `sandbox-exec` (`Some` iff `os_sandbox`).
        sbpl: Option<String>,
        /// Whether `sbpl` includes `$HOME`-scoped shell-history and credential rules.
        home_scoped: bool,
    },
    /// Spawning is denied for this mode.
    Deny {
        /// Mode this decision was made for.
        mode: ContainmentMode,
        /// Refusal reason printed by the launcher.
        reason: &'static str,
    },
}

impl SpawnDecision {
    /// Whether this decision permits spawning.
    #[must_use]
    pub const fn is_permitted(&self) -> bool {
        matches!(self, SpawnDecision::Permit { .. })
    }
}

/// Evaluates and audits whether the PTY spawn seam may run for `mode`.
#[must_use]
pub fn decide(mode: ContainmentMode) -> SpawnDecision {
    decide_on(
        mode,
        os_sandbox_actuated(),
        crate::sbpl::home_dir().as_deref(),
    )
}

/// [`decide`] with the platform's sandbox capability and scoped `$HOME`.
fn decide_on(mode: ContainmentMode, sandbox_available: bool, home: Option<&str>) -> SpawnDecision {
    let process_cap = ContainmentPolicy::process(mode);
    let denies_network = ContainmentPolicy::network(mode) == NetworkCapability::None;
    if denies_network && !sandbox_available {
        log_denial(SUBSYSTEM, "spawn initial shell", mode, NO_OS_SANDBOX_REASON);
        return SpawnDecision::Deny {
            mode,
            reason: NO_OS_SANDBOX_REASON,
        };
    }
    let os_sandbox = denies_network && sandbox_available;
    let scoped: Option<(String, bool)> = if os_sandbox {
        crate::sbpl::profile_for_scoped(ContainmentPolicy::capabilities(mode), home)
    } else {
        None
    };
    let home_scoped = scoped.as_ref().is_some_and(|(_, home)| *home);
    let sbpl: Option<String> = scoped.map(|(profile, _)| profile);
    debug_assert_eq!(
        sbpl.is_some(),
        os_sandbox,
        "sbpl must be Some iff os_sandbox"
    );

    if os_sandbox {
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            applied_posture(home_scoped),
        );
    } else {
        log_posture(
            SUBSYSTEM,
            "os-network-sandbox",
            mode,
            if mode == ContainmentMode::User {
                "OS sandbox not applied (network permitted by policy); process-cap gate only (resource limits inherited from aterm's own process)"
            } else {
                "OS sandbox not applied (network permitted by policy); resource limits + process-cap gate only"
            },
        );
    }
    match process_cap {
        ProcessCapability::Full | ProcessCapability::Restricted | ProcessCapability::NoFork => {
            SpawnDecision::Permit {
                mode,
                os_sandbox,
                sbpl,
                home_scoped,
            }
        }
    }
}

/// Summary string describing the enforced sandbox posture for `home_scoped`.
#[must_use]
pub const fn applied_posture(home_scoped: bool) -> &'static str {
    if home_scoped {
        "OS sandbox applied: no network; no debugger attach (no task port but a process's own); no mach service lookup but user and group queries (launchd answers by its own policy); writes only to temp dirs and shell history; no access to credential stores or private data under $HOME"
    } else {
        "OS sandbox applied: no network; no debugger attach (no task port but a process's own); no mach service lookup but user and group queries (launchd answers by its own policy); writes only to temp dirs; $HOME did not resolve, so credential stores and private data can still be read"
    }
}

#[cfg(test)]
#[path = "actuator_tests.rs"]
mod tests;
