// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WATCH-ONLY LOOP'S REMEDIES BESIDE A PROVEN SELF-CLAIM (the codex-alab
//! native harness contract, §4; the Codex screen lane's retirement).
//!
//! Beside a proven self-claim ([`Session::set_beside`]) the agent supervises
//! itself and this loop types, presses and badges nothing. Two of aterm's
//! remedies act from OUTSIDE the agent and stand: the memory ceiling's
//! restart (here) and the stall's explicit restart (`stall.rs`, on the stall
//! alone, as for every agent: a TUI that stopped reading its input mid-turn
//! keeps a record that says `busy` for ever).
//!
//! The memory restart ends the agent only at an IDLE POINT, read from no
//! screen: the agent's own session record, read live at decision time while
//! its identity is live ([`IdleHost::beside_idle`],
//! `crate::harness::self_claimed::read_idle`: `status: "idle"` with
//! `draftEmpty: true`). Anything else — busy, a draft, a record removed or
//! unreadable, a dead identity — is no idle point: the loop waits and asks
//! again at its next step (fail closed). Two eligible idle samples must
//! span at least [`super::WAIT_STEP`] on the monotonic clock and carry the
//! same process and current content sequence. Each sample re-reads that
//! sequence; an ineligible sample or changed holder starts over. This is a
//! conservative observation window, not an atomic idle guarantee: the
//! record has no bounded publication-lag contract, and the agent can start
//! work after the last read, before the server receives the restart.
//!
//! The act is the server's explicit restart, `restart pid=<leader>
//! quiet=<grace>` (the stall's adds `if-stalled`): it stamps the agent before
//! its `SIGTERM`, so the exit is the act's whoever reads it, and the host's
//! crash rule — the one decider of a self-claimed agent's exit — relaunches
//! it on the line aterm's copy of the record validated. Nothing here relaunches
//! or types anything. The server's own fences decide as for the stall's
//! remedy: a person's input within the grace, a hold, a driver's lease or turn
//! (`busy`, `halted`, `restart unavailable: halted|driver-lease|driver-turn`,
//! [`super::stall::refusal_lifts`]) refuse it UNCOUNTED, and the next step
//! asks again; one it can never carry (any other `restart unavailable`) is
//! counted, and the warning's row names the person's restart from then on.
//!
//! THE WORKER'S NEEDS YOU IS THE SELF-CLAIM'S OWN beside it (the contract's
//! §5 NEEDS YOU row, FOREVER CRUISE / FOREVER LAND): codex-alab raises and
//! takes down `meta set attention owner=needs` itself, so this loop never
//! writes or unsets that owner's entry — not at a point, not at the loop's
//! start (a standing entry is codex-alab's, never one a loop before this one
//! left), not at an Off park and not at the loop's end
//! ([`Session::needs_not_ours`]), whatever the claim's state at that moment
//! (an Off park gives it back, the loop's end releases it).

use super::stall::{refusal_lifts, reply_line};
use super::*;

impl<C: Ctl> Session<'_, C> {
    /// Whether this loop watches beside a proven self-claim
    /// ([`Self::set_beside`]).
    pub(super) fn beside_self_claim(&self) -> bool {
        self.setup.beside.is_some()
    }

    /// Whether the worker's NEEDS YOU entry (`owner=needs`) is another's to
    /// keep, so this loop neither writes nor unsets it (`escalate.rs`'s
    /// `sync_needs` and `needs_down`, `lifecycle.rs`'s `reconcile`): beside
    /// a proven self-claim, whose agent raises its own (module header), and
    /// behind another supervisor's claim, which answers for the session.
    pub(super) fn needs_not_ours(&self) -> bool {
        self.beside_self_claim() || self.claim.watching_behind().is_some()
    }

    /// THE MEMORY CEILING'S RESTART BESIDE A PROVEN SELF-CLAIM (module
    /// header), asked at each step of a wait with the step's measure `over`:
    /// a measure ACTED ON ([`super::super::memory::MemoryOver::acts`]) — the Mac in
    /// trouble, this agent the largest — with `[harness] relaunch` on and a
    /// host that relaunches the agent it ends, unattended, no stall in hand,
    /// and two eligible idle samples separated by the observation window,
    /// naming the same current process and screen `seq` (module header) —
    /// gets the explicit restart, once per agent process. A measure no
    /// longer acted on ends the episode's count. Never decides a point: the
    /// loop beside a self-claim decides none.
    pub(super) fn beside_memory_remedy(
        &mut self,
        over: Option<super::super::memory::MemoryOver>,
        seq: u64,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        // The door's own clock, read where the remedy reads it (after the
        // record and the screen): a clone, as the remedy holds `self`.
        let fx = self.fx.clone();
        self.beside_memory_remedy_at(over, seq, move || fx.mono(), opts, review)
    }

    /// The memory remedy with a supplied monotonic clock. Read it after the
    /// record and screen, so blocked reads cannot age the first sample.
    pub(super) fn beside_memory_remedy_at(
        &mut self,
        over: Option<super::super::memory::MemoryOver>,
        seq: u64,
        clock: impl FnOnce() -> Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        // Every early return, including a failed read, ends the candidate.
        let previous = self.beside_idle_at.take();
        let Some(over) = over.filter(super::super::memory::MemoryOver::acts) else {
            self.beside_memory_ended = None;
            self.beside_idle_at = None;
            return Ok(());
        };
        let Some(host) = self.stall_host.clone() else {
            return Ok(());
        };
        let Some(leader) = host.agent_pid() else {
            return Ok(());
        };
        if self
            .beside_memory_ended
            .is_some_and(|ended| ended == leader)
            || !review.unattended()
            || !opts.policy.relaunch
            || !host.relaunches_after_stall()
            || self.stalled.is_some()
        {
            return Ok(());
        }
        if !host.beside_idle() || self.screen()?.seq != seq {
            return Ok(());
        }
        let now = clock();
        let first = match previous {
            Some((pid, seen, first)) if pid == leader && seen == seq => first,
            _ => {
                self.beside_idle_at = Some((leader, seq, now));
                return Ok(());
            }
        };
        self.beside_idle_at = Some((leader, seq, first));
        // [`WAIT_STEP`] in every run but a test's (the setup carries it in
        // every build, so a tape a test recorded replays as it ran).
        let minimum = self.setup.beside_idle_min;
        if now.saturating_duration_since(first) < minimum {
            return Ok(());
        }
        let pid = format!("pid={leader}");
        let quiet = format!("quiet={}", self.stall_grace_s);
        let mut args = vec!["restart"];
        args.push(pid.as_str());
        args.push(quiet.as_str());
        let r = self.call(&args)?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("restart failed: {}", r.stderr.trim())));
        }
        let reply = reply_line(&r);
        if crate::supervise::report::without_verb(&r) {
            // An older server has no stamped restart: a bare signal's exit
            // would read as the agent's own, never relaunched. Nothing is
            // sent; the warning's row is the person's.
            self.beside_memory_ended = Some(leader);
            self.memory_refused = true;
            return Ok(());
        }
        if r.ok() || (r.is_err("restart unavailable") && !refusal_lifts(&r)) {
            // Made, or never to be made for this process: counted. Another
            // hand on the session (a hold, a driver's lease or turn) is not
            // that: uncounted, asked again at the next step.
            self.beside_memory_ended = Some(leader);
            self.memory_refused = !r.ok();
        }
        review
            .say(&format!(
                "{} seq={seq} memory footprint_mb={} ceiling_mb={} reply={reply}",
                if r.ok() {
                    "RESTARTED"
                } else {
                    "RESTART-REFUSED"
                },
                over.footprint_mib,
                over.ceiling_mib,
            ))
            .map_err(Fail::Hard)
    }
}
