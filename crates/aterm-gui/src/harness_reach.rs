// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session reach of the in-GUI supervisor host for need classification.

use crate::App;

/// The in-GUI supervisor host's reach over the sessions, for one refresh of
/// the need verdicts ([`App::harness_hosting`]).
#[derive(Default)]
pub(crate) struct HarnessHosting {
    /// The host supervises agent sessions at all now.
    pub(crate) supervising: bool,
    /// Agents admitted by their own live policy switches.
    pub(crate) agents: crate::harness_host::Admission,
    /// The adopted sessions it is holding off for their claim grace.
    pub(crate) held_off: Vec<String>,
    /// The sessions whose frozen agent it would relaunch
    /// (`HostHandle::relaunching`).
    pub(crate) relaunching: std::collections::HashSet<String>,
    /// The table's default Autopilot (`[harness] autopilot`), for a session
    /// with no word of its own.
    pub(crate) autopilot: aterm_agent::supervise::Autopilot,
}

impl HarnessHosting {
    /// Whether the host answers for `sid` (when it runs `agent`), whose own
    /// Autopilot word is `word`: only for an agent its live policy switch
    /// admits (`agents`), not while it holds the session off for its claim
    /// grace, and not while the session's Autopilot is OFF — its word, else
    /// the table's default (the host stops its worker there and starts none:
    /// `harness_host`'s `OffSessions`) — so the menu bar raises that session's
    /// boxes again, as for any session no host supervises (`held_off` the
    /// precedent).
    pub(crate) fn hosts(
        &self,
        sid: &str,
        word: Option<aterm_agent::supervise::Autopilot>,
        agent: aterm_phase::Program,
    ) -> bool {
        self.supervising
            && self.agents.admits(agent)
            && word.unwrap_or(self.autopilot) != aterm_agent::supervise::Autopilot::Off
            && !self.held_off.iter().any(|held| held == sid)
    }
}

impl App {
    /// Which sessions the in-GUI supervisor host answers for, read ONCE per
    /// refresh and before the registry lock is taken (the host's state lock
    /// is never nested under it): all of its agent sessions while it
    /// supervises, except those it is holding off for their adopted-claim
    /// grace after an update — nobody here answers
    /// their boxes yet, so the menu bar raises them as it would for a
    /// session no host supervises, until a claim (the host's, or the
    /// external supervisor's it waited for) is live.
    pub(crate) fn harness_hosting(&self) -> HarnessHosting {
        match self.harness.as_ref() {
            Some(host) if host.supervising() => HarnessHosting {
                supervising: true,
                agents: host.admission(),
                held_off: host.held_off(),
                relaunching: host.relaunching(),
                autopilot: self.harness_policy.get().policy.autopilot,
            },
            _ => HarnessHosting::default(),
        }
    }
}

#[cfg(test)]
#[path = "harness_reach_tests.rs"]
mod tests;
