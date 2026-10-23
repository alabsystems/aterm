// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Extension trait for `Mutex` that recovers from poisoned locks.

use std::sync::{Mutex, MutexGuard};

/// Extension trait for [`Mutex`] that logs and recovers from poisoned locks.
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
    fn test_lock_or_recover_after_poison() {
        let m = Arc::new(Mutex::new(0));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let mut g = m2.lock().expect("lock for poison");
            *g = 7;
            panic!("intentional poison");
        })
        .join();
        let guard = m.lock_or_recover();
        assert_eq!(*guard, 7);
    }
}
