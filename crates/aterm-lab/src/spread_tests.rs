// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
