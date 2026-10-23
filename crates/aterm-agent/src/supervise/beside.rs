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
}
