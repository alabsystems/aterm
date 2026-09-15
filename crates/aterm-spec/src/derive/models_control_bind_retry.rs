// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded recovery after the handoff's one authorized endpoint retirement.

use super::Model;

/// A transient bind failure may retry a vacant path. An occupied or unreadable
/// path cannot enter a stale-path retry that might unlink another listener.
/// Tier-1 drives the genuine retry loop with real native socket bindings.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_handoff_bind_retry_model() -> Model {
    crate::ty_model! {
        NativeUpdateHandoffBindRetry {
            const Buggy = 0;
            const Limit = 3;
            var attempts = 0;
            var failed = 0;
            var occupied = 0;
            var stopped = 0;
            var bound = 0;
            var retried_occupied = 0;

            action Fail when (
                stopped == 0 && bound == 0 && failed == 0 && attempts <= Limit - 1
            ) {
                attempts = attempts + 1;
                failed = 1;
            }
            action Occupy when (bound == 0 && occupied == 0) {
                occupied = 1;
            }
            action Retry when (
                stopped == 0 && failed == 1 && attempts <= Limit - 1 &&
                (occupied == 0 || Buggy == 1)
            ) {
                failed = 0;
                retried_occupied = occupied;
            }
            action Stop when (
                stopped == 0 && failed == 1 && (occupied == 1 || attempts == Limit)
            ) {
                stopped = 1;
            }
            action Bind when (
                stopped == 0 && bound == 0 && failed == 0 && occupied == 0 &&
                attempts <= Limit - 1
            ) {
                attempts = attempts + 1;
                bound = 1;
            }

            invariant BoundedAttempts: attempts <= Limit;
            invariant PreserveOccupiedEndpoint: retried_occupied == 0;
        }
    }
}
