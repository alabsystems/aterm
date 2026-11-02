// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Only current root-addressed requests consume recovery dispatch slots.
/// Completed requests become quiet; unrelated notices cannot fill the queue.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recovery_dispatch_model() -> Model {
    aterm_spec::ty_model! {
        DbRecoveryCurrentDispatch {
            const Buggy = 0;
            var phase = 0; var issued = 0; var generation = 1; var due = 0;
            action Issue when (phase == 0) {
                phase = 1; issued = 1; generation = 2; due = 1;
            }
            action Repeat when (phase == 1) {
                phase = 2; issued = if Buggy == 1 {1} else {0}; due = 0;
            }
            action Successor when (phase == 2) {
                phase = 3; issued = 1; generation = if Buggy == 5 {2} else {3}; due = 1;
            }
            action Quiet when (phase == 3) {
                phase = 4; issued = if Buggy == 1 {1} else {0}; due = 0;
            }
            action Flood when (phase == 0) {
                phase = 5; issued = if Buggy == 2 {0} else {1};
                generation = 2; due = 1;
            }
            action Stop when (phase == 0) {
                phase = 6; issued = 0; due = if Buggy == 3 {1} else {0};
            }
            action StopDevice when (phase == 0) {
                phase = 8; issued = 0; generation = 2; due = if Buggy == 3 {1} else {0};
            }
            action Unknown when (phase == 0) {
                phase = 7; issued = 0; due = if Buggy == 4 {1} else {0};
            }
            invariant ActualWorkOnly: issued == if phase == 1 || phase == 3 || phase == 5 {1} else {0};
            invariant CurrentSlotsOnly: due == if phase == 1 || phase == 3 || phase == 5 {1} else {0};
            invariant RootAdvances: generation == if phase == 3 || phase == 4 {3} else if phase == 1 || phase == 2 || phase == 5 || phase == 8 {2} else {1};
        }
    }
}
