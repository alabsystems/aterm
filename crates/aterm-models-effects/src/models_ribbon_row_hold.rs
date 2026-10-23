// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The row that owns a typing/editing gesture owns its ribbon renewal.

use aterm_spec::derive::Model;

/// Two resident typing cohorts on different rows over three increasing gesture stamps.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ribbon_row_hold_model() -> Model {
    aterm_spec::ty_model! {
        RibbonRowHold {
            const Buggy = 0;
            const Steps = 3;
            var stamp = 0;
            var clock0 = 0;
            var clock1 = 0;
            var prior0 = 0;
            var prior1 = 0;
            var owner = 0;

            action TouchRow0 when (stamp <= Steps - 1) {
                prior0 = clock0; prior1 = clock1;
                owner = 0;
                clock0 = if Buggy == 3 { stamp + 2 } else { stamp + 1 };
                clock1 = if Buggy == 1 { stamp + 1 } else { clock1 };
                stamp = stamp + 1;
            }
            action TouchRow1 when (stamp <= Steps - 1) {
                prior0 = clock0; prior1 = clock1;
                owner = 1;
                clock1 = if Buggy == 2 { clock1 } else { stamp + 1 };
                clock0 = if Buggy == 1 { stamp + 1 } else { clock0 };
                stamp = stamp + 1;
            }

            invariant OnlyOwnerRenews:
                if owner == 0 { clock1 == prior1 } else { clock0 == prior0 };
            invariant OwnerKeepsItsLease:
                if owner == 0 { clock0 == stamp } else { clock1 == stamp };
            invariant StateBounded:
                stamp <= Steps && clock0 <= stamp && clock1 <= stamp
                    && prior0 <= stamp && prior1 <= stamp && owner <= 1;
        }
    }
}
