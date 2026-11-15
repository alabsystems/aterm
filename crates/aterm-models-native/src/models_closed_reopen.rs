// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Atomic publication of a detached two-leaf closed-history candidate.

use aterm_spec::derive::Model;

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn closed_reopen_publication_model() -> Model {
    aterm_spec::ty_model! {
        ClosedReopenPublication {
            const Buggy = 0;
            var phase = 0;
            var candidate = 1;
            var window = 1;
            var prefix = 0;
            var custody = 0;
            var published = 0;
            var consumed = 0;
            var canonical = 0;
            var edited = 0;
            var stale_publication = 0;
            var dead_publication = 0;
            var double_custody = 0;
            action Begin when (phase == 0) {
                phase = 1;
                prefix = 1;
                custody = 1;
                consumed = if Buggy == 1 { 1 } else { 0 };
            }
            action BeginWaiting when (phase == 0) {
                phase = 1;
                consumed = if Buggy == 1 { 1 } else { 0 };
            }
            action Wait when (phase == 1 && candidate == 1 && window == 1) {
                phase = 1;
            }
            action FirstReady when (phase == 1 && prefix == 0 && candidate == 1 && window == 1) {
                prefix = 1;
                custody = 1;
            }
            action Stage when (phase == 1 && prefix == 1 && candidate == 1 && window == 1) {
                phase = 2;
                prefix = 2;
                custody = 2;
                published = if Buggy == 2 { 1 } else { 0 };
            }
            action Replace when (phase > 0 && phase <= 2 && candidate == 1) {
                candidate = 2;
            }
            action Expire when (phase > 0 && phase <= 2 && candidate == 1) {
                candidate = 0;
            }
            action Kill when (phase > 0 && phase <= 2 && window == 1) {
                window = 0;
            }
            action Edit when (phase > 0 && phase <= 2 && edited == 0) {
                canonical = 1;
                edited = 1;
            }
            action Cancel when (phase > 0 && phase <= 2) {
                phase = 3;
                prefix = 0;
                custody = 0;
                canonical = if Buggy == 5 { 0 } else { canonical };
            }
            action Publish when (
                phase == 2
                && (candidate == 1 || Buggy == 3)
                && (window == 1 || Buggy == 4)
            ) {
                phase = 3;
                prefix = 0;
                custody = 0;
                published = 2;
                consumed = 1;
                stale_publication = if candidate == 1 { 0 } else { 1 };
                dead_publication = if window == 0 { 1 } else { 0 };
            }
            action Repeat when (phase == 3 && consumed == 1 && double_custody == 0) {
                double_custody = if Buggy == 6 { 1 } else { 0 };
            }
            invariant HistoryOnlyAfterPublication: consumed == 0 || published == 2;
            invariant CompleteTopology: published == 0 || published == 2;
            invariant ExactCandidate: stale_publication == 0;
            invariant LiveDestination: dead_publication == 0;
            invariant SharedEditsSurvive: canonical == edited;
            invariant OneShotCustody: double_custody == 0;
            bound PhaseBounded: phase <= 3;
            bound PrefixBounded: prefix <= 2 && custody <= 2;
        }
    }
}
