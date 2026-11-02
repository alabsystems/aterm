// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Review preparation and health publication sample the same credential backend.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_review_storage_model() -> Model {
    aterm_spec::ty_model! {
        AccountReviewStorage {
            const Buggy = 0;
            var initial = 0; var selected = 0; var current = 0; var phase = 0;
            var hidden = 0; var isolated = 0; var launches = 0; var records = 0;
            var file_evidence = 0;
            action Initial when (phase == 0 && 3 > initial) { initial = initial + 1; }
            action Select when (phase == 0 && 3 > selected) { selected = selected + 1; }
            action Current when (phase == 0 && 3 > current) { current = current + 1; }
            action Check when (phase == 0) {
                phase = 1;
                hidden = if Buggy == 1 {if initial > 0 {1} else {0}} else {if selected > 0 {1} else {0}};
                isolated = if Buggy == 2 {if initial == 0 {1} else {0}} else {if selected == 0 {1} else {0}};
                launches = if Buggy == 3 || current == selected {1} else {0};
                records = if Buggy == 4 || current == selected {1} else {0};
                file_evidence = if Buggy == 5 || (initial == 0 && selected == 0) {1} else {0};
            }
            invariant CurrentBackend: phase == 0 || hidden == (if selected > 0 {1} else {0});
            invariant CurrentSeam: phase == 0 || isolated == (if selected == 0 {1} else {0});
            invariant LaunchFence: phase == 0 || launches == (if current == selected {1} else {0});
            invariant OutcomeFence: phase == 0 || records == (if current == selected {1} else {0});
            invariant FileEvidence: phase == 0 || selected == 0 || file_evidence == 0;
        }
    }
}
