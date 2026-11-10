// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WIDTH EACH TEST BINARY RUNS AT, WHAT IT IS CHARGED, AND THE ORDER THEY START IN
//! (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.4, A9).
//!
//! A binary runs at a width `w`, its `RUST_TEST_THREADS`, and is charged `c` cores of the test
//! stage's share `S`. [`Dispatcher`] starts a binary only while its charge fits in what the running
//! binaries' charges leave of `S`, so it admits at most `S` of historically estimated demand and
//! narrow binaries run side by side instead of one at a time. Descendant use contributes where
//! history measured it; an overrun is logged and raises the next charge, and runs alone once that
//! charge reaches `S`. This bounds charges, not unmeasured physical use (KI-1103-3).
//!
//! What a binary's history ([`crate::test_history`]) says of its last run, `cpu` (its process
//! group's CPU time, descendants included) and `wall` at width `w_o` over `n` tests, predicts it
//! at another width `w` from the threads it had, `t_o = min(w_o, n)`:
//! - its wall `wall(w) = wall · t_o / min(w, n)`. A thread busy waiting on a timer or a socket is
//!   as busy as one computing, so the threads, not the CPU, scale the wall: a binary of a hundred
//!   tests that each wait a second uses almost no CPU, and still takes a hundred times as long on
//!   one thread as on a hundred;
//! - its charge `c(w) = min(S, max(1, ⌈q · min(w, n)⌉))`, where `q` is the most CPU per thread
//!   any kept run showed (`cpu / wall / t_o`): a test that spawns processes uses more than its
//!   thread, and a binary whose run went over its charge is charged that much more next time.
//!
//! A measured binary retains at least the initial four-thread width, narrowed by its test
//! count, caller ceiling and measured CPU demand. A whole-run target cannot turn an isolated
//! concurrent suite into a serial one merely because unrelated suites take longer.
//! The plan has a target `T`: each binary runs at the narrowest remaining width whose predicted wall is
//! within `T` (its widest where none is), so the long poles get the width they need and the rest
//! leave the share to each other. A binary's widest is `min(S, n, ceiling)`, and no wider than
//! the width at which it uses `S` before the clamp (`max(1, ⌈q · min(w, n)⌉) ≤ S`; one where even
//! one thread uses more): a binary whose tests each run a pool of their own, charged `S` at a
//! wider width, would use more than the share with nothing beside it. No plan ends before its bound: the
//! share's CPU work `Σ wall · t_o · q / S` or the longest binary at
//! its widest, whichever is later. The planner tries that bound and each larger wall a binary
//! would take at an allowed width ([`TARGET_STEP`] apart at least), lays each plan out through
//! [`schedule`], and keeps the plan that ends first, the narrowest among ties. This optimizes
//! widths above the retained initial width; it cannot spend the whole run's slack by making
//! an already isolated binary serial. The prediction assumes parallel cases; measured replay
//! is still necessary to judge fixed startup costs, contention and cases of unequal lengths.
//!
//! Order: binaries with no history first (nothing says how long they take), at width
//! `min(4, S, ceiling)` and planned to hold as much memory as the heaviest binary the history
//! kept; then those whose last run was a FAIL, so a red shows early; then the rest; each group
//! but the first longest predicted first, the first in cargo's order. Known work then fills
//! predicted CPU gaps: a later binary moves ahead of a blocked head only if it fits the spare
//! charge and is predicted to finish before the running binaries release enough for that head.
//! This preserves the head's predicted start, unlike unrestricted first-fit packing. Predictions
//! change order only: execution still holds each binary's CPU and memory claims until it ends.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::Duration;

/// The initial libtest width, retained after history arrives unless the ceiling, number of
/// cases or measured CPU demand requires a narrower run.
pub const UNSEEN_WIDTH: u32 = 4;

/// The least factor between two targets the planner tries ([`plan`]): plans whose targets are
/// closer have schedules that differ by less than a prediction's own error, and a few hundred
/// binaries are planned in a few hundred schedules.
pub const TARGET_STEP: f64 = 1.05;

/// What one binary's history says (the planner's input; [`crate::test_history::History::seen`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
    /// Its last run's wall.
    pub wall: Duration,
    /// The `RUST_TEST_THREADS` its last run had.
    pub width: u32,
    /// The tests libtest ran in its last run (`running <n> tests`).
    pub tests: u32,
    /// The most CPU per thread any kept run showed (`cpu / wall / min(width, tests)`, at least
    /// zero; with no reading assume one busy core per thread).
    pub per_thread: f64,
    /// The most memory any kept run held at once, in bytes.
    pub peak: u64,
    /// Its last run was a FAIL.
    pub failed: bool,
}

impl Seen {
    /// The threads its last run had.
    fn threads(&self) -> u32 {
        self.width.min(self.tests).max(1)
    }

    /// The widest it runs at under `ceiling` on a share of `share` cores: no wider than its
    /// tests, nor than the width at which it uses the share ([`Seen::uses`]), since tests that
    /// each run processes or a pool of their own use more than their threads, and wider than that
    /// it would use more than the share however it is charged; at least one.
    fn widest(&self, ceiling: u32, share: u32) -> u32 {
        let most = ceiling.min(self.tests.max(1)).max(1);
        (1..=most)
            .rev()
            .find(|&w| self.uses(w) <= share)
            .unwrap_or(1)
    }

    /// Its predicted wall at width `w` (at least one).
    #[must_use]
    pub fn wall_at(&self, w: u32) -> Duration {
        let usable = w.max(1).min(self.tests.max(1));
        scale(self.wall, f64::from(self.threads()) / f64::from(usable))
    }

    /// What it is charged at width `w` (at least one) of a share of `share` cores: what it uses
    /// there, at most the share.
    #[must_use]
    pub fn charge_at(&self, w: u32, share: u32) -> u32 {
        self.uses(w).clamp(1, share.max(1))
    }

    /// The cores it uses at width `w` (at least one): its CPU per thread times the
    /// threads it has there, rounded up. A blocked thread does not consume a core.
    fn uses(&self, w: u32) -> u32 {
        let w = w.max(1);
        let usable = w.min(self.tests.max(1));
        let own = (self.per_thread.max(0.0) * f64::from(usable)).ceil();
        let own = if own.is_finite() && own < f64::from(u32::MAX) {
            // A finite, non-negative value below u32::MAX.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "checked finite, non-negative and below u32::MAX"
            )]
            let own = own as u32;
            own
        } else {
            u32::MAX
        };
        own.max(1)
    }
}

/// `d · k`, saturating, for a finite non-negative `k`.
fn scale(d: Duration, k: f64) -> Duration {
    Duration::try_from_secs_f64(d.as_secs_f64() * k).unwrap_or(Duration::MAX)
}

/// One binary of the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    /// Its index in the run's binaries (cargo's order).
    pub index: usize,
    /// Its `RUST_TEST_THREADS`.
    pub width: u32,
    /// The cores of the share it holds while it runs.
    pub charge: u32,
    /// The memory it holds while it runs, at most, in bytes (0: none known).
    pub peak: u64,
    /// Its predicted wall; `None` with no history.
    pub predicted: Option<Duration>,
}

/// The run's plan: every binary once, in the order they start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The share `S` the charges are taken from.
    pub share: u32,
    /// The target `T` (zero with no history at all).
    pub target: Duration,
    /// Every binary, in the order the dispatcher starts them.
    pub order: Vec<Planned>,
}

/// Plan binaries whose histories are `seen` (cargo's order) on a share of `share` cores, each
/// binary's width at most `ceiling` (`--test-threads`, or the caller's `RUST_TEST_THREADS`), a
/// binary with no history planned to hold `unseen_peak` bytes.
#[must_use]
pub fn plan(seen: &[Option<Seen>], share: u32, ceiling: Option<u32>, unseen_peak: u64) -> Plan {
    let share = share.max(1);
    let ceiling = ceiling.unwrap_or(u32::MAX).clamp(1, share);
    let known: Vec<(usize, Seen)> = seen
        .iter()
        .enumerate()
        .filter_map(|(index, s)| Some((index, (*s)?)))
        .collect();
    let floor = bound(&known, share, ceiling);
    let mut walls: Vec<Duration> = known
        .iter()
        .flat_map(|(_, s)| (1..=s.widest(ceiling, share)).map(|w| s.wall_at(w)))
        .filter(|&wall| wall > floor)
        .collect();
    walls.sort_unstable();
    let mut tried: Option<Duration> = None;
    let mut best: Option<(Duration, Plan)> = None;
    for target in std::iter::once(floor).chain(walls) {
        if tried.is_some_and(|t| target.as_secs_f64() < t.as_secs_f64() * TARGET_STEP) {
            continue;
        }
        tried = Some(target);
        let candidate = fill_gaps(Plan {
            share,
            target,
            order: at_target(&known, target, share, ceiling),
        });
        let end = schedule(&candidate).end;
        // The targets rise, so of plans that end together the last tried is the narrowest.
        if best.as_ref().is_none_or(|(first, _)| end <= *first) {
            best = Some((end, candidate));
        }
    }
    let (target, seen_order) =
        best.map_or((Duration::ZERO, Vec::new()), |(_, p)| (p.target, p.order));
    let width = UNSEEN_WIDTH.min(ceiling);
    let mut order: Vec<Planned> = seen
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_none())
        .map(|(index, _)| Planned {
            index,
            width,
            charge: width,
            peak: unseen_peak,
            predicted: None,
        })
        .collect();
    order.extend(seen_order);
    Plan {
        share,
        target,
        order,
    }
}

/// Fill predicted CPU gaps without delaying the blocked head's predicted start. This runs only
/// over known binaries, before unseen work is prepended. The resulting order is replayed by the
/// ordinary dispatcher, so its memory claim is measured by the same schedule as every other
/// plan. Actual durations are never deadlines: a prediction grants no admission or verdict.
fn fill_gaps(mut plan: Plan) -> Plan {
    let mut pending = std::mem::take(&mut plan.order);
    let mut running: BinaryHeap<Reverse<(Duration, usize, u32)>> = BinaryHeap::new();
    let mut now = Duration::ZERO;
    let mut held = 0_u32;
    loop {
        while let Some(head) = pending.first() {
            let fits = |b: &Planned| held.saturating_add(b.charge) <= plan.share;
            let next = if fits(head) {
                Some(0)
            } else {
                // Reserve the head's earliest start under the running jobs' predictions.
                let mut endings: Vec<_> = running.iter().map(|r| r.0).collect();
                endings.sort_unstable();
                let mut remaining = held;
                let reserved = endings.into_iter().find_map(|(end, _, charge)| {
                    remaining -= charge;
                    (remaining.saturating_add(head.charge) <= plan.share).then_some(end)
                });
                reserved.and_then(|reserved| {
                    pending.iter().position(|b| {
                        fits(b)
                            && b.predicted
                                .is_some_and(|wall| now.saturating_add(wall) <= reserved)
                    })
                })
            };
            let Some(next) = next else { break };
            let b = pending.remove(next);
            held += b.charge;
            running.push(Reverse((
                now.saturating_add(b.predicted.unwrap_or(plan.target)),
                b.index,
                b.charge,
            )));
            plan.order.push(b);
        }
        let Some(Reverse((end, _, charge))) = running.pop() else {
            return plan;
        };
        now = end;
        held -= charge;
    }
}

/// The earliest any plan of the binaries `known` on a share of `share` cores, each at most
/// `ceiling` wide, can end as predicted: their CPU work over the share, or the longest
/// of them at its widest ([`Seen::widest`]),
/// whichever is later. Zero
/// with none known.
fn bound(known: &[(usize, Seen)], share: u32, ceiling: u32) -> Duration {
    let work: f64 = known
        .iter()
        .map(|(_, s)| s.wall.as_secs_f64() * f64::from(s.threads()) * s.per_thread.max(0.0))
        .sum();
    let work = Duration::try_from_secs_f64(work / f64::from(share.max(1))).unwrap_or(Duration::MAX);
    let longest = known
        .iter()
        .map(|(_, s)| s.wall_at(s.widest(ceiling, share)))
        .max()
        .unwrap_or(Duration::ZERO);
    work.max(longest)
}

/// The binaries `known` (by their indexes) at target `target`: each at the narrowest width whose
/// predicted wall is within it, starting at its retained initial width and using its widest
/// where none is, in the order they start: those whose
/// last run failed, then the rest, each longest predicted first.
fn at_target(known: &[(usize, Seen)], target: Duration, share: u32, ceiling: u32) -> Vec<Planned> {
    let mut failed = Vec::new();
    let mut rest = Vec::new();
    for &(index, s) in known {
        let widest = s.widest(ceiling, share);
        let width = (UNSEEN_WIDTH.min(widest)..=widest)
            .find(|&w| s.wall_at(w) <= target)
            .unwrap_or(widest);
        let planned = Planned {
            index,
            width,
            charge: s.charge_at(width, share),
            peak: s.peak,
            predicted: Some(s.wall_at(width)),
        };
        if s.failed {
            failed.push(planned);
        } else {
            rest.push(planned);
        }
    }
    let longest_first =
        |a: &Planned, b: &Planned| b.predicted.cmp(&a.predicted).then(a.index.cmp(&b.index));
    failed.sort_by(longest_first);
    rest.sort_by(longest_first);
    failed.extend(rest);
    failed
}

/// Where a binary of the plan is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Not started.
    Queued,
    /// Started and not ended.
    Running,
    /// Ended.
    Done,
}

/// THE DISPATCHER'S ONE RULE, apart from its threads ([`crate::testrun::dispatch`]), so its
/// conformance test drives exactly the code that runs
/// (`aterm_models_capacity::test_dispatch_model`).
///
/// It starts the plan's binaries in the plan's order, the next one only once its charge fits in
/// what the running binaries' charges leave of the share, its peak in what their peaks leave of
/// the stage's memory, and the running binaries' live CPU rate plus its charge is within the
/// share and one core. A binary that ends gives its charge and its peak back. Once halted it starts
/// nothing more. Each charge is at most the share and the memory at least the largest peak, so
/// the next binary always fits once nothing runs: every queued binary starts unless the run
/// halts.
#[derive(Clone, Debug, PartialEq)]
pub struct Dispatcher {
    share: u32,
    memory: Option<u64>,
    order: Vec<Planned>,
    /// Each binary's place in `order`, by its index.
    at: Vec<Option<usize>>,
    phase: Vec<Phase>,
    next: usize,
    held: u32,
    held_bytes: u64,
    halted: bool,
}

impl Dispatcher {
    /// A dispatcher of `plan` on its share and, where the stage claimed it, `memory` bytes, raised
    /// to the largest peak of the plan (no binary runs on less than its own).
    #[must_use]
    pub fn new(plan: &Plan, memory: Option<u64>) -> Self {
        let share = plan.share.max(1);
        let largest = plan.order.iter().map(|b| b.peak).max().unwrap_or(0);
        let order: Vec<Planned> = plan
            .order
            .iter()
            .map(|b| Planned {
                charge: b.charge.clamp(1, share),
                ..*b
            })
            .collect();
        let width = order.iter().map(|b| b.index + 1).max().unwrap_or(0);
        let mut at = vec![None; width];
        for (place, b) in order.iter().enumerate() {
            at[b.index] = Some(place);
        }
        Dispatcher {
            share,
            memory: memory.map(|m| m.max(largest)),
            phase: vec![Phase::Queued; width],
            at,
            order,
            next: 0,
            held: 0,
            held_bytes: 0,
            halted: false,
        }
    }

    /// Whether the next binary may start now, the running binaries using `live` cores at their
    /// last reads.
    fn admits(&self, b: &Planned, live: f64) -> bool {
        let running = self.held > 0;
        !self.halted
            && self.held.saturating_add(b.charge) <= self.share
            && self
                .memory
                .is_none_or(|m| self.held_bytes.saturating_add(b.peak) <= m)
            && (!running || live + f64::from(b.charge) <= f64::from(self.share) + 1.0)
    }

    /// Start the next binary of the plan, if it may start now: it then holds its charge and its
    /// peak until [`Dispatcher::finish`].
    pub fn next(&mut self, live: f64) -> Option<Planned> {
        let b = *self.order.get(self.next)?;
        if !self.admits(&b, live) {
            return None;
        }
        self.next += 1;
        self.phase[b.index] = Phase::Running;
        self.held += b.charge;
        self.held_bytes = self.held_bytes.saturating_add(b.peak);
        Some(b)
    }

    /// Binary `index` ended: its charge and its peak go back.
    ///
    /// # Errors
    /// It was not running: nothing goes back.
    pub fn finish(&mut self, index: usize) -> Result<(), &'static str> {
        if self.phase.get(index) != Some(&Phase::Running) {
            return Err("that binary is not running");
        }
        let b = self
            .at
            .get(index)
            .copied()
            .flatten()
            .and_then(|place| self.order.get(place))
            .ok_or("that binary is not in the plan")?;
        self.phase[index] = Phase::Done;
        self.held -= b.charge;
        self.held_bytes = self.held_bytes.saturating_sub(b.peak);
        Ok(())
    }

    /// Start nothing more.
    pub fn halt(&mut self) {
        self.halted = true;
    }

    /// Whether a binary is left to start: one is queued and the run has not halted.
    #[must_use]
    pub fn waiting(&self) -> bool {
        !self.halted && self.next < self.order.len()
    }

    /// Where binary `index` is.
    #[must_use]
    pub fn phase(&self, index: usize) -> Option<Phase> {
        self.phase.get(index).copied()
    }

    /// The cores the running binaries hold.
    #[must_use]
    pub const fn held(&self) -> u32 {
        self.held
    }

    /// The bytes the running binaries hold.
    #[must_use]
    pub const fn held_bytes(&self) -> u64 {
        self.held_bytes
    }

    /// The memory the binaries' peaks are kept within, where the stage claimed it.
    #[must_use]
    pub const fn memory(&self) -> Option<u64> {
        self.memory
    }

    /// Whether the run stopped starting binaries.
    #[must_use]
    pub const fn halted(&self) -> bool {
        self.halted
    }
}

/// What a plan's schedule does ([`schedule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// When its last binary ends.
    pub end: Duration,
    /// The most memory its binaries hold at once, bytes.
    pub peak: u64,
}

/// The plan's schedule when each binary takes its predicted wall (one with no history: the
/// plan's target), started as [`Dispatcher`] starts them on no memory bound and no live rate:
/// when it ends, which the planner keeps the plan that ends first by ([`plan`]), and the most
/// memory its binaries hold at once, which the test run claims for them, so the claim is the
/// schedule's own concurrent peak rather than every binary's at once.
#[must_use]
pub fn schedule(plan: &Plan) -> Schedule {
    let mut d = Dispatcher::new(plan, None);
    let mut running = BinaryHeap::new();
    let mut now = Duration::ZERO;
    let mut peak = 0;
    loop {
        while let Some(b) = d.next(0.0) {
            let took = b.predicted.unwrap_or(plan.target);
            running.push(Reverse((now.saturating_add(took), b.index)));
            peak = peak.max(d.held_bytes());
        }
        // Nothing runs, so the head started if one was left: every binary has ended.
        let Some(Reverse((end, index))) = running.pop() else {
            return Schedule { end: now, peak };
        };
        now = end;
        // It was started, so it ends once: nothing refuses this.
        d.finish(index).ok();
    }
}

#[cfg(test)]
#[path = "test_plan_tests.rs"]
mod tests;
