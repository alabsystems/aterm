// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Extension trait for `Mutex` that recovers from poisoned locks.

use std::sync::{Mutex, MutexGuard};

/// Extension trait for [`Mutex`] that recovers from poisoned locks, so terminal state stays
/// reachable after a thread panicked holding one. Recovery is never silent: every poisoned
/// acquire logs an error with the caller's location, since the panicking thread may have left
/// invariants half-established.
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
                // Never silent: log with the caller's location. `aterm_log::__log` takes a
                // constant message and the caller in the record's file/line, not `error!`: a
                // runtime-argument `format_args!` cannot be lowered by Trust, a constant one can.
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
