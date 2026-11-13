// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Qualify's continuous incomplete-history slice parent and compiler-token escape.

use aterm_spec::derive::*;

/// THE QUALIFY LEARNERS CANNOT WEDGE BEHIND THEIR OWN COMPILER-TOKEN BARRIER OR EXPOSE A HANDOFF
/// GAP. When retained unit history is incomplete, one exclusive driver-sized parent remains held
/// continuously while native and then compat learn. Observing that parent releases every
/// non-export compiler token; the tree starts only after both slices and their parent have ended.
///
/// `KeepBarrier` is the old detector's negative control: it observes the serial state but retains
/// every compiler token, so native cannot finish. `DropParentBetweenSlices` recreates the old
/// native-to-compat gap. `TreeStartsEarly` independently starts tree work while the parent lives.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn qualify_fallback_admission_model() -> Model {
    aterm_spec::ty_model! {
        QualifyFallbackAdmission {
            const Buggy = 0;

            // One cold parent: 0 before admission, 1 held across both slices, 2 returned.
            var parent = 0;
            // Native: 0 before completion, 1 complete.
            var native = 0;
            // Compat: 0 before completion, 1 complete after native.
            var compat = 0;
            // Every non-export compiler token: 0 retained by admission, 1 released.
            var gate = 0;
            // Admission decision: 0 pending, 1 bounded serial escape, 2 wedged mutant.
            var decision = 0;
            // The tree remains outside the cold parent: 0 waiting, 1 started after it ends.
            var tree = 0;

            action ColdParentHolds when (parent == 0) {
                parent = 1;
            }
            action SerialEscape when (parent == 1 && gate == 0
                && decision == 0) {
                gate = 1;
                decision = 1;
            }
            action NativeSliceEnds when (parent == 1 && native == 0 && gate == 1) {
                native = 1;
            }
            action CompatSliceEnds when (parent == 1 && native == 1 && compat == 0
                && gate == 1) {
                compat = 1;
            }
            action ColdParentEnds when (parent == 1 && native == 1 && compat == 1) {
                parent = 2;
            }
            action TreeStarts when (parent == 2 && native == 1 && compat == 1
                && gate == 1 && tree == 0) {
                tree = 1;
            }

            action KeepBarrier when (Buggy == 1 && parent == 1
                && gate == 0 && decision == 0) {
                decision = 2;
            }
            action DropParentBetweenSlices when (Buggy == 1 && parent == 1
                && native == 1 && compat == 0) {
                parent = 2;
            }
            action TreeStartsEarly when (Buggy == 1 && tree == 0 && parent <= 1) {
                tree = 1;
            }

            invariant HeldParentSelectsSerialEscape: decision <= 1;
            invariant ColdParentCoversBothSlices: parent <= 1 || (native == 1 && compat == 1);
            invariant TreeStaysOutsideFallbackParent: tree == 0 || (parent == 2
                && native == 1 && compat == 1);
            bound FallbackAdmissionIsFinite: parent <= 2 && native <= 1 && compat <= 1
                && gate <= 1 && decision <= 2 && tree <= 1;
        }
    }
}

/// Every incomplete-history qualify reaches the tree once its continuous cold parent is held.
/// Each transition that can permanently hold the next one back is weakly fair. `KeepBarrier`
/// retains the only compiler tokens native can use; `DropParentBetweenSlices` strands compat.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(qualify_fallback_admission_model)]
pub fn qualify_fallback_admission_liveness() -> Liveness {
    Liveness {
        name: "FallbackSlicesReleaseTheTree",
        goal: eq(var("tree"), int(1)),
        weak: vec![
            "ColdParentHolds",
            "SerialEscape",
            "NativeSliceEnds",
            "CompatSliceEnds",
            "ColdParentEnds",
            "TreeStarts",
        ],
        strong: vec![],
        mutants: vec!["KeepBarrier", "DropParentBetweenSlices"],
    }
}
