// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Qualify's incomplete-history slice learning admission and compiler-token escape.

use aterm_spec::derive::*;

/// THE QUALIFY LEARNERS CANNOT WEDGE BEHIND THEIR OWN COMPILER-TOKEN BARRIER. When retained unit
/// history is incomplete, the native slice first holds an exclusive driver-sized learning claim
/// and the compat slice queues its own exclusive claim behind it. That exact held-plus-queued
/// observation releases every non-export compiler token, lets native finish and return its
/// claim, then lets compat hold its claim and finish. The tree starts only after both learning
/// slices have ended.
///
/// `KeepBarrier` is the old detector's negative control: it observes the serial state but retains
/// every compiler token. Native cannot finish without one, compat cannot admit before native
/// finishes, and the graph wedges. `TreeStartsEarly` is the independent negative control for
/// starting tree work while either exclusive learning claim remains live or queued.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn qualify_fallback_admission_model() -> Model {
    aterm_spec::ty_model! {
        QualifyFallbackAdmission {
            const Buggy = 0;

            // Native: 0 before admission, 1 claim held, 2 slice complete and claim returned.
            var native = 0;
            // Compat: 0 before request, 1 claim queued, 2 claim held, 3 slice complete.
            var compat = 0;
            // Every non-export compiler token: 0 retained by admission, 1 released.
            var gate = 0;
            // Admission decision: 0 pending, 1 bounded serial escape, 2 wedged mutant.
            var decision = 0;
            // The tree remains outside the learners: 0 waiting, 1 started after both.
            var tree = 0;

            action NativeParentHolds when (native == 0) {
                native = 1;
            }
            action CompatParentQueues when (native == 1 && compat == 0) {
                compat = 1;
            }
            action SerialEscape when (native == 1 && compat == 1 && gate == 0
                && decision == 0) {
                gate = 1;
                decision = 1;
            }
            action NativeSliceEnds when (native == 1 && compat == 1 && gate == 1) {
                native = 2;
            }
            action CompatParentHolds when (native == 2 && compat == 1 && gate == 1) {
                compat = 2;
            }
            action CompatSliceEnds when (native == 2 && compat == 2 && gate == 1) {
                compat = 3;
            }
            action TreeStarts when (native == 2 && compat == 3 && gate == 1 && tree == 0) {
                tree = 1;
            }

            action KeepBarrier when (Buggy == 1 && native == 1 && compat == 1
                && gate == 0 && decision == 0) {
                decision = 2;
            }
            action TreeStartsEarly when (Buggy == 1 && tree == 0
                && (native <= 1 || compat <= 2)) {
                tree = 1;
            }

            invariant HeldAndQueuedSelectSerialEscape: decision <= 1;
            invariant TreeStaysOutsideFallbackParents: tree == 0 || (native == 2 && compat == 3);
            bound FallbackAdmissionIsFinite: native <= 2 && compat <= 3 && gate <= 1
                && decision <= 2 && tree <= 1;
        }
    }
}

/// Every incomplete-history qualify reaches the tree once the native learner is held and the
/// compat learner is queued. Each transition that can permanently hold the next one back is
/// weakly fair. The old held-plus-queued detector (`KeepBarrier`) breaks the obligation by
/// retaining the only compiler tokens either slice can use.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(qualify_fallback_admission_model)]
pub fn qualify_fallback_admission_liveness() -> Liveness {
    Liveness {
        name: "FallbackSlicesReleaseTheTree",
        goal: eq(var("tree"), int(1)),
        weak: vec![
            "NativeParentHolds",
            "CompatParentQueues",
            "SerialEscape",
            "NativeSliceEnds",
            "CompatParentHolds",
            "CompatSliceEnds",
            "TreeStarts",
        ],
        strong: vec![],
        mutants: vec!["KeepBarrier"],
    }
}
