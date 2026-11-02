// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How long an open waits to be admitted (custody, the journal writer, a table's writer)
//! when its caller names no deadline of its own: 2 s in product. In a test every holder an
//! open races is one of the test's own, which releases it, so there the wait is the
//! one-minute hang detector: always in aterm-db's own tests, and in another crate's test
//! binary that asked ([`admission_waits_in_tests!`](crate::admission_waits_in_tests)). That
//! binary's opens reach aterm-db as a plain dependency, whose `cfg(test)` is off, so without
//! the ask a loaded machine that stretches one pass past 2 s refuses it ("application pass
//! deadline expired"), a must-happen step held to a latency budget.

use std::time::{Duration, Instant};

/// The wait where no test binary asked: a product open's 2 s, or the hang detector in
/// aterm-db's own tests.
#[cfg(not(test))]
const UNASKED: Duration = Duration::from_secs(2);
#[cfg(test)]
const UNASKED: Duration = aterm_spec::wait::MUST_HAPPEN;

/// When an open that names no deadline of its own stops waiting to be admitted.
pub(crate) fn deadline() -> Instant {
    #[cfg(any(test, debug_assertions))]
    if let Some(bound) = seam::asked() {
        return Instant::now() + bound;
    }
    Instant::now() + UNASKED
}

/// Make this test binary's aterm-db opens wait `$bound` (the hang detector,
/// `aterm_spec::wait::MUST_HAPPEN`) to be admitted where their callers name no deadline, not
/// a product open's 2 s. A load-time hook, so before any test thread; test builds on Apple
/// targets and Linux.
#[macro_export]
macro_rules! admission_waits_in_tests {
    ($bound:expr) => {
        #[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
        const _: () = {
            extern "C" fn admission_waits() {
                $crate::admission::seam::admit_within($bound);
            }
            #[used]
            #[cfg_attr(
                target_vendor = "apple",
                unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
            )]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
            static ADMISSION_WAITS: extern "C" fn() = admission_waits;
        };
    };
}

/// The test binary's ask ([`admission_waits_in_tests!`](crate::admission_waits_in_tests)):
/// test and debug builds only, so no release build can lengthen a product open's wait.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod seam {
    use std::sync::OnceLock;
    use std::time::Duration;

    static WITHIN: OnceLock<Duration> = OnceLock::new();

    /// Admit this process's opens that name no deadline within `bound` from now on: a test
    /// binary's load-time hook calls it. The first ask stands.
    pub fn admit_within(bound: Duration) {
        let _ = WITHIN.set(bound);
    }

    /// The bound a test binary asked for, if it asked.
    pub(super) fn asked() -> Option<Duration> {
        WITHIN.get().copied()
    }
}
