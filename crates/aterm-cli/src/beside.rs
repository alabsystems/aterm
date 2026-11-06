// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Work run beside its caller on a scoped thread, or by the caller itself where no thread starts.

use std::any::Any;
use std::thread::{Scope, ScopedJoinHandle};

/// Work [`start`] began beside its caller; [`Self::answer`] waits for it.
pub(crate) enum Beside<'scope, T, F> {
    /// On a thread of its own.
    Thread(ScopedJoinHandle<'scope, T>),
    /// No thread could start, so the caller runs the work when it asks for the answer.
    Here(F),
}

/// `work` on a thread named `name` in `scope`. Where the thread cannot start (the process is out
/// of threads or memory), the work stays with the caller, who runs it when it asks for the
/// answer: the answer is the same, only later.
pub(crate) fn start<'scope, T, F>(
    scope: &'scope Scope<'scope, '_>,
    name: &str,
    work: F,
) -> Beside<'scope, T, F>
where
    F: FnOnce() -> T + Clone + Send + 'scope,
    T: Send + 'scope,
{
    match spawned(scope, name, work.clone()) {
        Ok(thread) => Beside::Thread(thread),
        Err(_) => Beside::Here(work),
    }
}

fn spawned<'scope, T, F>(
    scope: &'scope Scope<'scope, '_>,
    name: &str,
    work: F,
) -> std::io::Result<ScopedJoinHandle<'scope, T>>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    #[cfg(test)]
    if tests::REFUSED.get() {
        return Err(std::io::Error::other(
            "no thread starts under tests::refusing",
        ));
    }
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn_scoped(scope, work)
}

impl<T, F: FnOnce() -> T> Beside<'_, T, F> {
    /// The work's answer; a panic in it carries on in the caller.
    pub(crate) fn answer(self) -> T {
        self.answer_or(|panic| std::panic::resume_unwind(panic))
    }

    /// The work's answer, or `panicked`'s from the payload of a panic in it, wherever it ran.
    pub(crate) fn answer_or(self, panicked: impl FnOnce(Box<dyn Any + Send>) -> T) -> T {
        match self {
            Self::Thread(thread) => thread.join().unwrap_or_else(panicked),
            Self::Here(work) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(panicked),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
}
