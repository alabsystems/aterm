// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TEST RUN'S WIDTH PLANNER AND BUDGET DISPATCHER
//! (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.4, A9).
//!
//! Each test binary `b` runs at a width `w_b`, its `RUST_TEST_THREADS`, and holds a charge `c_b` of
//! the run's share `S` of cores while it runs. The charge is the larger of its width and the cores
//! its last run used, descendants included, at that width, so a binary whose tests start busy
//! child processes is charged for them. The dispatcher starts the next binary only when the free
//! share covers its charge: the sum of the charges of the binaries running never exceeds `S`.
//!
//! Widths come from what each binary took the last time it ran here ([`crate::test_history`],
//! read as [`Seen`]): the target makespan is `T* = max(Σ held / S, max floor)`, where a binary
//! holds `max(cpu, wall × threads)` core-seconds ([`Seen::held_s`]: a waiting thread holds its
//! core). Each candidate plan gives every binary the least width whose predicted wall is within
//! `T*` times one of [`TIGHTEN`], so a short binary runs narrow beside others instead of taking
//! the whole share; the plan kept is the candidate whose simulated schedule ([`Plan::makespan`])
//! ends soonest. Binaries with no history start first, at width `min(4, cap)`; then the ones
//! that failed last time; then the rest, longest predicted first.
//!
//! A charge comes from what a binary used last time; the stage's meter reads what the binaries
//! running use now, every member of their process groups counted ([`crate::memory::Meter::live_rate`]),
//! and a start also waits while that rate plus its charge would pass the share by more than a core.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use crate::test_history::{History, Row};

/// The width a binary with no history starts at, within its cap.
pub const NEW_WIDTH: u32 = 4;

/// What one binary did the last time it ran, as its history's samples say.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    /// Its `RUST_TEST_THREADS`.
    pub width: u32,
    /// Its wall time, seconds.
    pub wall_s: f64,
    /// The CPU time its tree used, seconds (its wall times its width where unread).
    pub cpu_s: f64,
    /// The shortest wall of a run that started every one of its tests at once (its width at least
    /// its test count), seconds: about its longest test, which no width can beat. 0 until such a
    /// run; a narrower run's wall is how long its queue of tests took, not a floor.
    pub floor_s: f64,
    /// How many tests libtest said it runs, when it said.
    pub tests: Option<u32>,
    /// Whether that run failed.
    pub failed: bool,
}

impl Seen {
    /// The cores it used last time: CPU over wall.
    fn observed(&self) -> f64 {
        (self.cpu_s / self.wall_s.max(1e-3)).max(1e-3)
    }

    /// The test threads it runs at width `w`: `w`, or fewer when it has fewer tests.
    fn threads_at(&self, w: u32) -> u32 {
        self.tests.map_or(w, |t| w.min(t)).max(1)
    }

    /// The test threads it ran last time.
    fn threads(&self) -> u32 {
        self.threads_at(self.width)
    }

    /// The cores it uses at width `w`: each of its threads brings what each brought last time
    /// (less than a core when its tests wait, more when its descendants work beside it). Its
    /// tests are a queue its threads drain, so the rate scales with the threads that run, in
    /// either direction: a run at width 1 does not pin it there, and the next run's measure at
    /// the width planned corrects the scale.
    #[must_use]
    pub fn rate(&self, w: u32) -> f64 {
        let per_thread = self.observed() / f64::from(self.threads());
        f64::from(self.threads_at(w)) * per_thread
    }

    /// The core-seconds it holds of the share: its charge is at least its width, so a thread
    /// that waits holds its core all the same, and its descendants' cores are charged too. Under
    /// [`Self::rate`] this is the same at every width that runs no idle thread.
    #[must_use]
    pub fn held_s(&self) -> f64 {
        self.cpu_s.max(self.wall_s * f64::from(self.threads()))
    }

    /// Its predicted wall at width `w`, seconds: its CPU at [`Self::rate`], never below its
    /// floor.
    #[must_use]
    pub fn predicted(&self, w: u32) -> f64 {
        (self.cpu_s / self.rate(w)).max(self.floor_s)
    }
}

impl Seen {
    /// A binary's row read as its newest sample, with the floor its all-at-once runs set; `None`
    /// for a row with no sample.
    #[must_use]
    pub fn of(row: &Row) -> Option<Seen> {
        let last = row.samples.last()?;
        let secs = |ms: u64| Duration::from_millis(ms).as_secs_f64();
        let width = last.width.max(1);
        let wall_s = secs(last.wall_ms).max(1e-3);
        let tests = |t: Option<u64>| t.map(|t| u32::try_from(t).unwrap_or(u32::MAX));
        let floor_s = row
            .samples
            .iter()
            .filter(|s| tests(s.tests).is_some_and(|t| s.width >= t.max(1)))
            .map(|s| secs(s.wall_ms))
            .reduce(f64::min)
            .unwrap_or(0.0);
        Some(Seen {
            width,
            wall_s,
            cpu_s: last
                .cpu_ms
                .map_or(wall_s * f64::from(width), |cpu| secs(cpu).max(1e-3)),
            floor_s,
            tests: tests(last.tests),
            failed: last.outcome == "FAIL",
        })
    }
}

/// Every binary the history knows, read as [`Seen`], by its re-run spec.
#[must_use]
pub fn seen(history: &History) -> BTreeMap<String, Seen> {
    history
        .rows()
        .iter()
        .filter_map(|row| Some((row.test.clone(), Seen::of(row)?)))
        .collect()
}

/// Why a binary is where it is in the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// No history: it starts first, at [`NEW_WIDTH`].
    New,
    /// It failed last time: its verdict is the likeliest to be red, so it comes early.
    FailedLast,
    /// Known: longest predicted first.
    Known,
}

/// The run's plan: an order over the binaries, each one's width and charge.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The share `S` the charges are drawn from.
    pub share: u32,
    /// Binary indices in dispatch order.
    pub order: Vec<usize>,
    pub width: Vec<u32>,
    pub charge: Vec<u32>,
    pub class: Vec<Class>,
    /// Each binary's predicted wall at its width, seconds; `None` for a new one.
    pub predicted: Vec<Option<f64>>,
    /// The makespan its schedule predicts ([`Plan::makespan`]), seconds; `None` with no history.
    pub target_s: Option<f64>,
}

impl Plan {
    /// When the dispatcher's schedule of this plan ends, predicted: binaries in order, the head
    /// starting once the free share covers its charge, each ending after its predicted wall (a
    /// new one, unknown, after none).
    #[must_use]
    pub fn makespan(&self) -> f64 {
        let mut running: Vec<(f64, u32)> = Vec::new();
        let (mut now, mut free, mut end) = (0.0_f64, self.share, 0.0_f64);
        for &i in &self.order {
            let charge = self.charge[i].clamp(1, self.share);
            while free < charge {
                let first = running
                    .iter()
                    .enumerate()
                    .min_by(|a, b| a.1.0.total_cmp(&b.1.0))
                    .map(|(k, _)| k)
                    .expect("a charge short of the share has a binary running");
                let (ends, returned) = running.swap_remove(first);
                now = now.max(ends);
                free += returned;
            }
            let ends = now + self.predicted[i].unwrap_or(0.0);
            running.push((ends, charge));
            free -= charge;
            end = end.max(ends);
        }
        end
    }

    /// One binary, one thread, one core.
    #[must_use]
    pub fn one() -> Plan {
        plan(&["one"], &BTreeMap::new(), 1, 1)
    }
}

/// How far below `T*` a binary's predicted wall must come, tried in turn: at `T*` itself a long
/// pole may take the whole target beside narrow binaries the share then cannot pack, and a
/// tighter bound widens the poles. [`plan`] keeps the one whose schedule ends soonest.
const TIGHTEN: [f64; 5] = [1.0, 0.75, 0.5, 1.0 / 3.0, 0.25];

/// Plan `specs` within `share` cores, no binary wider than `cap` threads: of the plans whose
/// widths come within `T*` × each of [`TIGHTEN`], the one whose schedule ([`Plan::makespan`])
/// ends soonest, the looser on a tie.
#[must_use]
pub fn plan(specs: &[&str], history: &BTreeMap<String, Seen>, share: u32, cap: u32) -> Plan {
    let share = share.max(1);
    let seen: Vec<Option<&Seen>> = specs.iter().map(|s| history.get(*s)).collect();
    let known = seen.iter().flatten();
    let held: f64 = known.clone().map(|s| s.held_s()).sum();
    let floor = known.clone().map(|s| s.floor_s).fold(0.0_f64, f64::max);
    let Some(target) = seen
        .iter()
        .any(Option::is_some)
        .then(|| (held / f64::from(share)).max(floor))
    else {
        return plan_within(&seen, share, cap, None);
    };
    TIGHTEN
        .iter()
        .map(|t| plan_within(&seen, share, cap, Some(target * t)))
        .reduce(|best, p| {
            if p.makespan() < best.makespan() {
                p
            } else {
                best
            }
        })
        .expect("TIGHTEN is not empty")
}

/// The plan whose known binaries each take the least width predicted to end within `bound`.
fn plan_within(seen: &[Option<&Seen>], share: u32, cap: u32, bound: Option<f64>) -> Plan {
    let cap = cap.clamp(1, share);
    let cap_of = |s: Option<&Seen>| s.and_then(|s| s.tests).map_or(cap, |t| cap.min(t.max(1)));
    let mut width = Vec::with_capacity(seen.len());
    let mut charge = Vec::with_capacity(seen.len());
    let mut class = Vec::with_capacity(seen.len());
    let mut predicted = Vec::with_capacity(seen.len());
    for s in seen {
        let cap_b = cap_of(*s);
        match (s, bound) {
            (Some(s), Some(bound)) => {
                let w = (1..=cap_b)
                    .find(|&w| s.predicted(w) <= bound)
                    .unwrap_or(cap_b);
                width.push(w);
                charge.push((s.rate(w).ceil() as u32).clamp(w, share));
                class.push(if s.failed {
                    Class::FailedLast
                } else {
                    Class::Known
                });
                predicted.push(Some(s.predicted(w)));
            }
            _ => {
                let w = NEW_WIDTH.min(cap_b);
                width.push(w);
                charge.push(w);
                class.push(Class::New);
                predicted.push(None);
            }
        }
    }
    let mut order: Vec<usize> = (0..seen.len()).collect();
    order.sort_by(|&a, &b| {
        let wall = |i: usize| predicted[i].unwrap_or(f64::INFINITY);
        class[a]
            .cmp(&class[b])
            .then_with(|| wall(b).total_cmp(&wall(a)))
            .then_with(|| a.cmp(&b))
    });
    let mut plan = Plan {
        share,
        order,
        width,
        charge,
        class,
        predicted,
        target_s: None,
    };
    plan.target_s = bound.map(|_| plan.makespan());
    plan
}

/// The free share the dispatcher draws charges from, and the next place in its order.
struct Budget {
    free: Mutex<(u32, usize)>,
    returned: Condvar,
}

/// A step of the dispatcher, as its observer sees it under the budget's lock
/// ([`dispatch_observed`]): a binary has taken its charge, or is about to return it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Start(usize),
    End(usize),
}

/// A started binary's charge, returned to its budget when dropped, so a binary whose callback
/// panics does not keep it from the binaries still queued.
struct Held<'a> {
    budget: &'a Budget,
    index: usize,
    charge: u32,
    observe: &'a (dyn Fn(Step) + Sync),
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut free = self
            .budget
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (self.observe)(Step::End(self.index));
        free.0 += self.charge;
        drop(free);
        self.budget.returned.notify_all();
    }
}

/// Run `run(i, width)` for every binary of `plan`, in its order, on at most `workers` threads,
/// each start waiting until the free share covers its charge (greedy list scheduling: the head of
/// the queue waits, so a wide binary is never starved by narrow ones behind it). A finished
/// binary returns its charge, one whose callback panics included (the panic is the dispatcher's
/// once the others are done). Answers are by index. Each worker books its children into the
/// calling stage's memory meter ([`crate::memory::current`]) and hands them its cover marker
/// ([`crate::memory::cover`]); it also retains the actual stage reservation, so a nested bounded
/// walk can outlive its callback without releasing memory still used by that walk.
pub fn dispatch<T, F>(plan: &Plan, workers: usize, run: F) -> Vec<Option<T>>
where
    T: Send,
    F: Fn(usize, u32) -> T + Sync,
{
    let meter = crate::memory::current();
    let rate = || meter.as_ref().and_then(|m| m.live_rate());
    dispatch_with(plan, workers, &|_| {}, &rate, run)
}

/// [`dispatch`], telling `observe` each [`Step`] under the budget's lock, so the order it sees is
/// the order the charges were taken and returned in: the seam the dispatcher's conformance to
/// its model is judged through (`tests/conformance_test_dispatch.rs`).
#[cfg(any(test, debug_assertions))]
pub fn dispatch_observed<T, F>(
    plan: &Plan,
    workers: usize,
    observe: &(dyn Fn(Step) + Sync),
    run: F,
) -> Vec<Option<T>>
where
    T: Send,
    F: Fn(usize, u32) -> T + Sync,
{
    dispatch_with(plan, workers, observe, &|| None, run)
}

/// How long a start held back by the live rate ([`dispatch_with`]) waits before it looks again,
/// when no charge comes back sooner: the rate moves with no charge returned.
const RATE_LOOK_EVERY: Duration = Duration::from_millis(250);

/// [`dispatch`] with `observe` told each [`Step`] and `rate` read for the cores the running
/// binaries use now. THE BACKSTOP: a start whose charge the free share covers also waits while
/// the live rate plus its charge would pass the share by more than one core, unless nothing
/// runs; a binary whose descendants outran its charge then holds back the next start, which its
/// history alone would not until its next run.
fn dispatch_with<T, F>(
    plan: &Plan,
    workers: usize,
    observe: &(dyn Fn(Step) + Sync),
    rate: &(dyn Fn() -> Option<f64> + Sync),
    run: F,
) -> Vec<Option<T>>
where
    T: Send,
    F: Fn(usize, u32) -> T + Sync,
{
    let n = plan.order.len();
    let answers: Vec<Mutex<Option<T>>> = (0..plan.width.len()).map(|_| Mutex::new(None)).collect();
    let budget = Budget {
        free: Mutex::new((plan.share, 0)),
        returned: Condvar::new(),
    };
    let meter = crate::memory::current();
    let cover = crate::memory::cover();
    let memory = crate::current_memory_claim();
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, n.max(1)) {
            scope.spawn(|| {
                let _metering = crate::memory::metering(meter.clone());
                let _covering = crate::memory::covering(cover.clone());
                let _reservation = crate::holding_memory_claim(memory.clone());
                loop {
                    let (i, held) = {
                        let mut free = budget.free.lock().unwrap_or_else(PoisonError::into_inner);
                        // The head is read again after every wait: another worker may have
                        // started it meanwhile.
                        loop {
                            let Some(&i) = plan.order.get(free.1) else {
                                return;
                            };
                            let charge = plan.charge[i].clamp(1, plan.share);
                            let idle = free.0 == plan.share;
                            let fits = free.0 >= charge;
                            let cool = idle
                                || rate().is_none_or(|r| {
                                    r + f64::from(charge) <= f64::from(plan.share) + 1.0
                                });
                            if fits && cool {
                                free.0 -= charge;
                                free.1 += 1;
                                observe(Step::Start(i));
                                break (
                                    i,
                                    Held {
                                        budget: &budget,
                                        index: i,
                                        charge,
                                        observe,
                                    },
                                );
                            }
                            free = if fits {
                                budget
                                    .returned
                                    .wait_timeout(free, RATE_LOOK_EVERY)
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .0
                            } else {
                                budget
                                    .returned
                                    .wait(free)
                                    .unwrap_or_else(PoisonError::into_inner)
                            };
                        }
                    };
                    let out = run(i, plan.width[i]);
                    *answers[i].lock().unwrap_or_else(PoisonError::into_inner) = Some(out);
                    drop(held);
                }
            });
        }
    });
    answers
        .into_iter()
        .map(|s| s.into_inner().ok().flatten())
        .collect()
}

#[cfg(test)]
#[path = "test_plan_tests.rs"]
mod tests;
