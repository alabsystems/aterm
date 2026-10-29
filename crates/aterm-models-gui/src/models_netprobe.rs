// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A wake crossing a worker's already-checked, mutex-held park boundary: shutdown (`Stop`), or
/// the probe a network path's return owes (`Nudge`). Condvar parking releases the mutex
/// atomically, so a publisher that takes that mutex publishes after Park and its notification
/// reaches the parked worker. `Buggy=1` publishes the stop through an atomic alone, and
/// `Buggy=2` the nudge outside the mutex, so that notification can precede Park and be lost.
/// Tier-1 drives the real NetProbe loop at this boundary and replays both orders of each.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn netprobe_shutdown_park_model() -> Model {
    aterm_spec::ty_model! {
        NetprobeShutdownPark {
            const Buggy = 0;
            var checked = 1;
            var parked = 0;
            var stopped = 0;
            var notified = 0;
            var nudged = 0;
            var woken = 0;

            action Park when (checked == 1) {
                checked = 0;
                parked = 1;
            }
            action Stop when (stopped == 0 && (checked == 0 || Buggy == 1)) {
                stopped = 1;
                notified = parked;
            }
            action Nudge when (nudged == 0 && (checked == 0 || Buggy == 2)) {
                nudged = 1;
                woken = parked;
            }

            invariant StoppedWaiterHasWake: stopped == 0 || parked == 0 || notified == 1;
            invariant NudgedWaiterHasWake: nudged == 0 || parked == 0 || woken == 1;
        }
    }
}
