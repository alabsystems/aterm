// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab's threaded [`Spread`] and what `aos-search --frames` says of a run, a neutral module
//! (design §13.3) so the M6 learner reaches it without naming `aos_search`.

use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_effects::aos_momentum::m4::Spread;

/// A [`Spread`] over `n` scoped threads: each takes the next index until none is left, so a spread
/// changes only speed.
#[derive(Clone, Copy, Debug)]
pub struct Threads(pub usize);

impl Spread for Threads {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        let next = AtomicUsize::new(0);
        let width = self.0.clamp(1, n.max(1));
        std::thread::scope(|s| {
            for _ in 0..width {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        job(i);
                    }
                });
            }
        });
    }
}

/// This many threads: the host's parallelism, at least one.
#[must_use]
pub fn available() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// A stretch of a search's own work after its last candidate trained ([`Stepped`]), which
/// `--frames` says as a step when it begins, so a process test's hang detector holds each stretch
/// rather than the run to its end, one CPU-bound length a loaded host stretches past it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The winner's held-out read.
    HeldOut,
    /// The winner's bundle verified before it is written.
    Verify,
}

impl Phase {
    /// Its name in the step.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::HeldOut => "held-out",
            Self::Verify => "verify",
        }
    }
}

/// Says on stderr that `phase` of search `serial` began: `aos-search step=G.<name>`, each at most
/// once a search. Its callers call it under `--frames` only.
pub fn say(serial: u32, phase: Phase) {
    eprintln!("aos-search step={serial}.{}", phase.name());
}

/// Says on stderr that a run has built its evaluator: `aos-search step=0.evaluator`, once, search 0
/// being before the first. Its caller calls it under `--frames` only.
pub fn say_built() {
    eprintln!("aos-search step=0.evaluator");
}

/// A [`Spread`] that says each job it has run as a step of search `serial`, under `--frames`:
/// `aos-search step=G.trained.N`, `N` counting the jobs finished from 0 across its calls, so each
/// is said once whatever the round or thread. With it a run takes a step per candidate trained, and
/// a process test's hang detector holds one candidate's training rather than the whole search, one
/// CPU-bound length a loaded host stretches past it.
pub struct Stepped<'a> {
    inner: &'a dyn Spread,
    serial: u32,
    done: AtomicUsize,
    sink: &'a (dyn Fn(&str) + Sync),
}

impl<'a> Stepped<'a> {
    /// `inner`, saying its steps on stderr.
    #[must_use]
    pub fn new(inner: &'a dyn Spread, serial: u32) -> Stepped<'a> {
        Stepped::to(inner, serial, &stderr_line)
    }

    /// `inner`, saying its steps to `sink`, a line each.
    #[must_use]
    pub fn to(inner: &'a dyn Spread, serial: u32, sink: &'a (dyn Fn(&str) + Sync)) -> Stepped<'a> {
        Stepped {
            inner,
            serial,
            done: AtomicUsize::new(0),
            sink,
        }
    }
}

impl Spread for Stepped<'_> {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        self.inner.run(n, &|i| {
            job(i);
            let k = self.done.fetch_add(1, Ordering::Relaxed);
            (self.sink)(&format!("aos-search step={}.trained.{k}", self.serial));
        });
    }
}

fn stderr_line(line: &str) {
    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use aterm_effects::aos_momentum::m4::Serial;

    use super::*;

    /// The thread spread runs every index exactly once at any width, so it changes only speed.
    #[test]
    fn threads_run_every_index_once() {
        for width in [1, 3, 16] {
            for n in [0, 1, 7, 50] {
                let hits: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
                Threads(width).run(n, &|i| {
                    hits[i].fetch_add(1, Ordering::SeqCst);
                });
                assert!(
                    hits.iter().all(|h| h.load(Ordering::SeqCst) == 1),
                    "width {width}, n {n}"
                );
            }
        }
    }

    /// A stepped spread runs every index once through the spread it wraps and says one step per job
    /// finished, numbered across its calls (a later round's jobs are new steps) in the order they
    /// finish; a call with no jobs says nothing.
    #[test]
    fn stepped_spreads_say_one_step_per_job_and_run_every_index_once() {
        let step = |k: usize| format!("aos-search step=2.trained.{k}");
        for (inner, ordered) in [(&Serial as &dyn Spread, true), (&Threads(3), false)] {
            let said = Mutex::new(Vec::new());
            let sink = |line: &str| {
                said.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(line.to_owned());
            };
            let lines = || said.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let stepped = Stepped::to(inner, 2, &sink);
            stepped.run(0, &|_| panic!("no job"));
            assert!(lines().is_empty(), "no job, no step");
            let hits: Vec<AtomicUsize> = (0..6).map(|_| AtomicUsize::new(0)).collect();
            stepped.run(6, &|i| {
                hits[i].fetch_add(1, Ordering::SeqCst);
            });
            assert!(hits.iter().all(|h| h.load(Ordering::SeqCst) == 1));
            let mut first = lines();
            if !ordered {
                first.sort_unstable();
            }
            assert_eq!(first, (0..6).map(step).collect::<Vec<_>>());
            stepped.run(2, &|_| {});
            let mut later = lines().split_off(6);
            later.sort_unstable();
            assert_eq!(
                later,
                [step(6), step(7)],
                "a later round's jobs are new steps"
            );
        }
    }
}
