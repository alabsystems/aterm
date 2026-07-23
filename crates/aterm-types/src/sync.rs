// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Non-poisoning synchronization primitives.
//!
//! Drop-in replacements for `parking_lot::Mutex`, `MutexGuard`, and `Condvar`
//! built on top of `std::sync`. These wrappers recover from poison automatically
//! so callers get the same ergonomic API as `parking_lot` (`.lock()` returns a
//! guard directly, no `Result`) without pulling in an external crate.
//!
//! # Why not `std::sync::Mutex` directly?
//!
//! Terminal state must remain accessible even if a thread panicked while holding
//! a lock. `std::sync::Mutex` returns `Result<MutexGuard, PoisonError>` from
//! `.lock()`, forcing every call site to handle poison — and the correct
//! terminal-engine answer is always "recover". These wrappers centralize that
//! decision.
//!
//! # Differences from `parking_lot`
//!
//! - Backed by `std::sync` (no external dependency, works under Miri).
//! - `Condvar::wait_for` matches `parking_lot`'s signature:
//!   `wait_for(&self, &mut MutexGuard, Duration) -> WaitTimeoutResult`.

use std::ops::{Deref, DerefMut};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Mutex
// ---------------------------------------------------------------------------

/// A non-poisoning mutual-exclusion lock.
///
/// Wraps [`std::sync::Mutex`] and auto-recovers from poison on every lock
/// acquisition, so callers never need to handle `PoisonError`.
pub struct Mutex<T> {
    /// The wrapped std mutex — a NAMED field (not a tuple `.0`) so the
    /// lock-order census (OB-7) resolves the wrapper's interior delegations
    /// to the `raw` identity instead of UNKNOWN. Callers' acquisitions
    /// resolve to their own receiver names via the `.lock()`/`.try_lock()`
    /// tokens; `raw` is the merged class node of the wrapper's own interior
    /// and joins no held-acquire edges (the interiors nest nothing).
    raw: std::sync::Mutex<T>,
}

impl<T> Mutex<T> {
    /// Create a new mutex wrapping `val`.
    #[must_use]
    pub const fn new(val: T) -> Self {
        Self {
            raw: std::sync::Mutex::new(val),
        }
    }

    /// Acquire the lock, recovering from poison if necessary.
    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let guard = match self.raw.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                log_poison_recovery("Mutex::lock");
                poisoned.into_inner()
            }
        };
        MutexGuard(Some(guard))
    }

    /// Try to acquire the lock without blocking.
    ///
    /// Returns `None` if the lock is currently held by another thread.
    // Skip: the native typed-TrustIr lowering of the `std::sync::TryLockError`
    // payload-match does not complete (toolchain gap — the same class the
    // campaign documents for these std-sync wrappers), which fail-closes the
    // WHOLE crate's gate at this fn. The body is a total mapping over std's
    // `try_lock` result — both poison arms recover, no panic path of our own;
    // its panic-freedom bottoms out at absent std bodies. Verify-only skip;
    // callers demote to an expected-absent-callee assumption row.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        match self.raw.try_lock() {
            Ok(guard) => Some(MutexGuard(Some(guard))),
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(std::sync::TryLockError::Poisoned(e)) => {
                log_poison_recovery("Mutex::try_lock");
                Some(MutexGuard(Some(e.into_inner())))
            }
        }
    }

    /// Get a mutable reference to the underlying data.
    ///
    /// Since this requires `&mut self`, no locking is needed.
    // Skip: delegates to the absent std `Mutex::get_mut` body (fail-closed
    // hard error for the whole crate's gate until the toolchain grows a
    // totality summary for it — it returns `LockResult`, poison → `Err`, no
    // panic path). Same audited-total-wrapper class as `try_lock` above.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn get_mut(&mut self) -> &mut T {
        match self.raw.get_mut() {
            Ok(r) => r,
            Err(poisoned) => {
                log_poison_recovery("Mutex::get_mut");
                poisoned.into_inner()
            }
        }
    }

    /// Consume the mutex and return the underlying data.
    // Skip: same audited-total-wrapper class as `get_mut`/`try_lock` above —
    // the absent std `Mutex::into_inner` returns `LockResult`, poison
    // recovers, no panic path of our own.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn into_inner(self) -> T {
        match self.raw.into_inner() {
            Ok(v) => v,
            Err(poisoned) => {
                log_poison_recovery("Mutex::into_inner");
                poisoned.into_inner()
            }
        }
    }
}

/// F11-4 (#7941): emit a structured error log when a poisoned lock is
/// silently recovered. Silent recovery hides the fact that some other
/// thread panicked mid-critical-section — observability must not lose
/// that signal. Pulled out into a function so every recovery site ends
/// up with identical output and a stable call-site location.
#[cold]
#[inline(never)]
#[track_caller]
fn log_poison_recovery(site: &'static str) {
    // Trust gate note: this calls `aterm_log::__log` directly with per-site
    // constant messages and routes the caller location through the record's
    // `file`/`line` fields, instead of the `error!` macro. The macro's
    // `format_args!` with runtime arguments produces an `fmt::Arguments`
    // constructor the Trust native lowering cannot model; constant-literal
    // `format_args!` lowers fine. The log record carries the same signal
    // (level, site name, caller file/line); recovery semantics are identical.
    macro_rules! log_site {
        ($site:literal) => {
            aterm_log::__log(
                aterm_log::Level::Error,
                ::core::module_path!(),
                ::core::format_args!(concat!(
                    $site,
                    ": Mutex was poisoned — recovering inner value; another thread \
                     panicked mid-critical-section (see this record's file:line for \
                     the caller)"
                )),
                ::core::option::Option::Some(::core::panic::Location::caller().file()),
                ::core::option::Option::Some(::core::panic::Location::caller().line()),
            )
        };
    }
    match site {
        "Mutex::lock" => log_site!("Mutex::lock"),
        "Mutex::try_lock" => log_site!("Mutex::try_lock"),
        "Mutex::get_mut" => log_site!("Mutex::get_mut"),
        "Mutex::into_inner" => log_site!("Mutex::into_inner"),
        "Condvar::wait_for" => log_site!("Condvar::wait_for"),
        _ => log_site!("Mutex"),
    }
}

impl<T: Default> Default for Mutex<T> {
    // Skip: thin generic forwarder into `T::default()` — an OPEN-trait
    // dispatch on the instantiating type parameter, whose impl is unknowable
    // pre-monomorphization (the one genuinely-fatal dispatch class; the
    // sealed-trait closed-world rung does not apply to std `Default`).
    // Same class as the `DebugAsDisplay::fmt` skip.
    #[cfg_attr(trust_verify, trust::skip)]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Mutex<T> {
    // Skip: thin generic forwarder into the instantiating type's `Debug`
    // impl (user code, may panic by design) — same class as `default` above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.raw.try_lock() {
            Ok(guard) => f.debug_tuple("Mutex").field(&*guard).finish(),
            Err(std::sync::TryLockError::WouldBlock) => {
                f.debug_tuple("Mutex").field(&"<locked>").finish()
            }
            Err(std::sync::TryLockError::Poisoned(e)) => {
                f.debug_tuple("Mutex").field(&*e.into_inner()).finish()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MutexGuard
// ---------------------------------------------------------------------------

/// RAII guard returned by [`Mutex::lock`].
///
/// Wraps [`std::sync::MutexGuard`] in an `Option` so that the inner guard can
/// be temporarily extracted for APIs that consume it by value (e.g.,
/// `std::sync::Condvar::wait_timeout`).
pub struct MutexGuard<'a, T>(Option<std::sync::MutexGuard<'a, T>>);

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    #[allow(
        clippy::expect_used,
        reason = "trait impl cannot return Result; \
        INVARIANT: Option is always Some while the guard is live — \
        it is only temporarily None inside Condvar::wait_for"
    )]
    // AUDITED CONTRACT PANIC: the Option is None only transiently inside
    // Condvar::wait_for, which holds the only &mut to the guard while the
    // inner std guard is taken — no deref can interleave. A None here means
    // the guard-liveness invariant was broken; failing closed is the contract.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "MutexGuard inner was taken")
    )]
    fn deref(&self) -> &T {
        // INVARIANT: The Option is always Some while the guard is live.
        // It is only temporarily None inside Condvar::wait_for.
        // Explicit match + panic! (not `.expect(..)`): expect's panic lives in
        // the absent std body, which both defeats the contract_panic matcher
        // (unused-annotation = gate error) and leaves an unmodeled panic row;
        // the bare `panic!` binds the annotation directly (ArrayVec::push
        // precedent). Same message, same abort.
        match self.0.as_ref() {
            Some(inner) => inner,
            None => panic!("MutexGuard inner was taken"),
        }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    #[inline]
    #[allow(
        clippy::expect_used,
        reason = "trait impl cannot return Result; \
        INVARIANT: Option is always Some while the guard is live — \
        it is only temporarily None inside Condvar::wait_for"
    )]
    // AUDITED CONTRACT PANIC: same guard-liveness invariant as `deref` —
    // None is only transient inside Condvar::wait_for, which holds the only
    // &mut to the guard; a None here is a broken invariant, fail closed.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "MutexGuard inner was taken")
    )]
    fn deref_mut(&mut self) -> &mut T {
        // INVARIANT: The Option is always Some while the guard is live.
        // It is only temporarily None inside Condvar::wait_for.
        // Explicit match + panic! — same rationale as `deref` above.
        match self.0.as_mut() {
            Some(inner) => inner,
            None => panic!("MutexGuard inner was taken"),
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for MutexGuard<'_, T> {
    // Skip: thin generic forwarder into the instantiating type's `Debug`
    // impl — same open-trait dispatch class as `Mutex::default`/`fmt` above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

// ---------------------------------------------------------------------------
// WaitTimeoutResult
// ---------------------------------------------------------------------------

/// Result of a condvar wait with timeout.
///
/// Matches the `parking_lot::WaitTimeoutResult` API: call `.timed_out()` to
/// check whether the wait expired before being notified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns `true` if the wait timed out.
    #[must_use]
    pub fn timed_out(&self) -> bool {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Condvar
// ---------------------------------------------------------------------------

/// A non-poisoning condition variable.
///
/// Wraps [`std::sync::Condvar`] and provides a `wait_for` method with the
/// same signature as `parking_lot::Condvar::wait_for`.
pub struct Condvar(std::sync::Condvar);

impl Condvar {
    /// Create a new condition variable.
    #[must_use]
    pub const fn new() -> Self {
        Self(std::sync::Condvar::new())
    }

    /// Wait on the condvar with a timeout.
    ///
    /// Temporarily releases the lock, waits up to `timeout`, then reacquires
    /// the lock. Returns a [`WaitTimeoutResult`] indicating whether the
    /// timeout elapsed.
    ///
    /// This matches the `parking_lot::Condvar::wait_for` signature.
    // AUDITED CONTRACT PANIC: the `.take().expect(...)` below fires only if
    // the guard's inner Option is already None, i.e. a re-entrant wait_for on
    // the same guard — impossible through the public API (we hold the only
    // &mut and restore Some before returning). Failing closed is the contract.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "MutexGuard inner was already taken")
    )]
    // Skip: delegates to std `Condvar::wait_timeout`, whose documented panic
    // ("used with more than one mutex") is DELIBERATELY excluded from the
    // totality allowlist — a real possibility no wrapper code can preclude.
    // The assumption is the usage invariant: every aterm Condvar pairs with
    // exactly one Mutex for its lifetime (enforced by each call site's
    // struct layout — condvar and mutex live in the same owning struct).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn wait_for<T>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: Duration,
    ) -> WaitTimeoutResult {
        // Take the inner std guard out so we can pass it by value to
        // std::sync::Condvar::wait_timeout, which consumes it.
        // Explicit match + panic! (not `.expect(..)`): expect's panic lives in
        // the absent std body, which defeats the contract_panic matcher —
        // same rationale as the guard `deref`/`deref_mut` rewrites above.
        // INVARIANT: the Option is always Some while the guard is live — this
        // is the only code that calls `.take()`.
        let inner = match guard.0.take() {
            Some(inner) => inner,
            None => panic!("MutexGuard inner was already taken"),
        };
        let (new_guard, result) = match self.0.wait_timeout(inner, timeout) {
            Ok(v) => v,
            Err(poisoned) => {
                log_poison_recovery("Condvar::wait_for");
                poisoned.into_inner()
            }
        };
        guard.0 = Some(new_guard);
        WaitTimeoutResult(result.timed_out())
    }

    /// Wake one waiting thread.
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    /// Wake all waiting threads.
    pub fn notify_all(&self) {
        self.0.notify_all();
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mutex_lock_and_deref() {
        let m = Mutex::new(42);
        let guard = m.lock();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn mutex_lock_mut() {
        let m = Mutex::new(0);
        {
            let mut guard = m.lock();
            *guard = 7;
        }
        assert_eq!(*m.lock(), 7);
    }

    #[test]
    fn mutex_try_lock_succeeds_when_free() {
        let m = Mutex::new(1);
        let guard = m.try_lock();
        assert!(guard.is_some());
        assert_eq!(*guard.unwrap(), 1);
    }

    #[test]
    fn mutex_try_lock_fails_when_held() {
        let m = Mutex::new(1);
        let _guard = m.lock();
        assert!(m.try_lock().is_none());
    }

    #[test]
    fn mutex_get_mut() {
        let mut m = Mutex::new(10);
        *m.get_mut() = 20;
        assert_eq!(*m.lock(), 20);
    }

    #[test]
    fn mutex_into_inner() {
        let m = Mutex::new(99);
        assert_eq!(m.into_inner(), 99);
    }

    #[test]
    fn mutex_recovers_from_poison() {
        let m = Arc::new(Mutex::new(0));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let mut g = m2.lock();
            *g = 7;
            panic!("intentional poison");
        })
        .join();
        // The inner std Mutex is now poisoned, but our wrapper recovers.
        let guard = m.lock();
        assert_eq!(*guard, 7);
    }

    #[test]
    fn condvar_wait_for_timeout() {
        let m = Mutex::new(false);
        let c = Condvar::new();
        let mut guard = m.lock();
        let result = c.wait_for(&mut guard, Duration::from_millis(1));
        assert!(result.timed_out());
    }

    #[test]
    fn condvar_notify_before_timeout() {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let pair2 = Arc::clone(&pair);

        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut guard = pair2.0.lock();
            *guard = true;
            pair2.1.notify_all();
            drop(guard);
        });

        let mut guard = pair.0.lock();
        let start = std::time::Instant::now();
        loop {
            if *guard {
                break;
            }
            let result = pair.1.wait_for(&mut guard, Duration::from_secs(1));
            if result.timed_out() {
                panic!("timed out waiting for notification");
            }
        }
        assert!(*guard);
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn mutex_default() {
        let m: Mutex<i32> = Mutex::default();
        assert_eq!(*m.lock(), 0);
    }

    #[test]
    fn mutex_debug() {
        let m = Mutex::new(42);
        let dbg = format!("{m:?}");
        assert!(dbg.contains("42"), "debug should show value: {dbg}");
    }

    #[test]
    fn guard_debug() {
        let m = Mutex::new(42);
        let guard = m.lock();
        let dbg = format!("{guard:?}");
        assert_eq!(dbg, "42");
    }
}
