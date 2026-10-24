// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supported harness lanes (relaunch on exit, live upgrade, restart in place) per [`Program`].
use aterm_phase::Program;

/// Harness lanes implemented for a [`Program`] ([`lanes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    /// Relaunch on exit in the same tab and conversation ([`super::relaunch::on_exit`]).
    pub relaunch: bool,
    /// Live upgrade of an idle agent onto a newer installed build ([`super::upgrade_drive`]).
    pub upgrade: bool,
    /// In-place restart at an idle point ([`super::relaunch::restart_here`]).
    pub restart_in_place: bool,
}

impl Lanes {
    /// Empty lane set (`Program::Generic`).
    pub const NONE: Lanes = Lanes {
        relaunch: false,
        upgrade: false,
        restart_in_place: false,
    };
}

/// Returns the [`Lanes`] supported by `program`.
#[must_use]
pub const fn lanes(program: Program) -> Lanes {
    match program {
        Program::Claude => Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: true,
        },
        Program::Codex => Lanes {
            relaunch: true,
            upgrade: false,
            restart_in_place: false,
        },
        Program::Muse | Program::Agy => Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: false,
        },
        Program::Gemini => Lanes {
            relaunch: true,
            upgrade: false,
            restart_in_place: false,
        },
        Program::Grok | Program::Kimi | Program::Generic => Lanes::NONE,
    }
}

/// Returns whether the background sweep carries an in-flight relaunch for `program`.
#[must_use]
pub const fn sweep_carries_relaunch(program: Program) -> bool {
    match program {
        Program::Claude | Program::Codex => true,
        Program::Muse
        | Program::Agy
        | Program::Gemini
        | Program::Grok
        | Program::Kimi
        | Program::Generic => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Lanes, lanes, sweep_carries_relaunch};
    use aterm_phase::Program;

    const NONE: Lanes = Lanes::NONE;

    /// Gemini CLI is relaunched on exit (and restored), never upgraded live
    /// nor restarted in place: it has no atpkg row to land a build.
    #[test]
    fn muse_code_agy_and_gemini_cli_are_relaunched_on_exit_and_only_muse_and_agy_upgraded() {
        for p in [Program::Muse, Program::Agy] {
            assert_eq!(
                lanes(p),
                Lanes {
                    relaunch: true,
                    upgrade: true,
                    restart_in_place: false,
                },
                "{p:?}"
            );
            assert_ne!(lanes(p), lanes(Program::Claude), "{p:?}");
            assert!(!sweep_carries_relaunch(p), "{p:?}: its host carries it");
        }
        let p = Program::Gemini;
        assert!(p.supervisable(), "{p:?}");
        assert_eq!(
            lanes(p),
            Lanes {
                relaunch: true,
                upgrade: false,
                restart_in_place: false,
            },
            "{p:?}"
        );
        assert_ne!(lanes(p), lanes(Program::Claude), "{p:?}");
        assert!(!sweep_carries_relaunch(p), "{p:?}: its host carries it");
        assert!(sweep_carries_relaunch(Program::Codex));
    }

    /// Every agent has a lane: no `upgrade::Agent` is left with all three
    /// false, and the one program with none is the one no reader hosts.
    #[test]
    fn every_agent_has_a_lane_and_only_generic_has_none() {
        use super::super::upgrade::Agent;
        let unlaned: Vec<Agent> = Agent::ALL
            .into_iter()
            .filter(|agent| lanes(agent.program()) == NONE)
            .collect();
        assert_eq!(unlaned, []);
        assert_eq!(lanes(Program::Generic), NONE, "the negative control");
    }
}
