// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Per-row retry waits follow the current admitted account and monotonic deadline.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_retry_inputs_model() -> Model {
    aterm_spec::ty_model! {
        AccountRetryInputs {
            const Buggy = 0;
            var row = 0; var origin = 0; var cached = 0; var fresh = 0;
            var obsolete = 0; var result = 0; var isolated = 0;

            action RowA when (row == 1) {
                row = 0; result = 0; isolated = 0;
            }
            action RowB when (row == 0) {
                row = 1; result = 0; isolated = 0;
            }
            action Probe when (1 == 1) {
                result = if cached == 1 && (fresh == 1 || Buggy == 5) &&
                    (row == origin || Buggy == 1) && ((if Buggy == 2 {0} else {1}) == 1)
                    {1} else {2};
                cached = if cached == 1 && (row > origin || origin > row) &&
                    ((if Buggy == 3 {0} else {1}) == 1) {0} else {cached};
                fresh = if cached == 1 && (row > origin || origin > row) &&
                    ((if Buggy == 3 {0} else {1}) == 1) {0} else {fresh};
                obsolete = if cached == 1 && (row > origin || origin > row) {1} else {obsolete};
                isolated = 0;
            }
            action Fail when (result == 2) {
                origin = row; cached = 1; fresh = 1;
                obsolete = 0; result = 0; isolated = 0;
            }
            action Expire when (cached == 1 && fresh == 1) {
                fresh = 0; result = 0; isolated = 0;
            }
            action OtherFailure when (1 == 1) {
                cached = if Buggy == 4 {0} else {cached};
                isolated = if cached == 1 && fresh == 1 && origin == row {1} else {0};
                result = 0;
            }

            invariant HeldUsesCurrentRow: (1 > result || result > 1) || (cached == 1 && origin == row);
            invariant HeldBeforeDeadline: (1 > result || result > 1) || fresh == 1;
            invariant SameInputsWait: (2 > result || result > 2) || cached == 0 || fresh == 0 || (row > origin || origin > row);
            invariant NoOldWaitRestored: (1 > result || result > 1) || obsolete == 0;
            invariant IndependentRows: isolated == 0 || (cached == 1 && fresh == 1 && origin == row);
        }
    }
}
