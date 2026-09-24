// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A new broadcast `since=head` cursor must use the broker's head, even while
//! the bridge's lower-priority say reader still owes an earlier record.

use super::Model;

/// `PublishBacklog` is acknowledged by the broker before the opt-in; the bridge
/// may or may not have run `ObserveBacklog` when it handles the topic event.
/// `AddHead` resolves once, and `TakeBacklog` models the pending say record.
/// `Buggy=1` replays the old bridge decision from its stale observed head,
/// making the backlog appear in a session that asked to start at head. Tier-1
/// projects the real bridge's durable topic cursor and delivered ring rows.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broadcast_head_subscription_model() -> Model {
    crate::ty_model! {
        BroadcastHeadSubscription {
            const Buggy = 0;
            var broker_head = 0;
            var observed_head = 0;
            var cursor = 0;
            var subscribed = 0;
            var backlog_delivered = 0;

            action PublishBacklog when (broker_head == 0 && subscribed == 0) {
                broker_head = 1;
            }
            action ObserveBacklog when (broker_head == 1 && observed_head == 0) {
                observed_head = 1;
            }
            action AddHead when (broker_head == 1 && subscribed == 0) {
                cursor = if Buggy == 1 { observed_head } else { broker_head };
                subscribed = 1;
            }
            action TakeBacklog when (
                broker_head == 1 && subscribed == 1 && observed_head == 0
            ) {
                backlog_delivered = if cursor == 0 { 1 } else { 0 };
                observed_head = 1;
            }

            invariant HeadSkipsEarlierRecord: backlog_delivered == 0;
        }
    }
}
