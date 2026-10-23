// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the exceptions digest (`MessageCenter::digest`).

use aterm_spec::derive::Model;

/// Model verifying that `Later` exceptions never request a row, drop entries, or bypass dismissal.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exceptions_digest_model() -> Model {
    aterm_spec::ty_model! {
        ExceptionsDigest {
            const Buggy = 0;
            const Cap = 12;
            var open = 0;
            var glass = 0;
            var now_rows = 0;
            var quiet = 0;
            var filed = 0;
            var lost = 0;

            action PostNow when (now_rows <= 2) {
                now_rows = now_rows + 1;
                glass = glass + 1;
            }
            action FoldNow when (now_rows > 0) {
                now_rows = now_rows - 1;
                glass = glass - 1;
            }
            action PostLater when (quiet == 0 && open <= Cap - 1) {
                open = open + 1;
            }
            action PostLaterAtCap when (quiet == 0 && open > Cap - 1) {
                filed = if filed <= Cap - 1 { filed + 1 } else { filed };
            }
            action Suppress when (quiet == 1) {
                filed = filed;
            }
            action FileAll when (open > 0) {
                filed = if filed + open <= Cap { filed + open } else { Cap };
                open = 0;
                quiet = 1;
            }
            action Resurface when (quiet == 1) {
                quiet = 0;
            }
            action PostLaterSeated when (Buggy == 1 && glass == now_rows && quiet == 0 && open <= Cap - 1 && now_rows <= 2) {
                open = open + 1;
                glass = glass + 1;
            }
            action PostThroughQuiet when (Buggy == 1 && quiet == 1 && open <= Cap - 1) {
                open = open + 1;
            }
            action CapDrops when (Buggy == 1 && quiet == 0 && open > Cap - 1 && lost == 0) {
                lost = lost + 1;
            }
            action OverCap when (Buggy == 1 && quiet == 0 && open <= Cap) {
                open = open + 1;
            }

            invariant LaterNeverWantsARow: glass == now_rows;
            invariant DismissalHolds: quiet == 0 || open == 0;
            invariant NothingIsLost: lost == 0;
            invariant OpenIsBounded: open <= Cap;
        }
    }
}
