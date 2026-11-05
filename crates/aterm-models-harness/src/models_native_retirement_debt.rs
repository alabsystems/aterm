// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The client's durable hold preserves debt; it is never authority for a successor.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_retirement_debt_model() -> Model {
    aterm_spec::ty_model! {
        NativeRetirementDebt {
            const Buggy = 0;
            var held = 0; var observed = 0; var kept = 1;
            var owed = 1; var dispatched = 0; var finished = 0;
            action NotGranted when (observed == 0 && finished == 0) {
                held = 1; observed = 1; kept = if Buggy == 1 {0} else {1};
            }
            action Unknown when (observed == 0 && finished == 0) {
                held = if Buggy == 7 {1} else {0};
            }
            action Exited when (observed == 0 && finished == 0) {
                finished = 1; owed = 0; kept = 0;
            }
            action Completed when (observed == 0 && finished == 0) {
                finished = 1; owed = 0; kept = 0;
            }
            action Replay when (held == 1) { held = if Buggy == 2 {0} else {1}; }
            action Reload when (held == 1) { held = if Buggy == 3 {0} else {1}; }
            action Retarget when (held == 1) { kept = if Buggy == 4 {0} else {1}; }
            action Expire when (held == 1) { owed = if Buggy == 5 {0} else {1}; }
            action Rollback when (held == 1) { dispatched = if Buggy == 6 {1} else {0}; }
            invariant PositiveObservation: held == 0 || observed == 1;
            invariant NoRevival: observed == 0 || held == 1;
            invariant RecipeRetained: observed == 0 || kept == 1;
            invariant DebtRetained: observed == 0 || owed == 1;
            invariant NoRollback: observed == 0 || dispatched == 0;
            bound Finite: 0 <= held && held <= 1 && 0 <= observed && observed <= 1
                && 0 <= kept && kept <= 1 && 0 <= owed && owed <= 1
                && 0 <= dispatched && dispatched <= 1 && 0 <= finished && finished <= 1;
        }
    }
}
