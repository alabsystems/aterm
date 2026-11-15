// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The census's threads: [`std::thread::scope`] whose threads carry their starter's input
//! capture ([`crate::inputs::capture`]), run inline under a caller's single-worker policy
//! ([`without_parallel_workers`], [`on_one_worker`]).

thread_local! {
    static SERIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// [`std::thread::Scope`] for census code: each thread it starts carries the input capture of the
/// thread that started it, so a captured census's helper thread records its inputs.
#[derive(Clone, Copy)]
pub(crate) struct Threads<'scope, 'env>(&'scope std::thread::Scope<'scope, 'env>);

/// Run nested census work on the calling thread. Input captures still follow each job. This
/// bounds a caller that already owns one CPU permit without another executor.
/// # Panics
/// The work panics.
pub fn without_parallel_workers<T>(work: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERIAL.set(self.0);
        }
    }
    let _restore = Restore(SERIAL.replace(true));
    work()
}

/// Run census work on one worker with the same 8 MiB stack as its named parallel workers.
/// Nested jobs run inline, carrying their input captures.
/// # Errors
/// The worker could not be started.
/// # Panics
/// The work panics; its panic is resumed on the caller.
pub fn on_one_worker<T: Send>(work: impl FnOnce() -> T + Send) -> std::io::Result<T> {
    if SERIAL.get() {
        return Ok(work());
    }
    threads(|scope| {
        scope
            .spawn_named("census-serial".into(), 8 << 20, || {
                without_parallel_workers(work)
            })
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    })
}

/// A census job, run on a thread or inline under its caller's single-worker policy.
#[must_use]
pub(crate) enum Joined<'scope, T> {
    Thread(std::thread::ScopedJoinHandle<'scope, T>),
    Inline(Completed<T>),
}

/// An inline job's unjoined panic still ends its scope, as a scoped thread's does.
pub(crate) struct Completed<T>(Option<std::thread::Result<T>>);

impl<T> Completed<T> {
    fn join(mut self) -> std::thread::Result<T> {
        self.0.take().expect("the completed job has one answer")
    }
}

impl<T> Drop for Completed<T> {
    fn drop(&mut self) {
        if let Some(Err(panic)) = self.0.take()
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

impl<T> Joined<'_, T> {
    pub(crate) fn join(self) -> std::thread::Result<T> {
        match self {
            Self::Thread(worker) => worker.join(),
            Self::Inline(answer) => answer.join(),
        }
    }
}

impl<'scope> Threads<'scope, '_> {
    /// Starts `f` on a new thread of this scope, in the starting thread's input capture.
    pub(crate) fn spawn<T: Send + 'scope>(
        self,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> Joined<'scope, T> {
        let capture = crate::inputs::carried();
        let work = move || capture.enter(f);
        if SERIAL.get() {
            Joined::Inline(Completed(Some(std::panic::catch_unwind(
                std::panic::AssertUnwindSafe(work),
            ))))
        } else {
            Joined::Thread(self.0.spawn(work))
        }
    }

    /// [`Self::spawn`] on a thread named `name` with a `stack`-byte stack.
    pub(crate) fn spawn_named<T: Send + 'scope>(
        self,
        name: String,
        stack: usize,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> std::io::Result<Joined<'scope, T>> {
        if SERIAL.get() {
            return Ok(self.spawn(f));
        }
        let capture = crate::inputs::carried();
        std::thread::Builder::new()
            .name(name)
            .stack_size(stack)
            .spawn_scoped(self.0, move || capture.enter(f))
            .map(Joined::Thread)
    }
}

/// [`std::thread::scope`] whose threads carry their starter's input capture ([`Threads`]).
pub(crate) fn threads<'env, T>(f: impl for<'scope> FnOnce(Threads<'scope, 'env>) -> T) -> T {
    std::thread::scope(|scope| f(Threads(scope)))
}

#[cfg(test)]
#[path = "workers_tests.rs"]
mod tests;
