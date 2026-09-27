// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A queued Fabric outbox item has a prompt event and a bounded lost-event recovery.

use super::Model;

/// `Queue` represents each endpoint producer after it has put a post, receipt,
/// or fetch in the outbox. The bridge drains on its pushed event. If the push
/// stream loses that event, its two-second roster round drains the same durable
/// queue. `Buggy=1` removes the prompt event and the backstop, so the model
/// catches both ways a queued item used to wait indefinitely.
///
/// Tier-1 drives the real endpoint and bridge: a `fetch` event is read from the
/// session timeline, a lost `post` event is fault-injected, and delivery is
/// observed through the recipient's inbox.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_outbox_wake_model() -> Model {
    crate::ty_model! {
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
            // A two-step bounded clock: the first elapsed round leaves the
            // queue owed; the second is the roster's recovery drain.
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

/// A broker outage owns one absolute reconnect deadline. Local `EVENT` rows
/// may wake the mailbox, but only the deadline permits another dial; aterm's
/// inherited-lane close ends the wait immediately. `Buggy=1` is the historical
/// one-shot `mailbox.take(backoff)` behavior: the first event permits a retry.
///
/// The model has no physical clock. Tier-1 supplies the elapsed-time check
/// against the real mailbox helper and projects its event, deadline, and close
/// observations onto these actions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_reconnect_backoff_model() -> Model {
    crate::ty_model! {
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

/// A discovered session keeps its refill debt across a failed broker Fetch.
/// The durable group cursor may already be past its old mail. The subscription
/// can already be live when a new session is discovered, so its newer group
/// record must wait for refill even if the reader has queued it. `Buggy=1`
/// allows the former lost-debt path and premature delivery after a live Fetch
/// failure. Tier-1 drives both timings through the real bridge and broker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_refill_retry_model() -> Model {
    crate::ty_model! {
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
            // A reader opened before discovery may stay open while refill is
            // owed. It may not be treated as ready after that debt disappears
            // until the old record was actually delivered.
            invariant OldBeforeLive:
                live == 0 || discovered == 0 || owed == 1 || old_delivered == 1;
            invariant OldBeforeNew: new_delivered == 0 || old_delivered == 1;
        }
    }
}
