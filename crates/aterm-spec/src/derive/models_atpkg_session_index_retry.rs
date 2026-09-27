// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A headless session's same-build retry ladder after a published HEAD woke
//! a whole pass. Each tick is five minutes; a newer build bypasses the wait.

use super::*;

/// One build's first, second and later passes. `Buggy=1` records the old
/// repeated-pass behavior when a same-build HEAD arrives before its retry
/// deadline. Tier-1 checks `IndexAttempt::holds` at each real boundary.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_session_index_retry_model() -> Model {
    crate::ty_model! {
        AtpkgSessionIndexRetry {
            const Buggy = 0;
            var passes = 0;
            var age = 0;
            var newer = 0;
            var premature = 0;

            action First when (passes == 0) {
                passes = 1;
            }
            action Tick when (passes > 0 && age <= 11) {
                age = age + 1;
            }
            action EarlySame when (
                passes > 0 && age + 1 <=
                (if passes == 1 { 1 } else if passes == 2 { 3 } else { 12 })
            ) {
                premature = if Buggy == 1 { 1 } else { premature };
            }
            action RetryFirst when (passes == 1 && age > 0) {
                passes = 2;
                age = 0;
            }
            action RetrySecond when (passes == 2 && age > 2) {
                passes = 3;
                age = 0;
            }
            action RetryLater when (passes == 3 && age > 11) {
                age = 0;
            }
            action NewerHint when (passes > 0 && newer == 0) {
                newer = 1;
                passes = 1;
                age = 0;
            }

            invariant NoPrematureSameBuildPass: premature == 0;
            invariant BoundedRetry: passes <= 3;
        }
    }
}
