// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model for `atpkg::cli::wait_then_run` polling `pass-owed.json` and `progress.json`.

use aterm_spec::derive::*;

/// Returns the `AtpkgPendingWait` formal model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_pending_wait_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgPendingWait {
            const Buggy = 0;
            var owed = 1;
            var begun = 0;
            var running = 0;
            var planned = 0;
            var row = 0;
            var shim = 0;
            var bare = 0;
            var step = 0;
            var seen = 0;
            var coming = 0;
            var left = 0;
            var done = 0;
            var stub = 0;

            action Begin when (owed == 1 && begun == 0) {
                begun = 1;
                running = 1;
            }
            action Plan when (running == 1 && planned == 0) {
                planned = 1;
            }
            action PlanWithout when (running == 1 && planned == 0) {
                planned = 2;
            }
            action Retire when (
                owed == 1 && begun == 1 && (planned > 0 || running == 0 || Buggy == 2)
            ) {
                owed = 0;
            }
            action Lapse when (owed == 1 && begun == 0) {
                owed = 0;
            }
            action Rebegin when (owed == 1 && begun == 1 && running == 0 && (planned == 0 || planned == 2)) {
                running = 1;
                planned = 0;
            }
            action Download when (running == 1 && planned == 1 && row == 0) {
                row = 1;
            }
            action SetUp when (running == 1 && row == 1) {
                row = 2;
                shim = 1;
            }
            action SetUpWithout when (running == 1 && row == 1) {
                row = 2;
                bare = 1;
            }
            action Finish when (running == 1 && row == 2) {
                row = 3;
            }
            action FailRow when (running == 1 && planned == 1 && row <= 1) {
                row = 4;
            }
            action End when (running == 1) {
                running = 0;
            }

            action ObserveOwed when (step == 0 && stub == 0) {
                seen = if Buggy == 1 { shim } else { owed };
                step = 1;
            }
            action ObservePass when (step == 1) {
                coming = if Buggy == 1 { running } else { if seen == 1 { 1 } else { running } };
                left = if running == 1 && (planned == 0 || planned == 2) && (Buggy == 1 || seen == 0) {
                    1
                } else {
                    0
                };
                done = if row == 3 { 1 } else { 0 };
                step = 2;
            }
            action DecideRun when (
                step == 2
                    && (((Buggy == 0 || Buggy == 2) && shim == 1)
                        || (Buggy == 1 && (seen == 1 || (coming == 0 && owed == 0 && row <= 3))))
            ) {
                stub = 1;
                step = 3;
            }
            action DecideFail when (
                step == 2
                    && (((Buggy == 0 || Buggy == 2)
                        && shim == 0
                        && (row == 4 || coming == 0 || left == 1 || done == 1))
                        || (Buggy == 1
                            && seen == 0
                            && (row == 4
                                || (coming == 0 && owed == 0)
                                || (left == 1 && owed == 0)
                                || done == 1)))
            ) {
                stub = 2;
                step = 3;
            }
            action DecideWait when (
                step == 2
                    && (((Buggy == 0 || Buggy == 2)
                        && shim == 0
                        && row <= 3
                        && coming == 1
                        && left == 0
                        && done == 0)
                        || (Buggy == 1
                            && seen == 0
                            && row <= 3
                            && done == 0
                            && (coming == 1 || owed == 1)
                            && (left == 0 || owed == 1)))
            ) {
                step = 0;
            }

            invariant NoFalseFailure: stub <= 1 || shim == 0 || row == 4;
            invariant NoFailureWhileComing:
                stub <= 1
                    || row == 4
                    || planned == 2
                    || bare == 1
                    || (running == 0 && (begun == 1 || owed == 0));
            invariant RunsOnlyInstalled: stub == 0 || stub == 2 || shim == 1;
        }
    }
}
