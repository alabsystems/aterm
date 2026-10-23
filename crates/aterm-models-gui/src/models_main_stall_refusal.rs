// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model for refusing main-thread control verbs only while the main thread is stalled.

use aterm_spec::derive::Model;

/// Returns the formal model for main-thread stall refusal.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn main_thread_stall_refusal_model() -> Model {
    aterm_spec::ty_model! {
        MainThreadStallRefusal {
            const Buggy = 0;
            const Bar = 2;
            var root = 2;
            var quiet = 0;
            var stamp = 0;
            var stamp_age = 0;
            var hop = 0;
            var waiting = 0;
            var waited = 0;
            var refused = 0;
            var stalled = 0;
            var census = 0;
            var beat_only = 0;
            var until_reply = 0;
            var mutated = 0;

            action MutateCensusStamp when (Buggy == 1 && mutated == 0) {
                census = 1;
                mutated = 1;
            }
            action MutateHeartbeatOnly when (Buggy == 1 && mutated == 0) {
                beat_only = 1;
                mutated = 1;
            }
            action MutateCountsUntilReply when (Buggy == 1 && mutated == 0) {
                until_reply = 1;
                mutated = 1;
            }
            action BeatIdle {
                root = 0;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
            }
            action BeatWork {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
            }
            action BeatPark {
                root = 2;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
            }
            action Tick {
                quiet = if quiet <= Bar - 1 { quiet + 1 } else { quiet };
                stamp_age = if stamp_age <= Bar - 1 { stamp_age + 1 } else { stamp_age };
                waited = if waiting == 1 && waited <= Bar - 1 { waited + 1 } else { waited };
            }
            action Post when (waiting == 0) {
                hop = 1;
                waiting = 1;
                waited = 0;
            }
            action Answer when (hop == 1 && root <= 1) {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
                hop = 0;
                waiting = 0;
                waited = 0;
            }
            action Take when (hop == 1 && root <= 1) {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
                hop = 0;
            }
            action Reply when (hop == 0 && waiting == 1 && root <= 1) {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
                waiting = 0;
                waited = 0;
            }
            action GiveUp when (waiting == 1) {
                hop = 0;
                waiting = 0;
                waited = 0;
            }
            action Reset {
                stamp = if census == 1 { 0 } else { stamp };
            }
            action Probe {
                refused = if stamp == 1 && stamp_age == Bar && (
                    root == 1 || (
                        root == 0
                            && (hop == 1 || (until_reply == 1 && waiting == 1))
                            && (waited == Bar || beat_only == 1)
                    )
                ) { 1 } else { 0 };
                stalled = if quiet == Bar && (
                    root == 1 || (root == 0 && hop == 1 && waited == Bar)
                ) { 1 } else { 0 };
            }

            invariant NeverRefusesAMovingMainThread: refused <= stalled;
            invariant RefusesAStalledMainThread: stalled <= refused;
        }
    }
}
