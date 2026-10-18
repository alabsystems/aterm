// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mouse-selection lifecycle of `aterm_selection::TextSelection`: a press
//! starts a selection, a drag moves its far end, the release completes it, a
//! shift-click extends a completed one, and clearing ends it.

use aterm_spec::derive::*;

/// `TextSelection`'s five lifecycle calls, each callable in every phase and a no-op
/// outside the phase it belongs to.
///
/// `phase` is `TextSelection::state()` (0 None, 1 InProgress, 2 Complete); `end` is which
/// point the far anchor holds (0 the press, 1 the drag, 2 the shift-click). The state also
/// observes the last call: `last` (0 none, 1 Start, 2 Update, 3 Complete, 4 Extend,
/// 5 Clear), `pre` the phase it ran in, `moved` whether it moved the far anchor. Each
/// invariant reads one call's rule off that observer.
///
/// `Buggy = 1` drops each call's phase check: a drag moves the anchor after release, a
/// release with nothing pressed completes, a shift-click mid-drag jumps the anchor, and
/// clearing spares a completed selection. Tier-1:
/// `aterm-selection/tests/conformance_text_selection.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn text_selection_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        TextSelectionLifecycle {
            const Buggy = 0;
            var phase = 0;
            var end = 0;
            var pre = 0;
            var last = 0;
            var moved = 0;

            action Start {
                phase = 1;
                end = 0;
                pre = phase;
                last = 1;
                moved = if end == 0 { 0 } else { 1 };
            }
            action Update {
                end = if phase == 1 || (Buggy == 1 && phase == 2) { 1 } else { end };
                pre = phase;
                last = 2;
                moved = if (phase == 1 || (Buggy == 1 && phase == 2)) && (end <= 0 || end > 1) {
                    1
                } else {
                    0
                };
            }
            action Complete {
                phase = if phase == 1 || (Buggy == 1 && phase == 0) { 2 } else { phase };
                pre = phase;
                last = 3;
                moved = 0;
            }
            action Extend {
                phase = if phase == 2 || (Buggy == 1 && phase == 1) { 1 } else { phase };
                end = if phase == 2 || (Buggy == 1 && phase == 1) { 2 } else { end };
                pre = phase;
                last = 4;
                moved = if (phase == 2 || (Buggy == 1 && phase == 1)) && end <= 1 { 1 } else { 0 };
            }
            action Clear {
                phase = if Buggy == 1 && phase == 2 { 2 } else { 0 };
                pre = phase;
                last = 5;
                moved = 0;
            }

            invariant UpdateOnlyInProgress: last <= 1 || last > 2 || moved == 0 || pre == 1;
            invariant CompleteOnlyFromInProgress:
                last <= 2 || last > 3 || (pre == 1 && phase == 2)
                    || ((pre <= 0 || pre > 1) && phase == pre);
            invariant ExtendOnlyFromComplete:
                last <= 3 || last > 4 || (pre == 2 && phase == 1 && end == 2)
                    || (pre <= 1 && phase == pre && moved == 0);
            invariant ClearIsNone: last <= 4 || phase == 0;
        }
    }
}
