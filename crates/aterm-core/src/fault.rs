// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Deterministic fault injection (M7 FAULT-INJECT).
//!
//! A thread-local registry of armed fault points so tests can force the rare
//! error / cleanup branches that normal input cannot reach — an allocation that
//! fails, an I/O write that errors — and assert the fail-closed path actually
//! fails closed (degrades gracefully, never panics or corrupts state).
//!
//! Production builds pay ZERO: [`triggered`] is a `#[inline]` `const false` the
//! optimizer folds away at every call site. The registry + `arm`/`disarm` exist
//! only under `cfg(test)` or the opt-in `fault-injection` feature.
//!
//! Usage at a fail-closed seam:
//! ```ignore
//! if aterm_core::fault::triggered("scrollback.disk_spill") || real_op().is_err() {
//!     // graceful degradation (drop / skip / fail closed) — never panic
//! }
//! ```
//! In a test: `fault::arm("scrollback.disk_spill"); … assert graceful …;
//! fault::disarm("scrollback.disk_spill");`

#[cfg(any(test, feature = "fault-injection"))]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashSet;

    thread_local! {
        static ARMED: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
    }

    /// Arm `name` so the next [`triggered`] at that point returns `true`.
    pub fn arm(name: &'static str) {
        ARMED.with(|a| {
            a.borrow_mut().insert(name);
        });
    }

    /// Disarm `name` (tests should disarm after exercising the path).
    pub fn disarm(name: &'static str) {
        ARMED.with(|a| {
            a.borrow_mut().remove(name);
        });
    }

    /// Whether the fault point `name` is currently armed.
    #[must_use]
    pub fn triggered(name: &'static str) -> bool {
        ARMED.with(|a| a.borrow().contains(name))
    }

    /// Run `f` with `name` armed, disarming afterwards even on unwind. Keeps a
    /// test's fault scoped so it cannot leak into later tests on the same thread.
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

#[cfg(not(any(test, feature = "fault-injection")))]
mod imp {
    /// Production: no fault is ever armed; folded to `false` and inlined away.
    #[inline]
    #[must_use]
    pub fn triggered(_name: &'static str) -> bool {
        false
    }
}

pub use imp::triggered;
#[cfg(any(test, feature = "fault-injection"))]
pub use imp::{arm, disarm, with_armed};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unarmed_is_false_armed_is_true() {
        assert!(!triggered("nope"));
        arm("x");
        assert!(triggered("x"));
        assert!(!triggered("y"));
        disarm("x");
        assert!(!triggered("x"));
    }

    #[test]
    fn with_armed_scopes_and_disarms() {
        assert!(with_armed("scoped", || triggered("scoped")));
        assert!(!triggered("scoped"), "disarmed after the scope");
    }
}
