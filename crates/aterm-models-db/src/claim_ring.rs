// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One Mac's ring of day tables for account claims (`aterm_db::catalog::ring`,
/// `aterm_db::accounts::claims`). The clock runs two ticks to a day (`day`, `tick`) and may be set
/// back a day: no further than the floor (`ClockBack`), or below it, as a clock that ran ahead and
/// came back, or one set back days (`ClockCorrected`). `e` is the current day: the clock's, but
/// the day after the `floor` while the clock is at most a day behind it. Each write, window
/// reservation (`Enroll`) and retirement first raises the floor to `e - 1`, never lowering it,
/// and deletes every held day outside the window `e - 1 ..= e + 1` and below the floor (`h1`..`h6`
/// hold days 1..6); a held day at or above the floor stays while the clock is behind it, and so
/// does this Mac's row there (`row`, out of sight above the window). No write is made while the
/// clock is behind the floor; a write puts this Mac's row in the latest of the current day's
/// table, its own later one where it reads it and the floor's, lapsing a day (two ticks) after
/// it is written; a reservation takes up every window day, below the floor too, for reading.
/// `gone` is the day after the latest held day deleted here; `lost` records a table deleted below
/// the window that still held a live row, `reentered` a write into a day not after `gone`, where
/// a fresh table could fork this Mac's own signed history, and `blind` a write while this Mac's
/// own row sits above the window, where every peer reads it above the new one. At most three held
/// days lie below the floor (the window's, for reading) and three at or above it
/// (`TablesBounded`).
///
/// Mutants: 1 rows outlive the period (`until` four ticks ahead), 2 deletes the window's first
/// day, 3 never deletes, 4 keeps no floor, 5 keeps the window above the floor whatever the clock
/// (a clock back from running ahead stays blind), 6 writes below the floor, 7 deletes every held
/// day outside the window (a clock set back days, then righted, writes into a fresh table of a
/// day it wrote before), 8 writes at the floor while the clock is behind it (a clock set back two
/// days claims on a view without its own row of its peers' today).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claim_ring_model() -> Model {
    aterm_spec::ty_model! {
        DbClaimRing {
            const Buggy = 0;
            var day = 2; var tick = 0; var floor = 0; var e = 2;
            var row = 0; var until = 0;
            var h1 = 0; var h2 = 0; var h3 = 0; var h4 = 0; var h5 = 0; var h6 = 0;
            var gone = 0; var lost = 0; var reentered = 0; var blind = 0;
            action Tick when (5 > day) {
                tick = if tick == 1 {0} else {1};
                day = if tick == 1 {day + 1} else {day};
                e = if tick == 1 {
                    if Buggy == 4 {day + 1} else {
                        if Buggy == 5 {if day + 1 > floor {day + 1} else {floor + 1}} else {
                            if floor > day + 1 {day + 1} else {
                                if day + 1 > floor {day + 1} else {floor + 1}}}}
                } else {e};
            }
            action ClockBack when (day > 2 && day > floor) {
                day = day - 1;
                e = if Buggy == 4 {day - 1} else {if day - 1 > floor {day - 1} else {floor + 1}};
            }
            action ClockCorrected when (day > 2 && day <= floor) {
                day = day - 1;
                e = if Buggy == 5 {floor + 1} else {day - 1};
            }
            action Retire {
                floor = if Buggy == 3 || Buggy == 4 {floor} else {if floor > e - 1 {floor} else {e - 1}};
                gone = if h6 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6
                    || (6 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 6)) && 7 > gone {7}
                    else {if h5 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5
                    || (5 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 5)) && 6 > gone {6}
                    else {if h4 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4
                    || (4 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 4)) && 5 > gone {5}
                    else {if h3 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3
                    || (3 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 3)) && 4 > gone {4}
                    else {if h2 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2
                    || (2 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 2)) && 3 > gone {3}
                    else {if h1 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1
                    || (1 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 1)) && 2 > gone {2}
                    else {gone}}}}}};
                lost = if row > 0 && (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row
                    && until > day + day + tick {1} else {lost};
                row = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0}
                    else {if row > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > row {0} else {row}};
                until = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0}
                    else {if row > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > row {0} else {until}};
                h1 = if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1
                    || (1 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 1)) {0} else {h1};
                h2 = if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2
                    || (2 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 2)) {0} else {h2};
                h3 = if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3
                    || (3 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 3)) {0} else {h3};
                h4 = if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4
                    || (4 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 4)) {0} else {h4};
                h5 = if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5
                    || (5 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 5)) {0} else {h5};
                h6 = if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6
                    || (6 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 6)) {0} else {h6};
            }
            action Enroll {
                floor = if Buggy == 3 || Buggy == 4 {floor} else {if floor > e - 1 {floor} else {e - 1}};
                gone = if h6 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6
                    || (6 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 6)) && 7 > gone {7}
                    else {if h5 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5
                    || (5 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 5)) && 6 > gone {6}
                    else {if h4 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4
                    || (4 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 4)) && 5 > gone {5}
                    else {if h3 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3
                    || (3 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 3)) && 4 > gone {4}
                    else {if h2 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2
                    || (2 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 2)) && 3 > gone {3}
                    else {if h1 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1
                    || (1 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 1)) && 2 > gone {2}
                    else {gone}}}}}};
                lost = if row > 0 && (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row
                    && until > day + day + tick {1} else {lost};
                row = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0}
                    else {if row > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > row {0} else {row}};
                until = if (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row {0}
                    else {if row > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > row {0} else {until}};
                h1 = if 3 > e {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1
                    || (1 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 1)) {0} else {h1}};
                h2 = if 4 > e && e + 2 > 2 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2
                    || (2 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 2)) {0} else {h2}};
                h3 = if 5 > e && e + 2 > 3 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3
                    || (3 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 3)) {0} else {h3}};
                h4 = if 6 > e && e + 2 > 4 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4
                    || (4 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 4)) {0} else {h4}};
                h5 = if 7 > e && e + 2 > 5 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5
                    || (5 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 5)) {0} else {h5}};
                h6 = if e + 2 > 6 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6
                    || (6 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 6)) {0} else {h6}};
            }
            // A write is the retirement's deletions, then the latest of the current day, this
            // Mac's own later day (its row's, where that survives: `e + 1` at most) and the
            // floor; it is enabled while that lies in the window.
            action Write when ((Buggy == 6 || Buggy == 8 || floor <= day) && (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) <= e + 1) {
                floor = if Buggy == 3 || Buggy == 4 {floor} else {if floor > e - 1 {floor} else {e - 1}};
                gone = if h6 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6
                    || (6 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 6)) && 7 > gone {7}
                    else {if h5 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5
                    || (5 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 5)) && 6 > gone {6}
                    else {if h4 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4
                    || (4 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 4)) && 5 > gone {5}
                    else {if h3 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3
                    || (3 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 3)) && 4 > gone {4}
                    else {if h2 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2
                    || (2 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 2)) && 3 > gone {3}
                    else {if h1 == 1 && ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1
                    || (1 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 1)) && 2 > gone {2}
                    else {gone}}}}}};
                lost = if row > 0 && (if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > row
                    && until > day + day + tick {1} else {lost};
                until = day + day + tick + (if Buggy == 1 {4} else {2});
                row = (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})});
                h1 = if (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) == 1 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 1
                    || (1 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 1)) {0} else {h1}};
                h2 = if (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) == 2 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 2
                    || (2 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 2)) {0} else {h2}};
                h3 = if (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) == 3 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 3
                    || (3 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 3)) {0} else {h3}};
                h4 = if (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) == 4 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 4
                    || (4 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 4)) {0} else {h4}};
                h5 = if (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) == 5 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 5
                    || (5 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 5)) {0} else {h5}};
                h6 = if (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) == 6 {1}
                    else {if ((if Buggy == 2 {e} else {if Buggy == 3 {0} else {e - 1}}) > 6
                    || (6 > (if Buggy == 3 {7} else {e + 1}) && (if Buggy == 7 {7} else {floor}) > 6)) {0} else {h6}};
                blind = if row > e + 1 {1} else {blind};
                reentered = if gone > (if (if Buggy == 6 {0} else {floor}) > (if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})
                    {floor} else {(if Buggy == 3 {if row > e {row} else {e}} else {if row == e + 1 {e + 1} else {e}})}) {1} else {reentered};
            }
            invariant NoLiveRowRetired: lost == 0;
            invariant TablesBounded: (if floor > 1 {h1} else {0}) + (if floor > 2 {h2} else {0})
                + (if floor > 3 {h3} else {0}) + (if floor > 4 {h4} else {0})
                + (if floor > 5 {h5} else {0}) + (if floor > 6 {h6} else {0}) <= 3
                && (if floor > 1 {0} else {h1}) + (if floor > 2 {0} else {h2})
                + (if floor > 3 {0} else {h3}) + (if floor > 4 {0} else {h4})
                + (if floor > 5 {0} else {h5}) + (if floor > 6 {0} else {h6}) <= 3;
            invariant NoReentry: reentered == 0;
            invariant WindowHoldsTheClock: e <= day + 1;
            invariant NoBlindWrite: blind == 0;
        }
    }
}
