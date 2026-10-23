// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the disk walker's budgeted slice lifecycle (`aterm-diskwalk`).

use aterm_spec::derive::*;

/// BudgetedWalk: one folder walk advanced in bounded slices on the walker thread.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn budgeted_walk_model() -> Model {
    aterm_spec::ty_model! {
        BudgetedWalk {
            const Calls = 3;  // listing calls and child opens one walk makes
            const MaxGen = 3; // starts in one trace
            const Buggy = 0;

            var generation = 0;
            var left = 0;
            var done = 0;
            var cancelled = 0;
            var armed = 0;
            var refused = 0;
            var over = 0;
            var resumed_old = 0;
            var relisted = 0;

            action Start when (generation <= MaxGen - 1) {
                generation = generation + 1;
                left = Calls;
                done = 0;
                cancelled = 0;
                armed = 1;
                refused = 0;
            }
            action Slice when (armed == 1 && left > 0) {
                left = left - 1;
                refused = 0;
            }
            action FinalSlice when (armed == 1 && left == 0) {
                done = 1;
                armed = 0;
                refused = 0;
            }
            action StaleSlice when (generation > 1) {
                refused = 1;
            }
            action Cancel when (armed == 1) {
                cancelled = 1;
                armed = 0;
                refused = 0;
            }

            action OverBudget when (Buggy == 1 && armed == 1 && left > 1) {
                left = left - 2;
                over = 1;
            }
            action ResumeOldGeneration when (Buggy == 1 && armed == 1 && generation > 1 &&
                left > 0) {
                left = left - 1;
                resumed_old = 1;
            }
            action TimerAfterCancel when (Buggy == 1 && cancelled == 1 && armed == 0) {
                armed = 1;
            }
            action CursorRestartsDirectory when (Buggy == 1 && armed == 1 &&
                left <= Calls - 1) {
                left = Calls;
                relisted = 1;
            }

            invariant SliceBound: over == 0;
            invariant StaleGenerationNeverResumes: resumed_old == 0;
            invariant CancelLeavesNothingArmed: cancelled == 0 || armed == 0;
            invariant NoDirectoryListedTwice: relisted == 0;
            bound Bounds:
                generation <= MaxGen && left <= Calls && done <= 1 && cancelled <= 1 &&
                armed <= 1 && refused <= 1 && over <= 1 && resumed_old <= 1 &&
                relisted <= 1;
        }
    }
}

/// Liveness property requiring every started walk to eventually finish or cancel.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(budgeted_walk_model)]
pub fn budgeted_walk_liveness() -> Liveness {
    Liveness {
        name: "WalkEnds",
        goal: eq(var("armed"), int(0)),
        weak: vec!["Slice", "FinalSlice"],
        strong: vec![],
        mutants: vec!["CursorRestartsDirectory"],
    }
}
