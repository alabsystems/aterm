// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cell-slice ownership across fixed-width row-count changes.

use aterm_spec::derive::Model;

/// `Buggy=1` drops a shrinking row's slice without returning it to the pool (the resize leak).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resize_row_reuse_model() -> Model {
    aterm_spec::ty_model! {
        ResizeRowReuse {
            const MaxRows = 3;
            const Buggy = 0;
            const BypassReuse = 0;
            var live = 2;
            var free = 0;
            var allocated = 2;
            var peak = 2;

            action Shrink when (live > 1) {
                live = live - 1;
                free = if Buggy == 1 && BypassReuse == 0 { free } else { free + 1 };
            }

            action Grow when (live <= MaxRows - 1 && allocated <= MaxRows) {
                live = live + 1;
                free = if free > 0 && (Buggy == 0 || BypassReuse == 0) {
                    free - 1
                } else { free };
                allocated = if free > 0 && (Buggy == 0 || BypassReuse == 0) {
                    allocated
                } else { allocated + 1 };
                peak = if live + 1 > peak { live + 1 } else { peak };
            }

            action Rebuild {
                free = 0;
                allocated = live;
                peak = live;
            }

            action Output {
                live = live;
            }

            invariant OwnedExactlyOnce: allocated == live + free;
            invariant BoundedByHighWater: allocated <= peak;
        }
    }
}
