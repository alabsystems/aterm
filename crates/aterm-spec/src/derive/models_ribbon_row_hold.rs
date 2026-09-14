// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The row that owns a typing/editing gesture owns its ribbon renewal.

use super::Model;

/// Two already resident, ordinary typing cohorts on different rows, observed
/// over three monotonically increasing gesture stamps. `clock0` and `clock1`
/// project the cohorts' public `alive_at` values to those stamps. Typing or
/// erasing on one row renews that row; it cannot postpone the other row's exit.
/// Returning to the earlier row is permitted and renews its own lease again.
///
/// This is the renewal protocol, not a model of pixel coverage, the continuous
/// fade clock, cohort merging, or relocation. Tier-1 drives public `Ribbon`
/// events and reads actual cohort clocks; a separate production assertion
/// checks that uninterrupted typing below a row lets the earlier row expire.
/// `Buggy=1` restores the historical global finger hold, which silently renewed
/// every resident row whenever the user typed anywhere.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ribbon_row_hold_model() -> Model {
    crate::ty_model! {
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
                clock0 = stamp + 1;
                clock1 = if Buggy == 1 { stamp + 1 } else { clock1 };
                stamp = stamp + 1;
            }
            action TouchRow1 when (stamp <= Steps - 1) {
                prior0 = clock0; prior1 = clock1;
                owner = 1;
                clock1 = stamp + 1;
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
