// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH HARNESS LANES ARE WRITTEN FOR AN AGENT: one [`Lanes`] per
//! [`Program`], whether the relaunch on exit, the live upgrade and the
//! restart in place exist for it. A lane is written for a program when its
//! code knows that program's launch line, its exit and its conversation; the
//! owner's `[harness]` switches (`relaunch`, `upgrade`), read live beside it,
//! say whether a written lane runs. A program the supervisor does not host
//! (`Program::Generic`, `Program::Muse`) has none, and neither has agy, which
//! it hosts: no code here knows agy's launch line, its exit or its
//! conversation.

use aterm_phase::Program;

/// The harness lanes written for one program ([`lanes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    /// THE RELAUNCH ON EXIT: an agent that crashed started again in its own
    /// tab on its own conversation ([`super::relaunch::on_exit`]; Claude
    /// Code's `--resume`, Codex's `resume`), under `[harness] relaunch`. It
    /// gates the stall's remedy too: a frozen agent is ended only where that
    /// exit is relaunched, so the remedy's lane is this one.
    pub relaunch: bool,
    /// THE LIVE UPGRADE: an idle agent moved onto a newer installed build of
    /// itself ([`super::upgrade_drive`]; Claude Code's restart, Codex's
    /// daemon first and then a typed `/exit` and `codex resume`, never a
    /// signal), under `[harness] upgrade`.
    pub upgrade: bool,
    /// THE RESTART IN PLACE a supervising loop asks for at an idle point
    /// ([`super::relaunch::restart_here`]: a model's fallback and its return,
    /// a memory restart, a move to another account): the agent ended there
    /// and relaunched on its conversation as the worker's own act, under
    /// `[harness] relaunch`.
    pub restart_in_place: bool,
}

/// The lanes written for `program`.
#[must_use]
pub fn lanes(program: Program) -> Lanes {
    match program {
        Program::Claude => Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: true,
        },
        Program::Codex => Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: false,
        },
        Program::Agy | Program::Muse | Program::Generic => Lanes {
            relaunch: false,
            upgrade: false,
            restart_in_place: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Lanes, lanes};
    use aterm_phase::Program;

    const NONE: Lanes = Lanes {
        relaunch: false,
        upgrade: false,
        restart_in_place: false,
    };

    /// A program the supervisor does not host has no lane: nothing would
    /// watch its exit, read its build or type its launch line.
    #[test]
    fn a_program_the_supervisor_does_not_host_has_no_lanes() {
        for p in [Program::Muse, Program::Generic] {
            assert!(!p.supervisable(), "{p:?}");
            assert_eq!(lanes(p), NONE, "{p:?}");
        }
    }

    /// agy is supervised with no lane: nothing knows its launch line, its
    /// exit or its conversation, so its exit is not relaunched, no upgrade
    /// moves it and no restart is asked of it.
    #[test]
    fn agy_is_supervised_with_no_lanes() {
        assert!(Program::Agy.supervisable());
        assert_eq!(lanes(Program::Agy), NONE);
        assert_ne!(lanes(Program::Codex), NONE, "the negative control");
    }

    /// The lanes the window's host runs for each agent it supervises:
    /// Claude Code all three; Codex the relaunch on exit and the live upgrade,
    /// its restart in place not written (the host's restart is Claude Code's
    /// alone).
    #[test]
    fn each_supervised_agent_has_the_lanes_the_host_runs() {
        assert!(Program::Claude.supervisable() && Program::Codex.supervisable());
        assert_eq!(
            lanes(Program::Claude),
            Lanes {
                relaunch: true,
                upgrade: true,
                restart_in_place: true,
            }
        );
        assert_eq!(
            lanes(Program::Codex),
            Lanes {
                relaunch: true,
                upgrade: true,
                restart_in_place: false,
            }
        );
    }
}
