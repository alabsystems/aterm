// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The client's durable hold preserves debt; it is never authority for a successor.
/// A record whose admission reply was lost keeps only its wire (`bound` 0); the host's
/// positive unadmitted answer clears that wire alone, never a binding or a hold.
/// The host's lapsed answer (no exec grant was or can be issued under the prepare, the
/// `NativeHistoricalClosure` model) clears the wire and its binding, bound or not, but never
/// a hold; the debt stays for the fresh plan that pays it. The host answers it only for a cold
/// or typed asked restart, which the client checks again.
/// A typed asked recipe has the same debt boundary; its passive history is not a new grant.
/// The host's closed answer (the restart reached its close uncommitted, its exec grant may have
/// run) clears the wire and its binding like a lapse, and leaves a watch in their place: the
/// record plans nothing afresh until it adopts what runs or sees nothing run.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_retirement_debt_model() -> Model {
    aterm_spec::ty_model! {
        NativeRetirementDebt {
            const Buggy = 0;
            var held = 0; var observed = 0; var kept = 1;
            var owed = 1; var dispatched = 0; var finished = 0; var bound = 1; var asked = 0;
            var closed = 0; var watch = 0;
            action Ask when (asked == 0 && kept == 1 && observed == 0 && finished == 0) { asked = 1; }
            action NotGranted when (observed == 0 && finished == 0 && kept == 1) {
                held = 1; observed = 1; kept = if Buggy == 1 {0} else {1};
            }
            action Unknown when (observed == 0 && finished == 0) {
                held = if Buggy == 7 {1} else {0};
            }
            action Exited when (observed == 0 && finished == 0 && kept == 1) {
                finished = 1; owed = 0; kept = 0; bound = 0;
            }
            action Completed when (observed == 0 && finished == 0 && kept == 1) {
                finished = 1; owed = 0; kept = 0; bound = 0;
            }
            action Replay when (held == 1) { held = if Buggy == 2 {0} else {1}; }
            action Reload when (held == 1) { held = if Buggy == 3 {0} else {1}; }
            action Retarget when (held == 1) { kept = if Buggy == 4 {0} else {1}; }
            action Expire when (held == 1) { owed = if Buggy == 5 {0} else {1}; }
            action Rollback when (held == 1) { dispatched = if Buggy == 6 {1} else {0}; }
            action LoseAdmission when (bound == 1 && observed == 0 && finished == 0) { bound = 0; }
            action Unadmitted when (finished == 0) {
                kept = if (bound == 0 && held == 0) || Buggy == 8 {0} else {kept};
                held = if Buggy == 9 {0} else {held};
            }
            action Lapsed when ((observed == 0 || Buggy == 10) && finished == 0 && kept == 1) {
                kept = 0; bound = 0;
            }
            action Closed when (observed == 0 && finished == 0 && kept == 1) {
                kept = 0; bound = 0; closed = 1; watch = if Buggy == 11 {0} else {1};
            }
            invariant PositiveObservation: held == 0 || observed == 1;
            invariant NoRevival: observed == 0 || held == 1;
            invariant RecipeRetained: observed == 0 || kept == 1;
            invariant DebtRetained: observed == 0 || owed == 1;
            invariant NoRollback: observed == 0 || dispatched == 0;
            invariant BindingRetained: bound == 0 || kept == 1;
            invariant ClosedLeavesAWatch: closed == 0 || watch == 1;
            bound Finite: 0 <= held && held <= 1 && 0 <= observed && observed <= 1
                && 0 <= kept && kept <= 1 && 0 <= owed && owed <= 1
                && 0 <= dispatched && dispatched <= 1 && 0 <= finished && finished <= 1
                && 0 <= bound && bound <= 1 && 0 <= asked && asked <= 1
                && 0 <= closed && closed <= 1 && 0 <= watch && watch <= 1;
        }
    }
}
