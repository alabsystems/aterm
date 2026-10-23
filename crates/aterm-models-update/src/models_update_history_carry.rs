// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update handoff's history carry.

use aterm_spec::derive::Model;

/// One session's history across one handoff.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_history_carry_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateHistoryCarry {
            const Buggy = 0;
            const Cap = 1;
            const Deep = 2;
            var phase = 0;
            var exp = 0;
            var late = 0;
            var moved = 0;
            var total = 0;
            var carried = 0;
            var take = 0;
            var dropped = 0;
            var intact = 1;
            var cleared = 0;
            var held = 0;
            var failed = 0;

            action Export when (phase == 0) {
                phase = 1;
                exp = Deep;
            }
            action Output when (phase == 1 && late <= 1) {
                late = late + 1;
            }
            action Rewrap when (phase == 1 && moved == 0) {
                moved = 1;
            }
            action Park when (phase == 1) {
                phase = 2;
                total = exp + late;
                carried = Cap;
                take = if (moved == 0 || Buggy == 1) && late <= Cap {
                    exp + late - Cap
                } else {
                    0
                };
                dropped = if (moved == 0 || Buggy == 1) && late <= Cap {
                    0
                } else if Buggy == 1 {
                    0
                } else {
                    exp + late - Cap
                };
            }
            action Corrupt when (phase == 2 && intact == 1 && take > 0 && cleared == 0) {
                intact = 0;
            }
            action Clear when (phase == 2 && cleared == 0) {
                cleared = 1;
            }
            action Settle when (phase == 2) {
                phase = 3;
                held = if cleared == 1 && Buggy == 1 && intact == 1 {
                    take
                } else if cleared == 1 {
                    0
                } else if intact == 1 {
                    carried + take
                } else {
                    carried
                };
                failed = if cleared == 1 {
                    0
                } else if intact == 1 {
                    0
                } else if Buggy == 1 {
                    0
                } else {
                    take
                };
            }
            action Settled when (phase == 3) {
                phase = 3;
            }

            invariant NothingSilentlyLost:
                if phase == 3 {
                    cleared == 1 || held + dropped + failed == total
                } else {
                    held == 0
                };
            invariant SidecarOnlyOverItsOwnHistory:
                if take > 0 { moved == 0 && late <= carried } else { take == 0 };
            invariant ClearedStaysCleared:
                if phase == 3 && cleared == 1 { held == 0 } else { cleared <= 1 };
        }
    }
}
