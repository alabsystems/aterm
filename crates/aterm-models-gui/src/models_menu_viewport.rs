// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A popup's logical row interval, reveal operation and visible-to-logical hit mapping.

use aterm_spec::derive::Model;

/// The pure viewport helper only. Selection/enabling, painting, pointer ownership and
/// accessibility epochs belong to the App integration, not this machine.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn menu_viewport_model() -> Model {
    aterm_spec::ty_model! {
        MenuViewport {
            const Buggy = 0;
            const Rows = 5;
            const Absent = 5;
            var total = 5;
            var shown = 3;
            var first = 0;
            var target = 5;
            var revealed = 0;
            var cell = 0;
            var mapped = 5;
            var hit = 0;

            action RevealFirst when (total > 0) {
                target = 0;
                first = if Buggy == 1 { first } else { 0 };
                revealed = if shown > 0 { 1 } else { 0 };
                hit = 0;
            }
            action RevealLast when (total > 0) {
                target = total - 1;
                first = if shown == 0 { 0 } else {
                    if Buggy == 1 { first } else {
                        if total > first + shown { total - shown } else { first }
                    }
                };
                revealed = if shown > 0 { 1 } else { 0 };
                hit = 0;
            }
            action ScrollDown {
                first = if shown == 0 { 0 } else {
                    if total > first + shown { first + 1 } else { first }
                };
                target = Absent;
                revealed = 0;
                hit = 0;
            }
            action ScrollUp {
                first = if shown == 0 { 0 } else {
                    if first > 0 { first - 1 } else { 0 }
                };
                target = Absent;
                revealed = 0;
                hit = 0;
            }
            action ResizeShort {
                shown = 1;
                first = if total == 0 { 0 } else {
                    if total <= first { total - 1 } else { first }
                };
                target = Absent;
                revealed = 0;
                hit = 0;
            }
            action ResizeTall {
                shown = 3;
                first = if Buggy == 2 { first } else {
                    if total <= 3 { 0 } else {
                        if first > total - 3 { total - 3 } else { first }
                    }
                };
                target = Absent;
                revealed = 0;
                hit = 0;
            }
            action ResizeZero {
                shown = 0;
                first = 0;
                target = Absent;
                revealed = 0;
                hit = 0;
            }
            action Empty {
                total = 0;
                first = 0;
                target = Absent;
                revealed = 0;
                hit = 0;
            }
            action Refill when (total == 0) {
                total = Rows;
                first = 0;
                target = Absent;
                revealed = 0;
                hit = 0;
            }
            action HitFirst {
                cell = 0;
                mapped = if shown > 0 && total > first {
                    if Buggy == 3 { 0 } else { first }
                } else { Absent };
                hit = 1;
            }
            action HitSecond {
                cell = 1;
                mapped = if shown > 1 && total > first + 1 {
                    if Buggy == 3 { 1 } else { first + 1 }
                } else { Absent };
                hit = 1;
            }
            action HitOutside {
                cell = shown;
                mapped = Absent;
                hit = 1;
            }

            bound RowDomain: 0 <= total && total <= Rows;
            bound CapacityDomain: 0 <= shown && shown <= 3;
            bound ScalarBounds: 0 <= first && first <= Rows && 0 <= target && target <= Absent
                && 0 <= cell && cell <= 3 && 0 <= mapped && mapped <= Absent
                && 0 <= revealed && revealed <= 1 && 0 <= hit && hit <= 1;
            invariant ViewportIsNormalized: (shown > 0 || first == 0)
                && first <= if total <= shown { 0 } else { total - shown };
            invariant RequestedRowIsRevealed: revealed == 0 ||
                (shown > 0 && total > target && first <= target && first + shown > target);
            invariant HitNamesVisibleLogicalRow: hit == 0 || mapped ==
                if shown > cell && total > first + cell { first + cell } else { Absent };
        }
    }
}
