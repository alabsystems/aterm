// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH HARNESS LANES ARE WRITTEN FOR AN AGENT: one [`Lanes`] per
//! [`Program`], whether the relaunch on exit, the live upgrade and the
//! restart in place exist for it. A lane is written for a program when its
//! code knows that program's launch line, its exit and its conversation; the
//! owner's `[harness]` switches (`relaunch`, `upgrade`), read live beside it,
//! say whether a written lane runs. A program the supervisor does not host
//! (`Program::Generic`) has none.
//!
//! MUSE CODE HAS THE RELAUNCH ON EXIT (and so the restore after aterm's own
//! end), measured on 1.4.10-R6427.1 ([`super::upgrade_muse`]): its launch
//! line, its exit table and the session its process holds are known. And,
//! since 2026-10-18, THE LIVE UPGRADE'S QUIET FLOW
//! ([`super::upgrade_muse_live`]): when atpkg lands a newer build (its
//! `live_upgrade` row wakes the window's host), a supervised Muse at a
//! genuinely idle point — its session's log closed, nothing running in it or
//! under it, its screen idle and empty — is ended (SIGTERM, claimed as the
//! upgrade's own exit) and relaunched through the managed twin on the same
//! session, and nothing is typed after. Never the ask flow (its notice and
//! READY are Claude Code's words, and Muse's screen half of a READY was
//! measured false), whatever `[harness] upgrade_ask` says. Nor a restart in
//! place, which is Claude Code's alone.
//!
//! AGY HAS THE RELAUNCH ON EXIT TOO (and the restore), measured on 1.4.0
//! ([`super::upgrade_agy`]): its launch line and Go flags, its exit table
//! with its crash file, and the conversation whose presence lock its process
//! holds are known. And, since 2026-10-18, THE LIVE UPGRADE'S QUIET FLOW
//! ([`super::upgrade_agy_live`]): when atpkg lands a newer build (its
//! `live_upgrade` row wakes the window's host for its agy sessions alone), a
//! supervised agy at a genuinely idle point — its reader idle over an empty
//! composer, its summary row idle, no background task, timer, cron or
//! subagent open in its transcript or shown on its screen, no wake pending,
//! no process under it — is ended (SIGTERM, claimed as the upgrade's own
//! exit) and relaunched through the managed twin on the same conversation,
//! and nothing is typed after. Never the ask flow (its transcript has no
//! model id and no process identity: measured), whatever `[harness]
//! upgrade_ask` says; nor a restart in place.
//!
//! GEMINI CLI HAS THE RELAUNCH ON EXIT TOO (and the restore), measured on
//! 0.46.0 ([`super::upgrade_gemini`]): it runs as `node`, a parent and a
//! child, and no file names its process's session, so the session is named
//! by a START-TIME RULE (the one session header of its project begun since
//! it started) and nothing is resumed where that rule names none; its exit
//! table, its refusals and a read-only check that its own resume would load
//! every turn are known. NO LIVE UPGRADE: it is a Homebrew or npm install
//! with no atpkg row, so nothing lands a build under it to move it onto (and
//! no wake), and its graceful exit's notice and READY exchange are
//! unmeasured; nor a restart in place, nor the explicit restart (its parent
//! ignores SIGTERM).

use aterm_phase::Program;

/// The harness lanes written for one program ([`lanes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    /// Relaunch on exit: a crashed agent starts again in its own tab on its
    /// own conversation ([`super::relaunch::on_exit`]), under
    /// `[harness] relaunch`. The stall remedy uses this lane too: a frozen
    /// agent is ended only where its exit is relaunched.
    pub relaunch: bool,
    /// THE LIVE UPGRADE: an idle agent moved onto a newer installed build of
    /// itself ([`super::upgrade_drive`]; Claude Code's restart, and Muse
    /// Code's and agy's quiet restarts, [`super::upgrade_muse_live`],
    /// [`super::upgrade_agy_live`]), under `[harness]
    /// upgrade`. Codex has none: a vendor Codex is shown, never moved, and a
    /// codex-alab upgrades itself (its contract, §5).
    pub upgrade: bool,
    /// Restart in place: at an idle point a supervising loop asks for
    /// ([`super::relaunch::restart_here`]: a model fallback and its return, a
    /// memory restart, an account move), the agent ends and relaunches on its
    /// own conversation, under `[harness] relaunch`.
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
        Program::Generic => Lanes::NONE,
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
/// its tab owes, as before Muse Code had a lane. A Muse Code's, an agy's and
/// a Gemini CLI's relaunch in flight is its window's host's
/// (`relaunch::carry_in_flight`, `upgrade_muse_drive::carry_in_flight`,
/// `upgrade_agy_drive::carry_in_flight`,
/// `upgrade_gemini_drive::carry_in_flight`), never a sweep's: their records
/// are no upgrade's in flight. No lane (a program the supervisor does not
/// host): nothing carried, and [`lanes`] keeps both off it.
#[must_use]
pub fn sweep_carries_relaunch(program: Program) -> bool {
    match program {
        Program::Claude | Program::Codex => true,
        Program::Muse | Program::Agy | Program::Gemini | Program::Generic => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Lanes, lanes, sweep_carries_relaunch};
    use aterm_phase::Program;

    const NONE: Lanes = Lanes::NONE;

    /// Gemini CLI is relaunched on exit (and restored), never upgraded live
    /// nor restarted in place: it has no atpkg row to land a build. Muse Code
    /// and agy are relaunched on exit and upgraded live (their quiet flows),
    /// never restarted in place. None's relaunch in flight is the sweep's:
    /// each is its window's host's. NEGATIVE CONTROLS: Claude Code's lanes,
    /// the restart in place among them, are none of theirs, and codex-alab's
    /// relaunch is the sweep's.
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
