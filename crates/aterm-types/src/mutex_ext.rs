// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Extension trait for `Mutex` that recovers from poisoned locks.

use std::sync::{Mutex, MutexGuard};

/// Extension trait for [`Mutex`] that recovers from poisoned locks.
///
/// Terminal state must remain accessible even if a thread panicked while
/// holding a lock. This trait provides a concise, auditable recovery path
/// replacing the verbose `.lock().unwrap_or_else(|e| e.into_inner())` pattern.
///
/// F11-4 (#7941): recovery is visible. Silent poison recovery hides the fact
/// that some other thread panicked mid-critical-section, which can leave
/// invariants half-established. `lock_or_recover` logs an `error!` line the
/// first time a given lock is observed poisoned so operators see a signal.
pub trait MutexExt<T> {
    /// Acquires the lock, recovering from poison if needed.
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    #[inline]
    #[track_caller]
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        match self.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // F11-4 (#7941): never silently swallow poison. Include the
                // caller location so operators can tell which lock it was.
                //
                // Trust gate note: this calls `aterm_log::__log` directly with a
                // constant message and routes the caller location through the
                // record's `file`/`line` fields, instead of the `error!` macro.
                // The macro's `format_args!` with runtime arguments produces an
                // `fmt::Arguments` constructor the Trust native lowering cannot
                // model; a constant-literal `format_args!` lowers fine. The log
                // record carries the same signal (level, message, caller
                // file/line), and recovery semantics are byte-identical.
                let caller = ::core::panic::Location::caller();
                aterm_log::__log(
                    aterm_log::Level::Error,
                    ::core::module_path!(),
                    ::core::format_args!(
                        "lock_or_recover: Mutex was poisoned — recovering inner value; \
                         another thread panicked mid-critical-section and invariants \
                         may be partial (see this record's file:line for the caller)"
                    ),
                    ::core::option::Option::Some(caller.file()),
                    ::core::option::Option::Some(caller.line()),
                );
                poisoned.into_inner()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_lock_or_recover_normal_access() {
        let m = Mutex::new(42);
        let guard = m.lock_or_recover();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_lock_or_recover_after_poison() {
        let m = Arc::new(Mutex::new(0));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let mut g = m2.lock().expect("lock for poison");
            *g = 7;
            panic!("intentional poison");
        })
        .join();
        // Mutex is now poisoned; lock_or_recover should still work.
        let guard = m.lock_or_recover();
        assert_eq!(*guard, 7);
    }
}
