// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Deterministic fault injection: a thread-local registry of armed fault points, so tests can
//! force rare error branches and assert the path fails closed without panicking.
//!
//! Without `cfg(test)` or the `fault-injection` feature, [`triggered`](crate::fault::triggered) is an inlined `false`
//! and `arm`/`disarm` do not exist.
//!
//! Production builds pay nothing: [`triggered`](crate::fault::triggered) is an `#[inline]` `const
//! false`. The registry and `arm`/`disarm` exist only in a test or debug build,
//! where nothing arms a point but a test.
//!
//! Usage at a fail-closed seam:
//! ```
//! # fn real_op() -> Result<(), std::io::Error> { Ok(()) }
//! if aterm_core::fault::triggered("scrollback.cold_push") || real_op().is_err() {
//!     // graceful degradation (drop / skip / fail closed) — never panic
//! }
//! ```
//! In a test: `fault::arm("scrollback.cold_push"); … assert graceful …;
//! fault::disarm("scrollback.cold_push");`

#[cfg(any(test, debug_assertions))]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashSet;

    thread_local! {
        static ARMED: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
    }

    /// Arm `name` so the next [`triggered`] at that point returns `true`.
    #[inline]
    pub fn arm(name: &'static str) {
        ARMED.with(|a| {
            a.borrow_mut().insert(name);
        });
    }

    /// Disarm `name` (tests should disarm after exercising the path).
    #[inline]
    pub fn disarm(name: &'static str) {
        ARMED.with(|a| {
            a.borrow_mut().remove(name);
        });
    }

    /// Whether the fault point `name` is currently armed.
    #[must_use]
    #[inline]
    pub fn triggered(name: &'static str) -> bool {
        ARMED.with(|a| a.borrow().contains(name))
    }

    /// Run `f` with `name` armed, disarming afterwards even on unwind. Keeps a
    /// test's fault scoped so it cannot leak into later tests on the same thread.
    #[inline]
    pub fn with_armed<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
        struct Guard(&'static str);
        impl Drop for Guard {
            fn drop(&mut self) {
                disarm(self.0);
            }
        }
        arm(name);
        let _g = Guard(name);
        f()
    }
}

#[cfg(not(any(test, debug_assertions)))]
mod imp {
    /// Production: no fault is ever armed; folded to `false` and inlined away.
    #[inline]
    #[must_use]
    pub const fn triggered(_name: &'static str) -> bool {
        false
    }
}

pub use imp::triggered;
#[cfg(any(test, debug_assertions))]
pub use imp::{arm, disarm, with_armed};
