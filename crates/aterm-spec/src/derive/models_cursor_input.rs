// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact cursor-input evidence admission. This is the bounded policy behind the
//! native input seam's split-coordinate, grapheme, forward/reverse-wrap, and
//! row-prefix decisions; the real-code Tier-1 projections live beside that
//! seam in `aterm-gui/src/app_input.rs`.

use super::*;

/// A cursor movement candidate may arm only while every evidence dimension
/// relevant to its input shape is exact. Positive actions model the newly
/// supported shapes; rejection actions independently remove one credential.
/// `Buggy=1` turns each rejection into an arm, providing a counterexample for
/// every missing-evidence class rather than proving a vacuous always-dark gate.
///
/// Bottom-row forward wrap has one deliberately two-frame branch: the exact
/// uniform-scroll signal translates already-resident cell geometry, then the
/// sole-next parser generation is presented before its fresh-line material row
/// can be probed. That hold keeps the translated resident trail but clears
/// non-cell transients (crown/glide/pop-class state) and admits no fresh light.
/// The following exact material probe may complete the fold; any failed probe
/// retires it. `Buggy=1` reproduces the blackout by dropping the translated
/// resident at the hold boundary.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_input_evidence_model() -> Model {
    crate::ty_model! {
        CursorInputEvidence {
            const Buggy = 0;
            var phase = 0;
            var armed = 0;
            var session_bound = 1;
            var layout_bound = 1;
            var projection_exact = 1;
            var anchor_current = 1;
            var single_grapheme = 1;
            var wrap_exact = 1;
            var frontier_bounded = 1;
            var reverse_mode = 1;
            var reverse_row_available = 1;
            var adjacent_rows_exact = 1;
            var scroll_signal_exact = 1;
            var next_generation_exact = 1;
            var bottom_material_exact = 1;
            var deferred_wrap = 0;
            var bottom_scroll_fold = 0;
            var trail_lit = 0;
            var bottom_candidate_pending = 0;
            var bottom_material_hold = 0;
            var translated_trail_resident = 0;
            var non_cell_transient = 0;

            action ArmSplitProjected when (phase == 0) {
                phase = 1; armed = 1;
            }
            action RejectSiblingSession when (phase == 0) {
                phase = 1; session_bound = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action RejectMissingLayoutBinding when (phase == 0) {
                phase = 1; layout_bound = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action RejectMismatchedProjection when (phase == 0) {
                phase = 1; projection_exact = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action RejectStaleAnchor when (phase == 0) {
                phase = 1; anchor_current = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action ArmMultiscalarGrapheme when (phase == 0) {
                phase = 1; armed = 1;
            }
            action RejectMultipleGraphemes when (phase == 0) {
                phase = 1; single_grapheme = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action ArmDeferredWrap when (phase == 0) {
                phase = 1; armed = 1; deferred_wrap = 1;
                trail_lit = if Buggy == 1 { 0 } else { 1 };
            }
            action RejectWideDeferredWrap when (phase == 0) {
                phase = 1; wrap_exact = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action ArmBottomScrollFold when (phase == 0) {
                phase = 1; armed = 1; bottom_scroll_fold = 1; trail_lit = 1;
            }
            action SeedTranslatedBottomScrollTrail when (
                phase == 0 && translated_trail_resident == 0
                    && bottom_candidate_pending == 0
            ) {
                // A prior admitted trail has been translated by the exact
                // uniform-scroll signal. Seed one non-cell transient too so
                // the hold's selective retirement cannot pass vacuously.
                translated_trail_resident = 1; non_cell_transient = 1;
                bottom_candidate_pending = 1;
            }
            action HoldBottomScrollMaterialProbe when (
                phase == 0 && bottom_candidate_pending == 1
                    && translated_trail_resident == 1
                    && scroll_signal_exact == 1
                    && next_generation_exact == 1
                    && anchor_current == 1
            ) {
                // The sole-next generation is authenticated, but the newly
                // exposed material row is intentionally unprobed this frame.
                // Preserve translated CELLS only; clear non-cell transients;
                // do not turn provisional ownership into fresh admission.
                phase = 1; bottom_material_hold = 1;
                bottom_material_exact = 0; armed = 0;
                bottom_scroll_fold = 0; trail_lit = 0;
                translated_trail_resident = if Buggy == 1 { 0 } else { 1 };
                non_cell_transient = 0;
            }
            action ConfirmHeldBottomScrollMaterial when (
                phase == 1 && bottom_candidate_pending == 1
                    && bottom_material_hold == 1
                    && scroll_signal_exact == 1
                    && next_generation_exact == 1
            ) {
                // A fresh-line material probe now supplies the final exact
                // credential. The translated resident remains, and the fold
                // may independently add its authenticated trail segment.
                phase = 2; bottom_candidate_pending = 0;
                bottom_material_hold = 0; bottom_material_exact = 1;
                armed = 1; bottom_scroll_fold = 1; trail_lit = 1;
            }
            action RejectHeldBottomScrollMaterial when (
                phase == 1 && bottom_candidate_pending == 1
                    && bottom_material_hold == 1
            ) {
                // The proof frame is the same parser generation as the hold.
                // Retire only the candidate/new-birth credential and non-cell
                // state; mature translated cells on earlier rows stay valid.
                phase = 2; bottom_candidate_pending = 0;
                bottom_material_hold = 0; bottom_material_exact = 0;
                armed = 0; bottom_scroll_fold = 0; trail_lit = 0;
                non_cell_transient = 0;
            }
            action RejectBottomScrollSignal when (phase == 0) {
                phase = 1; scroll_signal_exact = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action RejectBottomScrollGeneration when (phase == 0) {
                phase = 1; next_generation_exact = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action RejectBottomScrollMaterial when (phase == 0) {
                phase = 1; bottom_material_exact = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action ArmReverseFold when (phase == 0) {
                phase = 1; armed = 1; trail_lit = 1;
            }
            action RejectReverseWrapDisabled when (phase == 0) {
                phase = 1; reverse_mode = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action RejectReverseTopRow when (phase == 0) {
                phase = 1; reverse_row_available = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action RejectReverseRowMismatch when (phase == 0) {
                phase = 1; adjacent_rows_exact = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }
            action ArmWideGridPrefix when (phase == 0) {
                phase = 1; armed = 1;
            }
            action RejectOverCapFrontier when (phase == 0) {
                phase = 1; frontier_bounded = 0;
                armed = if Buggy == 1 { 1 } else { 0 };
            }

            invariant PhaseBounded: phase <= 2;
            invariant ArmedBounded: armed <= 1;
            invariant SessionBoundRequired: armed <= session_bound;
            invariant LayoutBindingRequired: armed <= layout_bound;
            invariant ExactProjectionRequired: armed <= projection_exact;
            invariant CurrentAnchorRequired: armed <= anchor_current;
            invariant SingleGraphemeRequired: armed <= single_grapheme;
            invariant ExactWrapRequired: armed <= wrap_exact;
            invariant BoundedFrontierRequired: armed <= frontier_bounded;
            invariant ReverseModeRequired: armed <= reverse_mode;
            invariant ReverseRowRequired: armed <= reverse_row_available;
            invariant ExactAdjacentRowsRequired: armed <= adjacent_rows_exact;
            invariant ExactScrollSignalRequired: armed <= scroll_signal_exact;
            invariant ExactNextGenerationRequired: armed <= next_generation_exact;
            invariant ExactBottomMaterialRequired: armed <= bottom_material_exact;
            invariant DeferredWrapLights: deferred_wrap <= trail_lit;
            invariant BottomScrollFoldLights: bottom_scroll_fold <= trail_lit;
            invariant TrailNeedsAdmission: trail_lit <= armed;
            invariant BottomScrollHoldPreservesTranslatedTrail:
                bottom_material_hold <= translated_trail_resident;
            invariant BottomScrollHoldClearsNonCellTransients:
                if bottom_material_hold == 1 {
                    non_cell_transient == 0
                } else { non_cell_transient <= 1 };
            invariant BottomScrollHoldCannotAdmitFreshTrail:
                if bottom_material_hold == 1 {
                    armed == 0 && trail_lit == 0 && bottom_scroll_fold == 0
                        && bottom_material_exact == 0
                } else { bottom_material_hold == 0 };
            invariant BottomScrollHoldTracksCandidate:
                bottom_material_hold <= bottom_candidate_pending;
            invariant BottomScrollLifecycleBounded:
                bottom_candidate_pending <= 1 && bottom_material_hold <= 1
                    && translated_trail_resident <= 1
                    && non_cell_transient <= 1;
        }
    }
}
