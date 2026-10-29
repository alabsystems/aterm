// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty occupied-cell coverage ceiling and retract/re-arm reversal continuity models.

use aterm_spec::derive::Model;

/// Occupied-cell bed coverage clamp (`rainbow_kitty::ribbon::clamp_bed_cov`).
/// `Buggy = 1` omits the `UNDER_COV_CAP` ceiling so requested coverage above `Cap` escapes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_occupied_coverage_model() -> Model {
    aterm_spec::ty_model! {
        RainbowOccupiedCoverage {
            const Cap = 236;
            const RequestedMax = 270;
            const Buggy = 0;
            var requested = 0;
            var emitted = 0;
            action Raise when (requested <= RequestedMax - 1) {
                requested = requested + 1;
                emitted = if requested + 1 > Cap {
                    if Buggy == 1 { requested + 1 } else { Cap }
                } else {
                    requested + 1
                };
            }
            invariant OccupiedBodyBounded: emitted <= Cap;
        }
    }
}

/// Retracting-cell re-arm reversal continuity (`rainbow_kitty::ribbon`).
/// `Buggy = 1` snaps a partially faded cell back to `Peak` at the reversal instant instead of
/// preserving its faded envelope and easing back up along `edge_in`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_retract_rearm_model() -> Model {
    aterm_spec::ty_model! {
        RainbowRetractRearm {
            const Peak = 4;
            const Buggy = 0;
            var mode = 0;
            var visibility = 4;
            var discontinuity = 0;
            action BeginFade when (mode == 0) { mode = 1; }
            action Fade when (mode == 1 && visibility > 0) {
                visibility = visibility - 1;
            }
            action Reclaim when (mode == 1) {
                discontinuity = if Buggy == 1 && Peak > visibility { 1 } else { 0 };
                visibility = if Buggy == 1 { Peak } else { visibility };
                mode = 2;
            }
            action Rearm when (mode == 2 && visibility <= Peak - 1) {
                visibility = visibility + 1;
            }
            bound VisibilityBounded: visibility <= Peak;
            invariant ReversalContinuous: discontinuity == 0;
        }
    }
}
