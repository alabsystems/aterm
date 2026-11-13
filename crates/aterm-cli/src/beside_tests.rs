// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Beside, start};
use std::cell::Cell;

thread_local! {
    /// Set while [`refusing`] runs: no [`start`] on this thread gets a thread.
    pub(super) static REFUSED: Cell<bool> = const { Cell::new(false) };
}

/// `body` with every [`start`] it makes on this thread refused a thread, as a process out of
/// threads refuses one.
pub(crate) fn refusing<R>(body: impl FnOnce() -> R) -> R {
    struct Lift(bool);
    impl Drop for Lift {
        fn drop(&mut self) {
            REFUSED.set(self.0);
        }
    }
    let _lift = Lift(REFUSED.replace(true));
    body()
}

/// Work that gets a thread runs on it; work refused one runs in the caller as it asks for the
/// answer, and both give the same answer. NEGATIVE CONTROL: the thread's id differs from the
/// caller's only where a thread started.
#[test]
fn work_refused_a_thread_runs_in_its_caller_with_the_same_answer() {
    let caller = std::thread::current().id();
    let work = || (6 * 7, std::thread::current().id());
    let threaded = std::thread::scope(|scope| {
        let beside = start(scope, "aterm-test-beside", work);
        assert!(matches!(beside, Beside::Thread(_)));
        beside.answer()
    });
    assert_eq!(threaded.0, 42);
    assert_ne!(threaded.1, caller, "the work ran on a thread of its own");
    let here = refusing(|| {
        std::thread::scope(|scope| {
            let beside = start(scope, "aterm-test-beside", work);
            assert!(matches!(beside, Beside::Here(_)));
            beside.answer()
        })
    });
    assert_eq!(here, (42, caller), "the caller ran the work itself");
    let after = std::thread::scope(|scope| start(scope, "aterm-test-beside", work).answer());
    assert_ne!(after.1, caller, "refusing ends with its body");
}

/// A panic in the work is `answer_or`'s fallback whether the work had a thread or not.
/// NEGATIVE CONTROL: work that returns gives its own answer.
#[test]
fn a_panic_in_the_work_is_the_fallback_s_answer_wherever_it_ran() {
    let panics = || -> &'static str { panic!("the work fails") };
    let fine = || "answered";
    for refused in [false, true] {
        let run = |work: fn() -> &'static str| {
            let answer = || {
                std::thread::scope(|scope| {
                    start(scope, "aterm-test-beside", work).answer_or(|panic| {
                        assert_eq!(panic.downcast_ref(), Some(&"the work fails"));
                        "fallback"
                    })
                })
            };
            if refused { refusing(answer) } else { answer() }
        };
        assert_eq!(run(panics), "fallback", "refused: {refused}");
        assert_eq!(run(fine), "answered", "refused: {refused}");
    }
}
