// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content-scroll band ring history replay vs invalidation model (`ContentScrollState::delta_since`).

use aterm_spec::derive::Model;

/// Bounded ring-buffer history model for `ContentScrollState::delta_since`: up to `Cap`
/// (`CONTENT_SCROLL_BAND_RING = 16`) consecutive band moves without an unexplained screen reset
/// replay as `Bands` (`verdict == 1`), whereas ring overflow (`moves > Cap`) or an unexplained
/// reset (`Poison`) retires replay to `Invalidate` (`verdict == 2`). `Buggy = 1` replays an
/// overwritten ring slot (`moves == Cap + 1`) as `Bands`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn content_scroll_band_history_model() -> Model {
    aterm_spec::ty_model! {
        ContentScrollBandHistory {
            const Cap = 16;
            const Buggy = 0;
            var moves = 0;
            var unexplained = 0;
            var verdict = 0;
            action Step when (moves <= Cap) {
                moves = moves + 1;
                verdict = if unexplained == 1 { 2 } else {
                    if moves <= Cap - 1 || Buggy == 1 { 1 } else { 2 }
                };
            }
            action Poison when (unexplained == 0) {
                unexplained = 1;
                verdict = 2;
            }
            action Baseline when (moves > 0 || unexplained == 1) {
                moves = 0;
                unexplained = 0;
                verdict = 0;
            }
            bound Bounds: moves <= Cap + 1 && unexplained <= 1 && verdict <= 2;
            invariant ReplayComplete: verdict == 0 || verdict == 2 ||
                (moves > 0 && moves <= Cap && unexplained == 0);
            invariant RetireIncomplete: (moves <= Cap && unexplained == 0) || verdict == 2;
        }
    }
}
