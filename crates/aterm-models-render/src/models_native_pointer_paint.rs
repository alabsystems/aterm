// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Presented hit geometry survives host-proven pointer feedback, while paint provenance
/// and generic invalidation keep their stricter authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_pointer_paint_model() -> Model {
    aterm_spec::ty_model! {
        NativePointerPaint {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var pointer = 0; var strict = 0; var pending = 0; var unchanged = 1;
            action AnotherCase when (phase == 0 && case <= 8) { case = case + 1; }
            action Read when (phase == 0) {
                phase = 1;
                pointer = if (Buggy == 1 && (case == 2 || case == 3))
                    || (Buggy == 10 && case == 1) || (Buggy == 12 && case == 9) {0}
                    else if (Buggy == 2 && case == 4) || (Buggy == 3 && case == 5)
                        || (Buggy == 4 && case == 6) || (Buggy == 5 && case == 7)
                        || (Buggy == 6 && case == 8) {1}
                    else if case <= 3 || case == 9 {1} else {0};
                strict = if Buggy == 11 && (case == 2 || case == 3) {1}
                    else if case == 1 || case == 9 {1} else {0};
                pending = if Buggy == 8 && (case == 2 || case == 3) {0}
                    else if (case == 2 || case == 3) || (Buggy == 9 && case == 9) {1} else {0};
                unchanged = if Buggy == 7 && (case == 2 || case == 3) {0}
                    else if case <= 7 {1} else {0};
            }
            invariant PointerStyleRetainsInput: phase == 0 || case <= 1 || case > 3 || pointer == 1;
            invariant PointerStyleStillNeedsPaint: phase == 0 || case <= 1 || case > 3 || pending == 1;
            invariant StrictReaderRejectsPending: phase == 0 || case <= 1 || case > 3 || strict == 0;
            invariant GenericInvalidationIsNotFeedback: phase == 0 || 4 > case || case > 4 || pointer == 0;
            invariant ChangedInputIsRejected: phase == 0 || case <= 4 || case > 8 || pointer == 0;
            invariant NoPaintingRetainsProvenance: phase == 0 || case > 7 || unchanged == 1;
            invariant SuccessfulPresentRestoresReaders: phase == 0 || 9 > case
                || (pointer == 1 && strict == 1 && pending == 0);
            invariant CurrentFrameAvailable: phase == 0 || case > 1 || (pointer == 1 && strict == 1);
        }
    }
}
