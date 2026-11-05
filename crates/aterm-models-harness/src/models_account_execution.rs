// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Exact retained account intent and a writer held through the execution grant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_execution_model() -> Model {
    aterm_spec::ty_model! {
        AccountExecution {
            const Buggy = 0;
            var phase = 0; var held = 0; var revision = 0;
            var captured = 2; var guard = 0; var granted = 0;
            action Hold when (held == 0 && (guard == 0 || Buggy == 3)) { held = 1; }
            action Release when (held == 1 && guard == 0) { held = 0; }
            action Renew when (revision == 0 && (guard == 0 || Buggy == 4)) { revision = 1; }
            action Capture when (phase == 0 && held == 0) {
                captured = revision; phase = 1;
            }
            action Admit when (phase == 1) {
                granted = if (held == 0 || Buggy == 1) && (captured == revision || Buggy == 2) {1} else {0};
                guard = if Buggy == 5 {0} else {if (held == 0 || Buggy == 1) && (captured == revision || Buggy == 2) {1} else {0}};
                phase = 2;
            }
            action Retry when (phase == 2 && guard == 0) { phase = 1; granted = 0; }
            action Finish when (phase == 2 && guard == 1) { guard = 0; phase = 3; granted = 0; }
            invariant Custody: guard == 0 || held == 0;
            invariant Generation: guard == 0 || captured == revision;
            invariant Grant: granted == guard;
            bound Finite: 0 <= phase && phase <= 3 && 0 <= held && held <= 1
                && 0 <= revision && revision <= 1 && 0 <= captured && captured <= 2;
        }
    }
}
