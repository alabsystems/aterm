// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broadcast cursor checkpointing model for topic delivery and crash recovery.

use aterm_spec::derive::Model;

/// Formal model of broadcast topic cursor checkpointing across delivery and crash recovery.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broadcast_cursor_checkpoint_model() -> Model {
    aterm_spec::ty_model! {
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
