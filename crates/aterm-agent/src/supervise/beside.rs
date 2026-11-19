// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WATCH-ONLY LOOP'S REMEDIES BESIDE A PROVEN SELF-CLAIM.

use super::stall::{refusal_lifts, reply_line};
use super::*;

impl<C: Ctl> Session<'_, C> {
    /// Whether this loop watches beside a proven self-claim ([`Self::set_beside`]).
    pub(super) const fn beside_self_claim(&self) -> bool {
        self.setup.beside.is_some()
    }

    /// Whether the worker's NEEDS YOU entry (`owner=needs`) is another's to keep.
    pub(super) fn needs_not_ours(&self) -> bool {
        self.beside_self_claim() || self.claim.watching_behind().is_some()
    }

    /// THE MEMORY CEILING'S RESTART BESIDE A PROVEN SELF-CLAIM: the server's `restart`, only
    /// once two reads `beside_idle_min` apart, on one unchanged screen, find the agent at its own
    /// idle point.
    pub(super) fn beside_memory_remedy(
        &mut self,
        over: Option<super::super::memory::MemoryOver>,
        seq: u64,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let fx = self.fx.clone();
        self.beside_memory_remedy_at(over, seq, move || fx.mono(), opts, review)
    }

    /// The memory remedy with a supplied monotonic clock.
    pub(super) fn beside_memory_remedy_at(
        &mut self,
        over: Option<super::super::memory::MemoryOver>,
        seq: u64,
        clock: impl FnOnce() -> Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
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
        let minimum = self.setup.beside_idle_min;
        if now.saturating_duration_since(first) < minimum {
            return Ok(());
        }
        // An unconfirmed agent is never restarted ([`super::observe::RESTART_WITHHELD`]): the
        // leader's ceiling ends here, refused, and is said once.
        if self.observing() {
            return self.withhold_memory_restart(leader, &over, seq, review);
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
        if super::observe::refused(&r) {
            return self.withhold_memory_restart(leader, &over, seq, review);
        }
        let reply = reply_line(&r);
        if crate::supervise::report::without_verb(&r) {
            self.beside_memory_ended = Some(leader);
            self.memory_refused = true;
            return Ok(());
        }
        if r.ok() || (r.is_err("restart unavailable") && !refusal_lifts(&r)) {
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

    /// THE MEMORY CEILING BESIDE A CONTRACT 4 CLAIM HOLDING `relaunch` (HX-15m, D19: the
    /// watchdog serves every contract 4 claim, a Codex tab's too, whose worker only watches).
    /// Where the host says such a claimant takes the tab's memory restart
    /// ([`IdleHost::scoped_restart`] of `Restart::Memory`), the restart is the host's
    /// ([`IdleHost::restart`], which tells that harness `restart cause=memory`), never the
    /// server's `restart` verb: asked at the agent's idle point as the screen reads it, on that
    /// unchanged screen, once per agent process, and, while the host defers it (a hand on the
    /// tab, a stopped claimant, another restart watched), asked again on the restart back-off.
    /// Whether the host's restart is the tab's memory remedy; where it is not, the self-claim's
    /// ([`Self::beside_memory_remedy`]) stands, unchanged.
    pub(super) fn beside_scoped_memory(
        &mut self,
        over: Option<super::super::memory::MemoryOver>,
        seen: &Turn,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let now = self.fx.mono();
        self.beside_scoped_memory_at(over, seen, now, opts, review)
    }

    /// [`Self::beside_scoped_memory`] at the loop's monotonic `now`.
    pub(super) fn beside_scoped_memory_at(
        &mut self,
        over: Option<super::super::memory::MemoryOver>,
        seen: &Turn,
        now: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        use super::super::policy::turn_end::{Restart, RestartAsk};
        let Some(host) = self.stall_host.clone() else {
            return Ok(false);
        };
        if !host.scoped_restart(&Restart::Memory) {
            self.beside_memory_retry = None;
            return Ok(false);
        }
        // The self-claim remedy's observation does not carry over to the claim's.
        self.beside_idle_at = None;
        let Some(over) = over.filter(super::super::memory::MemoryOver::acts) else {
            self.beside_memory_ended = None;
            self.beside_memory_retry = None;
            return Ok(true);
        };
        let Some(leader) = host.agent_pid() else {
            return Ok(true);
        };
        let seq = seen.screen.seq;
        if self.beside_memory_ended == Some(leader)
            || !review.unattended()
            || !opts.policy.relaunch
            || self.stalled.is_some()
            || !matches!(seen.phase, Phase::Idle | Phase::Limited { .. })
            || self.beside_memory_retry.is_some_and(|(_, at)| now < at)
            || self.screen()?.seq != seq
        {
            return Ok(true);
        }
        let ask = RestartAsk::of(&Restart::Memory, self.now_unix());
        let Some(answer) = host.restart(&Restart::Memory, &ask) else {
            return Ok(true);
        };
        let step = answer.step;
        if super::turn_end_loop::defers(&step) {
            let waits = self.beside_memory_retry.map_or(0, |(waits, _)| waits);
            let pause = alab_policy::ladder::Ladder(&self.turn_end.timing.restart_backoff)
                .step(usize::try_from(waits).unwrap_or(usize::MAX));
            self.beside_memory_retry = Some((waits.saturating_add(1), now + pause));
            if super::turn_end_loop::restart_note_due(waits) {
                review.note(&format!(
                    "WAITING seq={seq} until={} restart:memory waits: {step}",
                    self.stamp_in(now + pause)
                ));
            }
            return Ok(true);
        }
        self.beside_memory_retry = None;
        self.beside_memory_ended = Some(leader);
        let made = matches!(
            step.split_once(':').map_or(step.as_str(), |(head, _)| head),
            "adopted" | "relaunched" | "continued" | "done"
        );
        self.memory_refused = !made;
        review
            .say(&format!(
                "{} seq={seq} memory footprint_mb={} ceiling_mb={} reply={step}",
                if made { "RESTARTED" } else { "RESTART-REFUSED" },
                over.footprint_mib,
                over.ceiling_mib,
            ))
            .map_err(Fail::Hard)?;
        Ok(true)
    }

    /// The memory ceiling's restart withheld from an unconfirmed agent: ended for `leader` and
    /// refused, as a server's lasting refusal is, so it is said once,
    /// `RESTART-REFUSED … reply=(<why>)`.
    fn withhold_memory_restart(
        &mut self,
        leader: u32,
        over: &super::super::memory::MemoryOver,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.beside_memory_ended = Some(leader);
        self.memory_refused = true;
        review
            .say(&format!(
                "RESTART-REFUSED seq={seq} memory footprint_mb={} ceiling_mb={} reply=({})",
                over.footprint_mib,
                over.ceiling_mib,
                super::observe::RESTART_WITHHELD,
            ))
            .map_err(Fail::Hard)
    }
}
