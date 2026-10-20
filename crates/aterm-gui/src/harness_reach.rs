// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS HOST'S REACH (`docs/DESIGN-owner-surfaces-2026-10-16.md` §9 S2):
//! which sessions the in-GUI supervisor host answers for — the one fact the
//! need classifier ([`crate::needs`]) reads to tell a box a harness presses
//! from one nobody does. Read once per refresh, before the registry lock.

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
    /// grace after an update (round four, item 9) — nobody here answers
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
mod tests {
    use super::*;

    /// A SESSION HELD OFF FOR ITS CLAIM GRACE IS NOBODY'S YET (the round-four
    /// plan, item 9): while the supervisor host waits for an external
    /// supervisor to claim an adopted session again, it answers nothing there,
    /// so the menu-bar rows treat that session as unsupervised — its agent's
    /// own box is raised rather than left to a host that is not answering it.
    /// Every other session stays the host's; a host that supervises nothing
    /// hosts nothing.
    #[test]
    fn a_session_held_off_for_its_claim_grace_is_not_the_hosts() {
        let hosting = HarnessHosting {
            supervising: true,
            agents: crate::harness_host::Admission::of(&Default::default()),
            held_off: vec!["s-adopted".to_string()],
            relaunching: Default::default(),
            autopilot: aterm_agent::supervise::Autopilot::Cruise,
        };
        assert!(!hosting.hosts("s-adopted", None, aterm_phase::Program::Claude));
        assert!(hosting.hosts("s-other", None, aterm_phase::Program::Claude));
        assert!(!HarnessHosting::default().hosts("s-other", None, aterm_phase::Program::Claude));
    }

    /// AN OFF SESSION IS NOBODY'S (the Autopilot design, §2.2): the host stops
    /// its worker and starts none, so the menu-bar rows treat it as
    /// unsupervised and raise its agent's own box — its word `off`, or no word
    /// under a default of `off`. NEGATIVE CONTROLS: `cruise` and `land` are the
    /// host's, a word `cruise` opts one session in under a default of `off`,
    /// no word ever lifts a host that supervises nothing, and no word
    /// admits an agent its own switch holds off (`agents`).
    #[test]
    fn an_off_session_is_not_the_hosts() {
        use aterm_agent::supervise::Autopilot;
        use aterm_phase::Program;
        let codex_off = aterm_agent::supervise::SupervisorConfig {
            off: vec![Program::Codex],
            ..Default::default()
        };
        let mut hosting = HarnessHosting {
            supervising: true,
            agents: crate::harness_host::Admission::of(&codex_off),
            ..HarnessHosting::default()
        };
        assert!(hosting.hosts("s-a", None, Program::Claude));
        assert!(hosting.hosts("s-a", Some(Autopilot::Land), Program::Claude));
        assert!(!hosting.hosts("s-a", Some(Autopilot::Off), Program::Claude));
        hosting.autopilot = Autopilot::Off;
        assert!(!hosting.hosts("s-a", None, Program::Claude));
        assert!(hosting.hosts("s-a", Some(Autopilot::Cruise), Program::Claude));
        assert!(!HarnessHosting::default().hosts("s-a", Some(Autopilot::Cruise), Program::Claude));
        assert!(!hosting.hosts("s-a", Some(Autopilot::Cruise), Program::Codex));
    }
}
