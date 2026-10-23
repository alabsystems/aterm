// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of `WatcherSet::is_advance` across alternate-screen re-entry.

use aterm_spec::derive::Model;

/// `WatcherSet::is_advance` across alternate-screen re-entry (`epoch`, `seq`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn observation_screen_generation_model() -> Model {
    aterm_spec::ty_model! {
        ObservationScreenGeneration {
            const Buggy = 0;
            const SeqCap = 3;
            const EpochCap = 2;
            var seq = 1;
            var epoch = 0;
            var seen_seq = 1;
            var seen_epoch = 0;
            var dirty = 0;
            var missed = 0;

            action Write when (seq <= SeqCap - 1) {
                seq = seq + 1;
                dirty = 1;
            }
            action Reenter when (epoch <= EpochCap - 1) {
                epoch = epoch + 1;
                seq = 1;
                dirty = 1;
            }
            action Observe when (dirty == 1) {
                missed = if (seq > seen_seq || (Buggy == 0 && epoch > seen_epoch)) {
                    missed
                } else {
                    1
                };
                seen_seq = if (seq > seen_seq || (Buggy == 0 && epoch > seen_epoch)) {
                    seq
                } else {
                    seen_seq
                };
                seen_epoch = if (Buggy == 0 && epoch > seen_epoch) { epoch } else { seen_epoch };
                dirty = 0;
            }

            invariant EveryChangeIsSeen: missed == 0;
        }
    }
}
