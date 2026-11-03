// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded exclusive file-lock contention waits on Unix and Windows.

use std::io;
use std::time::{Duration, Instant};

/// Acquire an exclusive lock on an already validated file, waiting only for
/// actual lock contention until `deadline`. A first attempt is always made,
/// including when the deadline has passed. Other I/O errors return immediately.
/// File admission and any protocol-specific identity recheck belong to the caller.
/// The caller must explicitly unlock before closing or transferring ownership.
///
/// # Errors
/// Returns `WouldBlock` with `busy` when the lock remains held at the deadline,
/// or the original error of attempting the lock.
pub fn lock_exclusive_until(file: &std::fs::File, deadline: Instant, busy: &str) -> io::Result<()> {
    lock_exclusive_until_with(file, deadline, busy, || {})
}

/// [`lock_exclusive_until`], calling `held` after each actual contention result
/// before waiting. A test may release its holder there without timing assumptions.
///
/// # Errors
/// Returns [`lock_exclusive_until`]'s errors.
pub fn lock_exclusive_until_with(
    file: &std::fs::File,
    deadline: Instant,
    busy: &str,
    held: impl FnMut(),
) -> io::Result<()> {
    lock_exclusive_with_deadline(file, Some(deadline), busy, held)
}

/// Shared contention wait; `None` retains an unrepresentable duration's unbounded wait.
pub(crate) fn lock_exclusive_with_deadline(
    file: &std::fs::File,
    deadline: Option<Instant>,
    busy: &str,
    held: impl FnMut(),
) -> io::Result<()> {
    lock_exclusive_with_clock(file, deadline, busy, held, Instant::now, std::thread::sleep)
}

/// The clock and wait boundary lets tests advance the deadline without scheduling assumptions.
pub(crate) fn lock_exclusive_with_clock(
    file: &std::fs::File,
    deadline: Option<Instant>,
    busy: &str,
    mut held: impl FnMut(),
    now: impl Fn() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    crate::keep_out_of_children(file)?;
    let remaining = || {
        deadline.map_or(Duration::MAX, |deadline| {
            deadline.saturating_duration_since(now())
        })
    };
    let expired = || io::Error::new(io::ErrorKind::WouldBlock, busy.to_owned());
    let mut backoff = Duration::from_millis(1);
    let mut first_attempt = true;
    loop {
        if !first_attempt && remaining().is_zero() {
            return Err(expired());
        }
        first_attempt = false;
        #[expect(
            clippy::disallowed_methods,
            reason = "the caller retains the validated file and explicitly unlocks before close"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
            Err(std::fs::TryLockError::WouldBlock) => {
                if remaining().is_zero() {
                    return Err(expired());
                }
                #[cfg(any(test, debug_assertions))]
                LOCK_WAIT_OBSERVER.with(|observer| {
                    if let Some(observer) = observer.borrow_mut().as_mut() {
                        observer(file);
                    }
                });
                held();
                let pause = backoff.min(remaining());
                if pause.is_zero() {
                    return Err(expired());
                }
                sleep(pause);
                backoff = (backoff * 2).min(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(any(test, debug_assertions))]
type LockWaitObserver = Box<dyn FnMut(&std::fs::File)>;

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    static LOCK_WAIT_OBSERVER: std::cell::RefCell<Option<LockWaitObserver>> =
        const { std::cell::RefCell::new(None) };
}

/// Test support: observe actual, unexpired lock contention on this thread while
/// `run` executes. The previous observer is restored even if `run` unwinds.
/// The observer must not recursively acquire a contended lock on this thread.
#[cfg(any(test, debug_assertions))]
pub fn with_lock_wait_observer<T>(
    observer: impl FnMut(&std::fs::File) + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<LockWaitObserver>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LOCK_WAIT_OBSERVER.with(|observer| {
                *observer.borrow_mut() = self.0.take();
            });
        }
    }
    let previous = LOCK_WAIT_OBSERVER.with(|current| current.replace(Some(Box::new(observer))));
    let _restore = Restore(previous);
    run()
}
