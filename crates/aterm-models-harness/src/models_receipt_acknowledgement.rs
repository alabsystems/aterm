// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One exact receipt read and file/directory acknowledgement. An immutable
/// creation can leave visible bytes with an unacknowledged file flush.
/// This does not model native execution,
/// process authorization, arbitrary filesystem failures, or service liveness.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn receipt_acknowledgement_model() -> Model {
    aterm_spec::ty_model! {
        ReceiptAcknowledgement {
            const Buggy = 0;
            var changed = 0; var failed = 0; var file_failed = 0;
            var done = 0; var accepted = 0;
            action Changed when (done == 0 && changed == 0) { changed = 1; }
            action Failed when (done == 0 && failed == 0) { failed = 1; }
            action FileFailed when (done == 0 && file_failed == 0) { file_failed = 1; }
            action Acknowledge when (done == 0) {
                done = 1;
                accepted = if Buggy == 3 {0}
                    else if (changed == 0 || Buggy == 1) && (failed == 0 || Buggy == 2)
                        && (file_failed == 0 || Buggy == 4)
                    {1} else {0};
            }
            invariant Exact: accepted == 0 || changed == 0;
            invariant Durable: accepted == 0 || (failed == 0 && file_failed == 0);
            invariant Available: done == 0 || changed == 1 || failed == 1
                || file_failed == 1 || accepted == 1;
            bound Finite: 0 <= changed && changed <= 1 && 0 <= failed && failed <= 1
                && 0 <= file_failed && file_failed <= 1
                && 0 <= done && done <= 1 && 0 <= accepted && accepted <= 1;
        }
    }
}
