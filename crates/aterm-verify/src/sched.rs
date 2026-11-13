// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Concurrent stage scheduler ([`Lane`], [`StageSpec::after_stages`],
//! [`crate::plan::StageId::opens_windows`], [`crate::plan::Lane::AfterCompile`], [`awaited`],
//! [`check_waits`]), within the run's memory budget ([`StageSpec::mem`], [`memory_holders`]),
//! ended early by an ordinary source-preflight red or an armed release/`--fail-fast` halt
//! ([`Halt`]). The release tree half otherwise collects the complete sweep.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::ladder::Report;
use crate::plan::{Lane, StageSpec};

/// What a run's first proven red ends (design §4.3, A3): set once, by the first stage row that is
/// a finding or the first test binary that ended FAIL, in a run that stops there (an armed release, or
/// `--fail-fast`), or a source/type preflight in an ordinary run. Once set, no stage or test
/// binary starts: [`run_stages`] stops holding a waiting stage back, so each starts at once and
/// says NOT RUN with [`Halt::words`] (`staged`'s first look); the test dispatcher starts no
/// binary; and the end it was given ends the run's live children (`exec::group::halt`: SIGTERM,
/// a grace, then SIGKILL, each read as stopped rather than as a finding). A NOT RUN for the
/// machine and a skip are no proven red, and set nothing.
#[derive(Debug)]
pub struct Halt {
    /// What a red ends, when it ends anything: the subject its words name ("the release", "the
    /// run") and what else ends with it.
    armed: Option<Armed>,
    /// Ordinary runs stop only at failures which cannot be inherited from main.
    preflight: Option<fn(&str)>,
    /// The halt's words, once set: `stopped: <subject> failed at <check>`.
    words: OnceLock<String>,
}

#[derive(Debug)]
struct Armed {
    subject: &'static str,
    end: fn(&str),
}

impl Halt {
    /// A run that never halts: its full sweep runs whatever fails.
    #[must_use]
    pub const fn off() -> Self {
        Self {
            armed: None,
            preflight: None,
            words: OnceLock::new(),
        }
    }

    /// A run its first proven red ends: `subject` is what its words say failed ("the release",
    /// "the run"), and `end` ends what else the run has running, given the words.
    #[must_use]
    pub const fn at_first_red(subject: &'static str, end: fn(&str)) -> Self {
        Self {
            armed: Some(Armed { subject, end }),
            preflight: None,
            words: OnceLock::new(),
        }
    }

    /// Stop an ordinary run at a source or type-preflight failure. Test failures still
    /// complete the sweep so `verify land` can apply its existing inheritance rule.
    #[must_use]
    pub const fn after_preflight(end: fn(&str)) -> Self {
        Self {
            armed: None,
            preflight: Some(end),
            words: OnceLock::new(),
        }
    }

    /// Whether every proven red ends this run. A preflight-only stop does not alter the test
    /// dispatcher's full-sweep behavior or its inherited-failure evidence.
    #[must_use]
    pub const fn armed(&self) -> bool {
        self.armed.is_some()
    }

    /// A proven red of `check`: the first selected red halts the run and is `true`.
    /// A preflight-only run ignores test reds; a completely unarmed run ignores every red.
    pub fn red(&self, check: &str) -> bool {
        let (subject, end) = match &self.armed {
            Some(armed) => (armed.subject, armed.end),
            None => match self.preflight {
                Some(end)
                    if matches!(
                        check,
                        "released-shim" | "fmt" | "census" | "export-scan" | "lint"
                    ) =>
                {
                    ("the run", end)
                }
                _ => return false,
            },
        };
        let mut first = false;
        let words = self.words.get_or_init(|| {
            first = true;
            format!("stopped: {subject} failed at {check}")
        });
        if first {
            end(words);
        }
        first
    }

    /// What every row the halt stopped or kept from starting says, once the run is halted.
    #[must_use]
    pub fn words(&self) -> Option<&str> {
        self.words.get().map(String::as_str)
    }
}

/// Live scheduler bookkeeping.
#[derive(Debug)]
struct State {
    started: Vec<bool>,
    done: Vec<bool>,
    running: usize,
    results: Vec<Option<Report>>,
    /// The first stage's panic, once one panicked: no stage starts after it, the collector stops
    /// waiting, and [`run_stages`] raises it again once every stage thread has ended.
    panicked: Option<Box<dyn std::any::Any + Send>>,
}

/// Can stage `i` start right now, within a run whose stages may claim `budget` bytes of memory
/// together? Pure, so the rule is testable without threads: free of every other constraint
/// ([`unconstrained`]) and held by no stage's memory ([`memory_holders`]).
#[must_use]
pub(crate) fn ready(
    specs: &[StageSpec],
    started: &[bool],
    done: &[bool],
    running: usize,
    i: usize,
    budget: u64,
) -> bool {
    unconstrained(specs, started, done, running, i)
        && memory_holders(specs, started, done, running, i, budget).is_empty()
}

/// Could stage `i` start right now, memory aside? Not started, no unfinished earlier stage it
/// shares a lane with, no exclusive one either side, no two that open desktop windows at once, and
/// every stage it awaits done.
fn unconstrained(
    specs: &[StageSpec],
    started: &[bool],
    done: &[bool],
    running: usize,
    i: usize,
) -> bool {
    if started[i] {
        return false;
    }
    let me = &specs[i];
    let blocked = specs.iter().take(i).zip(done).any(|(earlier, finished)| {
        !finished
            && (me.exclusive
                || earlier.exclusive
                || (earlier.lane == me.lane && me.lane != Lane::Pure)
                || (earlier.id.opens_windows() && me.id.opens_windows()))
    });
    !blocked && !(me.exclusive && running > 0) && awaited(specs, i).all(|j| done[j])
}

/// THE STAGES WHOSE MEMORY HOLDS STAGE `i` BACK, by index; none for a stage that claims no memory
/// ([`StageSpec::mem`] `0`), which memory never holds. A stage that claims some does not start
///
/// - while the stages that claim some and run would claim more than `budget` with it: the ones
///   running are its holders. With none running it starts whatever it claims, so a stage that
///   claims more than the whole budget runs, and runs with no other that claims memory;
/// - nor while one declared before it that claims some is free of every other constraint
///   ([`unconstrained`]) and not yet started: that one is its holder, so a stage that fits never
///   overtakes an earlier one that waits for room, and the earlier one starts as soon as there
///   is room for it.
///
/// The scheduler alone never deadlocks on this: with nothing running, the first stage declared
/// that claims memory and is free of every other constraint has no holder, and stages that claim
/// none have none, so a stage is ready whenever one is free of every other constraint, as it is
/// without memory ([`check_waits`]). What a started stage waits for outside it is not this
/// rule's: its claims in the capacity ledger, which wait on other runs' claims, and which hold
/// nothing while they wait but a timed stage's quiet machine, its run's across its timed
/// stages, with memory claimed only on it (`crate::staged`; the ledger's FIFO and ALONE rules
/// hold its waits to an end).
#[must_use]
pub fn memory_holders(
    specs: &[StageSpec],
    started: &[bool],
    done: &[bool],
    running: usize,
    i: usize,
    budget: u64,
) -> Vec<usize> {
    let me = &specs[i];
    if me.mem == 0 || started[i] {
        return Vec::new();
    }
    let claiming: Vec<usize> = (0..specs.len())
        .filter(|&j| started[j] && !done[j] && specs[j].mem > 0)
        .collect();
    let claimed = claiming
        .iter()
        .fold(0u64, |sum, &j| sum.saturating_add(specs[j].mem));
    if !claiming.is_empty() && claimed.saturating_add(me.mem) > budget {
        return claiming;
    }
    (0..i)
        .filter(|&j| specs[j].mem > 0 && unconstrained(specs, started, done, running, j))
        .take(1)
        .collect()
}

/// What memory kept a stage from starting once nothing else did ([`memory_holders`]): for how
/// long, and behind which stages (by index, each named once, in the order first seen).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// The time it was held by memory alone.
    pub waited: Duration,
    /// The stages that held it.
    pub behind: Vec<usize>,
}

/// Stages `i` waits for ([`StageSpec::after_stages`], [`check_waits`]).
pub fn awaited(specs: &[StageSpec], i: usize) -> impl Iterator<Item = usize> + '_ {
    let me = &specs[i];
    (0..i).filter(move |&j| me.after_stages.contains(&specs[j].id))
}

/// What keeps the no-deadlock argument above true: a stage awaits only stages declared before it.
///
/// # Errors
/// Names the first stage that awaits itself or a stage declared after it.
pub fn check_waits(specs: &[StageSpec]) -> Result<(), String> {
    for (i, me) in specs.iter().enumerate() {
        if let Some(later) = specs[i..].iter().find(|s| me.after_stages.contains(&s.id)) {
            return Err(format!(
                "{} awaits {}, which is not declared before it",
                me.title, later.title
            ));
        }
    }
    Ok(())
}

/// How often a stage waiting in a run that can halt looks for the halt between the wakes a
/// stage's end brings: a halt set while a stage still runs (a test binary's FAIL) releases the
/// waiting stages within this, not when that stage ends.
const HALT_LOOK: Duration = Duration::from_millis(50);

/// Wait for a wake, or within [`HALT_LOOK`] in a run that can halt; a poisoned lock is still read.
fn wait<'a>(cv: &Condvar, g: MutexGuard<'a, State>, armed: bool) -> MutexGuard<'a, State> {
    if armed {
        cv.wait_timeout(g, HALT_LOOK)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    } else {
        cv.wait(g).unwrap_or_else(PoisonError::into_inner)
    }
}

/// Run every stage under the rule above, its stages that claim memory within `budget` bytes
/// together ([`ready`]); `run` is told what memory held each stage back ([`Held`]). Once `halt`
/// is set ([`Halt::red`]) nothing holds a stage back any more: each that has not started starts
/// within [`HALT_LOOK`], so `run` says what the halt kept from running without waiting for its
/// lane, its memory or an exclusive stage to come free.
///
/// Every stage thread ends its stage, however `run` ends: a `run` that panics still marks its
/// stage done and wakes every waiting thread, no stage starts after it, the stages already
/// running finish, and then the first panic is raised again here, to the caller, with its own
/// payload. A panicking stage therefore never leaves a later stage, or the collector, waiting
/// for a report that will not come.
///
/// # Panics
/// On a plan whose stage awaits itself or a later stage ([`check_waits`]), and with the first
/// panic of `run`, once every stage thread has ended.
pub fn run_stages<R, P>(
    specs: &[StageSpec],
    budget: u64,
    halt: &Halt,
    run: R,
    mut on_report: P,
) -> Vec<Report>
where
    R: Fn(&StageSpec, &Held) -> Report + Sync,
    P: FnMut(usize, &Report),
{
    let n = specs.len();
    if n == 0 {
        return Vec::new();
    }
    if let Err(why) = check_waits(specs) {
        panic!("aterm-verify plan defect: {why}");
    }
    let state = Mutex::new(State {
        started: vec![false; n],
        done: vec![false; n],
        running: 0,
        results: (0..n).map(|_| None).collect(),
        panicked: None,
    });
    let cv = Condvar::new();
    let run = &run;

    // No code runs under this lock that can panic, and a stage's panic is caught outside it; a
    // poisoned lock is still read, so a panic can never turn into a second one here.
    let lock = || state.lock().unwrap_or_else(PoisonError::into_inner);

    let mut ordered: Vec<Report> = Vec::with_capacity(n);
    std::thread::scope(|scope| {
        for i in 0..n {
            let cv = &cv;
            let lock = &lock;
            scope.spawn(move || {
                let mut held = Held::default();
                {
                    let mut g = lock();
                    let mut since: Option<Instant> = None;
                    while g.panicked.is_none()
                        && halt.words().is_none()
                        && !ready(specs, &g.started, &g.done, g.running, i, budget)
                    {
                        let holders =
                            memory_holders(specs, &g.started, &g.done, g.running, i, budget);
                        if unconstrained(specs, &g.started, &g.done, g.running, i)
                            && !holders.is_empty()
                        {
                            since.get_or_insert_with(Instant::now);
                            for h in holders {
                                if !held.behind.contains(&h) {
                                    held.behind.push(h);
                                }
                            }
                        } else if let Some(at) = since.take() {
                            held.waited += at.elapsed();
                        }
                        g = wait(cv, g, halt.armed());
                    }
                    if g.panicked.is_some() {
                        // An earlier stage panicked: this one never starts, and the run ends
                        // with that panic.
                        return;
                    }
                    if let Some(at) = since {
                        held.waited += at.elapsed();
                    }
                    g.started[i] = true;
                    g.running += 1;
                }
                // A stage held back only until this one started may start now
                // ([`memory_holders`]).
                cv.notify_all();
                let ran = catch_unwind(AssertUnwindSafe(|| run(&specs[i], &held)));
                {
                    let mut g = lock();
                    g.done[i] = true;
                    g.running -= 1;
                    match ran {
                        Ok(report) => g.results[i] = Some(report),
                        Err(payload) => {
                            g.panicked.get_or_insert(payload);
                        }
                    }
                }
                cv.notify_all();
            });
        }

        for i in 0..n {
            let mut g = lock();
            while g.results[i].is_none() && g.panicked.is_none() {
                g = cv.wait(g).unwrap_or_else(PoisonError::into_inner);
            }
            let Some(report) = g.results[i].take() else {
                // A stage panicked: the scope's end waits for every stage thread, and the panic
                // is raised below.
                return;
            };
            drop(g);
            on_report(i, &report);
            ordered.push(report);
        }
    });
    if let Some(payload) = state
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .panicked
    {
        resume_unwind(payload);
    }
    ordered
}

#[cfg(test)]
#[path = "sched_tests.rs"]
mod tests;
