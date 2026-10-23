// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update handoff's window show state.

use aterm_spec::derive::Model;

/// One restore's carried show state in the process that restored it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_window_show_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateWindowShow {
            const Buggy = 0;
            var lane = 0;
            var revealed = 0;
            var proof = 0;
            var stacked = 0;
            var keyed = 0;
            var typed = 0;
            var committed = 0;
            var front = 0;
            var fs = 0;
            var fsfront = 0;

            action Launch when (lane == 0) {
                lane = 1;
            }
            action ColdLaunch when (lane == 0) {
                lane = 2;
                committed = 1;
            }
            action Reveal when (lane > 0 && revealed == 0) {
                revealed = 1;
            }
            action Prove when (lane == 1 && revealed == 1 && proof == 0) {
                proof = 1;
            }
            action Stack when (
                lane > 0
                    && stacked == 0
                    && ((revealed == 1 && (lane == 2 || proof == 1)) || Buggy == 1)
            ) {
                stacked = 1;
                keyed = if typed == 1 || Buggy == 1 { 0 } else { 1 };
            }
            action Activate when (front == 0) {
                front = 1;
            }
            action Deactivate when (front == 1) {
                front = 0;
            }
            action Steal when (lane == 1 && committed == 0 && stacked == 1 && keyed == 1) {
                keyed = 0;
            }
            action Type when (lane == 1 && committed == 0 && revealed == 1 && keyed == 0) {
                typed = 1;
            }
            action Commit when (lane == 1 && committed == 0 && proof == 1) {
                committed = 1;
                keyed = if stacked == 1 && (typed == 0 || Buggy == 1) { 1 } else { keyed };
            }
            action EnterFullScreen when (
                stacked == 1
                    && fs == 0
                    && (committed == 1 || Buggy == 1)
                    && (front == 1 || Buggy == 1)
            ) {
                fs = 1;
                fsfront = front;
            }

            invariant StackOnlyOnGlass: stacked == 0 || revealed == 1;
            invariant StackOnlyAtProof: lane == 2 || stacked == 0 || proof == 1;
            invariant FullScreenOnlyAfterCommit: fs == 0 || committed == 1;
            invariant FullScreenOnlyInFront: fs == 0 || fsfront == 1;
            invariant KeyWindowAtCommit:
                committed == 0 || stacked == 0 || typed == 1 || keyed == 1;
            invariant TypingKeepsItsWindow: committed == 0 || typed == 0 || keyed == 0;
        }
    }
}
