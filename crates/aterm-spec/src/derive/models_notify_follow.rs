// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A follow-mode notifier checkpoints a fetched page after its records have
//! passed through the serial executor, never when its reader merely offers it.

use super::Model;

/// One fetched page with two matching records and one filtered-out offset.
/// The zero-buffer handoff may accept the second record while its command is
/// still running; the reader may then offer the page checkpoint, but the
/// executor cannot receive that checkpoint until the command finishes.
/// `Buggy=1` writes the page cursor on offer, recreating the skip on a crash.
/// Tier-1 drives the real notifier while its second command is blocked and
/// reads the durable cursor and journal before releasing it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn notify_follow_checkpoint_model() -> Model {
    crate::ty_model! {
        NotifyFollowCheckpoint {
            const Cap = 2;
            const Buggy = 0;
            var received = 0;
            var handled = 0;
            var cursor = 0;
            // 0=no page checkpoint, 1=offered, 2=persisted.
            var checkpoint = 0;

            action ReceiveRecord when (received <= Cap - 1 && received == handled && checkpoint == 0) {
                received = received + 1;
            }
            action HandleRecord when (handled <= received - 1) {
                handled = handled + 1;
                cursor = handled + 1;
            }
            action OfferCheckpoint when (received == Cap && checkpoint == 0) {
                checkpoint = 1;
                cursor = if Buggy == 1 { Cap + 1 } else { cursor };
            }
            action PersistCheckpoint when (checkpoint == 1 && handled == Cap) {
                checkpoint = 2;
                cursor = Cap + 1;
            }

            invariant CursorNeverSkipsUnfinishedRecord:
                if handled == Cap { cursor <= Cap + 1 } else { cursor <= handled };
        }
    }
}
