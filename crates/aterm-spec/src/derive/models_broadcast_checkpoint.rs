// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Irrelevant broadcast cursor movement may checkpoint late, but a delivered
//! record must commit its topic cursor before a bridge restart can replay it.

use super::Model;

/// One subscribed topic over four broadcast records. `Irrelevant` is the live
/// frontier sweep, `Checkpoint` is its optional sharded state-file write,
/// `Deliver` is the accounted delivery followed by the immediate state-file
/// write, and `Crash` reloads that file. `Buggy=1` removes the delivered write:
/// a crash then re-offers a record the endpoint already received.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broadcast_cursor_checkpoint_model() -> Model {
    crate::ty_model! {
        BroadcastCursorCheckpoint {
            const Cap = 4;
            const Buggy = 0;
            var head = 0;
            var memory = 0;
            var durable = 0;
            var delivered = 0;
            var reoffered = 0;

            action Irrelevant when (memory == head && head <= Cap - 1) {
                head = head + 1;
                memory = memory + 1;
            }
            action Checkpoint when (memory > durable) {
                durable = memory;
            }
            action Deliver when (memory == head && head <= Cap - 1) {
                head = head + 1;
                memory = memory + 1;
                durable = if Buggy == 1 { durable } else { memory + 1 };
                delivered = memory + 1;
            }
            action Crash when (head > 0) {
                reoffered = if delivered > durable { 1 } else { 0 };
                memory = durable;
            }
            action ReplayIrrelevant when (memory <= head - 1) {
                memory = memory + 1;
            }

            invariant NoDeliveredReplay: reoffered == 0;
        }
    }
}
