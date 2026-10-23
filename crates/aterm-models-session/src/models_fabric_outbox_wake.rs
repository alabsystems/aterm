// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A queued Fabric outbox item has a prompt event and a bounded lost-event recovery.

use aterm_spec::derive::Model;

/// Formal model for `FabricOutboxWake`: prompt outbox event drain and bounded roster backstop recovery.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_outbox_wake_model() -> Model {
    aterm_spec::ty_model! {
        FabricOutboxWake {
            const Buggy = 0;
            var queued = 0;
            var event = 0;
            var lost = 0;
            var ticks = 0;
            var delivered = 0;

            action Queue when (queued == 0 && delivered == 0) {
                queued = 1;
                event = if Buggy == 1 { 0 } else { 1 };
            }
            action LoseEvent when (queued == 1 && event == 1) {
                event = 0;
                lost = 1;
            }
            action PromptDrain when (queued == 1 && event == 1) {
                queued = 0;
                event = 0;
                delivered = 1;
            }
            action BackstopTick when (queued == 1 && event == 0 && ticks <= 1) {
                ticks = ticks + 1;
                queued = if ticks == 1 && Buggy == 0 { 0 } else { queued };
                delivered = if ticks == 1 && Buggy == 0 { 1 } else { delivered };
            }

            invariant PromptAvailable: queued == 0 || lost == 1 || event == 1;
            invariant BackstopBounded: queued == 0 || ticks <= 1;
        }
    }
}

/// Formal model for `FabricReconnectBackoff`: reconnect retries require the absolute backoff deadline to expire.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_reconnect_backoff_model() -> Model {
    aterm_spec::ty_model! {
        FabricReconnectBackoff {
            const Buggy = 0;
            var events = 0;
            var expired = 0;
            // 0=waiting, 1=retry, 2=aterm closed.
            var result = 0;

            action Event when (result == 0 && events <= 1) {
                events = events + 1;
                result = if Buggy == 1 { 1 } else { 0 };
            }
            action Deadline when (result == 0) {
                expired = 1;
                result = 1;
            }
            action Closed when (result == 0) {
                result = 2;
            }

            invariant RetryNeedsDeadline: result == 0 || result == 2 || expired == 1;
        }
    }
}

/// Formal model for `FabricRefillRetry`: discovered sessions retain refill debt across failed broker fetches before live delivery.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_refill_retry_model() -> Model {
    aterm_spec::ty_model! {
        FabricRefillRetry {
            const Buggy = 0;
            var discovered = 0;
            var owed = 0;
            var connected = 0;
            var failures = 0;
            var old_delivered = 0;
            var live = 0;
            var new_delivered = 0;

            action Discover when (discovered == 0) {
                discovered = 1;
                owed = 1;
            }
            action Attach when (connected == 0) {
                connected = 1;
            }
            action FetchFails when (connected == 1 && owed == 1 && failures == 0) {
                connected = 0;
                failures = 1;
                owed = if Buggy == 1 { 0 } else { 1 };
            }
            action RefillCompletes when (connected == 1 && owed == 1) {
                old_delivered = 1;
                owed = 0;
            }
            action Subscribe when (connected == 1 && owed == 0 && live == 0) {
                live = 1;
            }
            action DeliverNew when (live == 1 && discovered == 1 && new_delivered == 0
                && (Buggy == 1 || (connected == 1 && owed == 0))) {
                new_delivered = 1;
            }

            invariant RefillDebtSurvivesFailure:
                discovered == 0 || old_delivered == 1 || owed == 1;
            invariant OldBeforeLive:
                live == 0 || discovered == 0 || owed == 1 || old_delivered == 1;
            invariant OldBeforeNew: new_delivered == 0 || old_delivered == 1;
        }
    }
}
