// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH HARNESS LANES ARE WRITTEN FOR AN AGENT: one [`Lanes`] per
//! [`Program`], whether the relaunch on exit, the live upgrade and the
//! restart in place exist for it. A lane is written for a program when its
//! code knows that program's launch line, its exit and its conversation; the
//! owner's `[harness]` switches (`relaunch`, `upgrade`), read live beside it,
//! say whether a written lane runs. A program the supervisor does not host
//! (`Program::Generic`) has none, and neither have agy and Gemini CLI, which
//! it hosts: no code here knows agy's launch line, its exit or its
//! conversation; Gemini CLI runs as `node` and relaunches itself as a parent
//! and a child, and its conversation is named only on its farewell (`gemini
//! --resume <id>`), after it has left. Both are `upgrade::Agent`s already, so
//! a record or a restore leaf naming one is read as that agent, and this
//! table is what keeps every lane off it (`upgrade_drive::lane_on`,
//! `St::from_json`, the host's `relaunches_restored_of`).
//!
//! MUSE CODE HAS THE RELAUNCH ON EXIT (and so the restore after aterm's own
//! end), measured on 1.4.10-R6427.1 ([`super::upgrade_muse`]): its launch
//! line, its exit table and the session its process holds are known. It has
//! NO LIVE UPGRADE yet: what a running Muse Code does when atpkg lands a newer
//! build under it, its graceful exit's notice and READY exchange, and a
//! revision-aware build compare are unmeasured (the lanes research's X2 and
//! X3), so nothing moves it onto a newer build while it runs, and atpkg's
//! `live_upgrade` for it stays off. Nor a restart in place, which is Claude
//! Code's alone.

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
    /// itself ([`super::upgrade_drive`]; Claude Code's restart), under
    /// `[harness] upgrade`. Codex has none: a vendor Codex is shown, never
    /// moved, and a codex-alab upgrades itself (its contract, §5).
    pub upgrade: bool,
    /// THE RESTART IN PLACE a supervising loop asks for at an idle point
    /// ([`super::relaunch::restart_here`]: a model's fallback and its return,
    /// a memory restart, a move to another account): the agent ended there
    /// and relaunched on its conversation as the worker's own act, under
    /// `[harness] relaunch`.
    pub restart_in_place: bool,
}

impl Lanes {
    /// No lane written: nothing relaunches, upgrades or restarts the program.
    pub const NONE: Lanes = Lanes {
        relaunch: false,
        upgrade: false,
        restart_in_place: false,
    };
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
            upgrade: false,
            restart_in_place: false,
        },
        Program::Muse => Lanes {
            relaunch: true,
            upgrade: false,
            restart_in_place: false,
        },
        Program::Agy | Program::Gemini | Program::Generic => Lanes::NONE,
    }
}

/// WHO CARRIES `program`'S RELAUNCH ON EXIT LEFT IN FLIGHT (a record
/// mid-relaunch, owed its relaunch or its continuation): the SWEEP's pass
/// (`true`), or the WINDOW'S HOST at the agent's idle point (`false`). One
/// answer the sweep (`upgrade_drive::lane_on`) and the window's host (its idle
/// step) both read, so a relaunch-only lane's record has one carrier: the
/// sweep's pass, or the idle step of its tab's worker of that same agent —
/// the host's worker of a relaunch-only lane carries its own agent's record
/// alone (its `carries_only`, `relaunch::owed_of`), never a codex-alab's
/// left owed in a Muse Code's tab.
///
/// codex-alab's crash relaunch is the sweep's (`codex_alab_drive::sweep`):
/// its window's host reads its snapshot at the attach and steps nothing at
/// its idle points. Claude Code's lane is taken by both for its live upgrade,
/// whatever this answers, and its worker's idle step carries whatever record
/// its tab owes, as before Muse Code had a lane. A Muse Code's relaunch in
/// flight is its window's host's (`relaunch::carry_in_flight`,
/// `upgrade_muse_drive::carry_in_flight`), never a sweep's: its records are no
/// upgrade's in flight. No lane (agy, Gemini CLI, a program the supervisor
/// does not host): nothing carried, and [`lanes`] keeps both off it.
#[must_use]
pub fn sweep_carries_relaunch(program: Program) -> bool {
    match program {
        Program::Claude | Program::Codex => true,
        Program::Muse | Program::Agy | Program::Gemini | Program::Generic => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Lanes, lanes};
    use aterm_phase::Program;

    const NONE: Lanes = Lanes::NONE;

    /// agy and Gemini CLI are supervised with no lane: nothing knows agy's
    /// launch line, its exit or its conversation, nor Gemini CLI's
    /// conversation before it has left, so an exit is not relaunched, no
    /// upgrade moves them and no restart is asked of them.
    #[test]
    fn agy_and_gemini_cli_are_supervised_with_no_lanes() {
        for p in [Program::Agy, Program::Gemini] {
            assert!(p.supervisable(), "{p:?}");
            assert_eq!(lanes(p), NONE, "{p:?}");
        }
        assert_ne!(lanes(Program::Codex), NONE, "the negative control");
    }

    /// Muse Code is relaunched on exit (and restored), never upgraded live
    /// nor restarted in place: its upgrade is unmeasured. NEGATIVE CONTROL:
    /// Claude Code's lanes, the upgrade and the restart in place among them,
    /// are not Muse Code's.
    #[test]
    fn muse_code_is_relaunched_on_exit_and_never_upgraded() {
        assert!(Program::Muse.supervisable());
        assert_eq!(
            lanes(Program::Muse),
            Lanes {
                relaunch: true,
                upgrade: false,
                restart_in_place: false,
            }
        );
        assert_ne!(lanes(Program::Muse), lanes(Program::Claude));
    }

    /// The same two as `upgrade::Agent`s (`agy`, `gemini`): agents with no
    /// lane yet, all three lanes false, while every agent with a lane is one
    /// of Claude Code, Codex and Muse Code.
    #[test]
    fn the_agents_with_no_lane_yet_are_agy_and_gemini_cli() {
        use super::super::upgrade::Agent;
        let unlaned: Vec<Agent> = Agent::ALL
            .into_iter()
            .filter(|agent| lanes(agent.program()) == NONE)
            .collect();
        assert_eq!(unlaned, [Agent::Agy, Agent::Gemini]);
    }
}
