// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

thread_local! {
    static HEARD: Cell<u32> = const { Cell::new(0) };
    static HEARD_OUTER: Cell<u32> = const { Cell::new(0) };
}

fn hears() {
    HEARD.set(HEARD.get() + 1);
}

fn hears_outer() {
    HEARD_OUTER.set(HEARD_OUTER.get() + 1);
}

/// AT THE FIRST TICK, THEN AT LEAST [`EVERY`] APART: a step's first tick says it began, and a
/// loop of a million entries reports no more often than one of ten.
#[test]
fn a_step_is_heard_at_once_and_then_at_most_so_often() {
    let start = Instant::now();
    let base = HEARD.get();
    heard_by(hears, || {
        tick_at(start);
        assert_eq!(HEARD.get() - base, 1, "at once");
        tick_at(start + EVERY / 2);
        assert_eq!(HEARD.get() - base, 1, "inside the spacing");
        tick_at(start + EVERY);
        assert_eq!(HEARD.get() - base, 2, "once it has passed");
        tick_at(start + EVERY + EVERY / 2);
        assert_eq!(HEARD.get() - base, 2, "measured from the last report");
    });
}

/// EVERY STEP STARTS FRESH: a step that begins just after another ended is heard at its first
/// tick, because that tick is what says it began.
#[test]
fn a_new_step_is_heard_at_its_first_tick() {
    let start = Instant::now();
    let base = HEARD.get();
    heard_by(hears, || tick_at(start));
    heard_by(hears, || tick_at(start));
    assert_eq!(HEARD.get() - base, 2);
}

/// ANOTHER THREAD'S TICK TELLS NO ONE: this is what keeps a waiter's poke, or anything else
/// beside a hung step, from reading as that step's progress. What hears here counts across
/// threads, or a hook that reached the other thread would count where no one looks.
#[test]
fn a_tick_on_another_thread_tells_no_one() {
    static ANYWHERE: AtomicU32 = AtomicU32::new(0);
    fn hears_anywhere() {
        ANYWHERE.fetch_add(1, Ordering::SeqCst);
    }
    heard_by(hears_anywhere, || {
        std::thread::scope(|scope| {
            scope.spawn(tick).join().expect("the other thread ticked");
        });
        assert_eq!(ANYWHERE.load(Ordering::SeqCst), 0);
        tick();
        assert_eq!(
            ANYWHERE.load(Ordering::SeqCst),
            1,
            "this thread's own is heard"
        );
    });
}

/// A STEP THAT PANICS TAKES ITS HOOK WITH IT: a thread that goes on after a caught panic must
/// not report for a step that is over.
#[test]
fn a_step_that_panics_takes_its_hook_with_it() {
    let base = HEARD.get();
    let failed = std::panic::catch_unwind(|| {
        heard_by(hears, || panic!("a step that fails (a test's)"));
    });
    assert!(failed.is_err());
    tick();
    assert_eq!(HEARD.get(), base);
}

/// AN INNER STEP HANDS BACK TO THE OUTER: the outer hook hears again, on its own spacing.
#[test]
fn an_inner_step_hands_back_to_the_outer() {
    let start = Instant::now();
    let (outer, inner) = (HEARD_OUTER.get(), HEARD.get());
    heard_by(hears_outer, || {
        tick_at(start);
        heard_by(hears, || tick_at(start + EVERY / 2));
        assert_eq!((HEARD_OUTER.get() - outer, HEARD.get() - inner), (1, 1));
        tick_at(start + EVERY / 2);
        assert_eq!(HEARD_OUTER.get() - outer, 1, "inside its own spacing");
        tick_at(start + EVERY);
        assert_eq!(
            (HEARD_OUTER.get() - outer, HEARD.get() - inner),
            (2, 1),
            "measured from its own report, not the inner step's"
        );
    });
}

/// WITH NOTHING LISTENING A TICK ONLY COUNTS: a CLI's walk has no one to tell.
#[test]
fn with_no_hook_a_tick_tells_no_one_and_still_counts() {
    let (heard, before) = (HEARD.get(), ticks_here());
    tick();
    tick();
    assert_eq!(ticks_here() - before, 2);
    assert_eq!(HEARD.get(), heard);
}
