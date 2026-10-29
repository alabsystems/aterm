// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One Mac's ring of day tables for account claims (`aterm_db::catalog::ring`,
/// `aterm_db::accounts::claims`). The clock runs two ticks to a day (`day`, `tick`) and may be set
/// back a day. `e` is the current day: the clock's, never below the day after the `floor`. Each
/// write, window reservation (`Enroll`) and retirement first raises the floor to `e - 1` and
/// deletes every held day below it (`h1`..`h6` hold days 1..6); a write puts this Mac's row in the
/// current day's table or its own later one, lapsing a day (two ticks) after it is written.
/// `gone` is the lowest day never deleted here; `lost` and `reentered` record a deleted table that
/// still held a live row, and a deleted day enrolled again.
///
/// Mutants: 1 rows outlive the period (`until` four ticks ahead), 2 deletes the window's first
/// day, 3 never deletes, 4 keeps no floor.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claim_ring_model() -> Model {
    aterm_spec::ty_model! {
        DbClaimRing {
            const Buggy = 0;
            var day = 2; var tick = 0; var floor = 0; var e = 2;
            var row = 0; var until = 0;
            var h1 = 0; var h2 = 0; var h3 = 0; var h4 = 0; var h5 = 0; var h6 = 0;
            var gone = 0; var lost = 0; var reentered = 0;
            action Tick when (5 > day) {
                tick = if tick == 1 {0} else {1};
                day = if tick == 1 {day + 1} else {day};
                e = if tick == 1 {
                    if Buggy == 4 {day + 1} else {if day + 1 > floor {day + 1} else {floor + 1}}
                } else {e};
            }
            action ClockBack when (day > 2) {
                day = day - 1;
                e = if Buggy == 4 {day - 1} else {if day - 1 > floor {day - 1} else {floor + 1}};
            }
            action Retire {
                floor = if Buggy == 3 || Buggy == 4 {floor} else {e - 1};
                gone = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > gone
                    {if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}} else {gone};
                lost = if row > 0 && (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row
                    && until > day + day + tick {1} else {lost};
                row = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row};
                until = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {until};
                h1 = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1 {0} else {h1};
                h2 = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2 {0} else {h2};
                h3 = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3 {0} else {h3};
                h4 = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4 {0} else {h4};
                h5 = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5 {0} else {h5};
                h6 = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6 {0} else {h6};
            }
            action Enroll {
                floor = if Buggy == 3 || Buggy == 4 {floor} else {e - 1};
                gone = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > gone
                    {if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}} else {gone};
                lost = if row > 0 && (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row
                    && until > day + day + tick {1} else {lost};
                row = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row};
                until = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {until};
                h1 = if 3 > e {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1 {0} else {h1}};
                h2 = if 4 > e && e + 2 > 2 {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2 {0} else {h2}};
                h3 = if 5 > e && e + 2 > 3 {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3 {0} else {h3}};
                h4 = if 6 > e && e + 2 > 4 {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4 {0} else {h4}};
                h5 = if 7 > e && e + 2 > 5 {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5 {0} else {h5}};
                h6 = if e + 2 > 6 {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6 {0} else {h6}};
                reentered = if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > gone
                    {if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}} else {gone}) > e - 1
                    {1} else {reentered};
            }
            action Write when ((if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) <= e + 1) {
                floor = if Buggy == 3 || Buggy == 4 {floor} else {e - 1};
                gone = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > gone
                    {if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}} else {gone};
                lost = if row > 0 && (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row
                    && until > day + day + tick {1} else {lost};
                row = if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e
                    {row} else {e};
                until = day + day + tick + (if Buggy == 1 {4} else {2});
                h1 = if (if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e {row} else {e}) == 1
                    {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1 {0} else {h1}};
                h2 = if (if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e {row} else {e}) == 2
                    {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2 {0} else {h2}};
                h3 = if (if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e {row} else {e}) == 3
                    {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3 {0} else {h3}};
                h4 = if (if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e {row} else {e}) == 4
                    {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4 {0} else {h4}};
                h5 = if (if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e {row} else {e}) == 5
                    {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5 {0} else {h5}};
                h6 = if (if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e {row} else {e}) == 6
                    {1} else {if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6 {0} else {h6}};
                reentered = if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > gone
                    {if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}} else {gone})
                    > (if (if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0} else {row}) > e {row} else {e})
                    {1} else {reentered};
            }
            invariant NoLiveRowRetired: lost == 0;
            invariant TablesBounded: h1 + h2 + h3 + h4 + h5 + h6 <= 4;
            invariant NoReentry: reentered == 0;
        }
    }
}
