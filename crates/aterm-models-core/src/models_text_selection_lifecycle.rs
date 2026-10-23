// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the mouse-selection lifecycle in `aterm_selection::TextSelection`.

use aterm_spec::derive::*;

/// `TextSelection`'s five lifecycle calls.
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
