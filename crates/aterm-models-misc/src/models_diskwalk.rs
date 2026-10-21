// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The disk walker's slice lifecycle (`aterm-diskwalk`'s `Walker`,
//! docs/DESIGN-system-awareness-2026-10-16.md §17 BudgetedWalk).

use aterm_spec::derive::*;

/// BudgetedWalk: one folder walk advanced in bounded slices on the walker thread.
///
/// `Walker::start` mints a generation and arms; `Walker::slice` advances the walk in flight by
/// at most its budget (a slice reads its clock after every listing call and child open and
/// returns at the first boundary at or past the budget); `Walker::cancel` disarms. `left` is the
/// listing calls the walk in flight still owes: under the Tier-1's clock, which charges a whole
/// budget per reading, each `Slice` makes exactly one call, and the `FinalSlice` makes none and
/// answers done. A ticket from a superseded generation is refused (`StaleSlice`) and walks
/// nothing.
///
/// Laws, each caught by its `Buggy = 1` action:
/// * `SliceBound`: no slice runs a call past its budget (`OverBudget`);
/// * `StaleGenerationNeverResumes`: a superseded ticket never advances a walk
///   (`ResumeOldGeneration`);
/// * `CancelLeavesNothingArmed`: after a cancel nothing owes the walk a slice until the next
///   start (`TimerAfterCancel`, a host timer that survives the cancel);
/// * `NoDirectoryListedTwice`: the cursor keeps each listing's position, so no directory is
///   listed again (`CursorRestartsDirectory`, which also breaks [`budgeted_walk_liveness`]:
///   a walk that restarts a directory each slice never ends).
///
/// `Calls = 3` stands for any walk; the Tier-1 (`aterm-diskwalk/tests/
/// conformance_budgeted_walk.rs`) overrides it with the real walk's call count.
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

            // A start supersedes the walk in flight (armed or ended) and owes the new one
            // its slices.
            action Start when (generation <= MaxGen - 1) {
                generation = generation + 1;
                left = Calls;
                done = 0;
                cancelled = 0;
                armed = 1;
                refused = 0;
            }
            // One call inside the budget; the walk owes more.
            action Slice when (armed == 1 && left > 0) {
                left = left - 1;
                refused = 0;
            }
            // The walk's end: nothing left to call, done, disarmed.
            action FinalSlice when (armed == 1 && left == 0) {
                done = 1;
                armed = 0;
                refused = 0;
            }
            // A superseded ticket walks nothing.
            action StaleSlice when (generation > 1) {
                refused = 1;
            }
            action Cancel when (armed == 1) {
                cancelled = 1;
                armed = 0;
                refused = 0;
            }

            // Mutant: the slice makes a second call after its clock said the budget is spent.
            action OverBudget when (Buggy == 1 && armed == 1 && left > 1) {
                left = left - 2;
                over = 1;
            }
            // Mutant: a superseded ticket advances the walk in flight.
            action ResumeOldGeneration when (Buggy == 1 && armed == 1 && generation > 1 &&
                left > 0) {
                left = left - 1;
                resumed_old = 1;
            }
            // Mutant: a host timer the cancel did not disarm.
            action TimerAfterCancel when (Buggy == 1 && cancelled == 1 && armed == 0) {
                armed = 1;
            }
            // Mutant: a slice reopens the directory it was listing and starts it over.
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

/// Every walk ends: a walk that is started is eventually done or cancelled, given that a slice
/// owed is eventually taken (weak fairness on `Slice` and `FinalSlice`). Caught by
/// `CursorRestartsDirectory`, a cursor that restarts a directory each slice.
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
