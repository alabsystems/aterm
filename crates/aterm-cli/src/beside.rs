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
#[path = "beside_tests.rs"]
pub(crate) mod tests;
