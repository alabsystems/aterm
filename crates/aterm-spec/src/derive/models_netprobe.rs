// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Model;

/// Shutdown crossing a worker's already-checked, mutex-held park boundary.
/// Condvar parking atomically releases the mutex; a stop that takes that same
/// mutex must therefore publish after Park. `Buggy=1` is the old atomic-only
/// publisher, whose notification can precede Park and be lost. Tier-1 drives
/// the real NetProbe loop at this boundary and replays both orders.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn netprobe_shutdown_park_model() -> Model {
    crate::ty_model! {
        NetprobeShutdownPark {
            const Buggy = 0;
            var checked = 1;
            var parked = 0;
            var stopped = 0;
            var notified = 0;

            action Park when (checked == 1) {
                checked = 0;
                parked = 1;
            }
            action Stop when (stopped == 0 && (checked == 0 || Buggy == 1)) {
                stopped = 1;
                notified = parked;
            }

            invariant StoppedWaiterHasWake: stopped == 0 || parked == 0 || notified == 1;
        }
    }
}
