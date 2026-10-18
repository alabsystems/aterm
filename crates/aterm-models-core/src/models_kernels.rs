// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two small kernels: the run-length codec's local compaction and the grapheme scanner's
//! ASCII fast path.

use aterm_spec::derive::Model;

/// Local compaction around the spliced `mid` run in `Rle::split_range_*`.
///
/// Models a 3-slot neighborhood `(left_val, mid_val, right_val)` surrounded by
/// compact outer neighbors `(outer_left_val != left_val, right_val != outer_right_val)`:
/// * At `Buggy = 0`, `compact_around(mid)` merges `mid` with `left` if equal and
///   with `right` if equal, establishing `NoAdjacentEqual` across the whole
///   neighborhood in $O(1)$ without scanning the outer runs.
/// * At `Buggy = 1`, omitting the right-neighbor check after a left merge leaves
///   adjacent runs with identical values when `left_val == mid_val == right_val`
///   (e.g. healing `[1, 2, 1]` by setting the middle run to `1`), caught by
///   `NoAdjacentEqual`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rle_local_compact_model() -> Model {
    aterm_spec::ty_model! {
        RleLocalCompactModel {
            const Buggy = 0;

            var left_val = 1;
            var mid_val = 2;
            var right_val = 1;
            var merged_left = 0;
            var merged_right = 0;
            var compacted = 0;

            action SpliceSameAsNeighbors when (compacted == 0 && mid_val > 1) {
                mid_val = 1;
            }

            action SpliceDifferent when (compacted == 0 && mid_val == 2) {
                mid_val = 3;
            }

            action CompactAroundMid when (compacted == 0) {
                merged_left = if left_val == mid_val { 1 } else { 0 };
                merged_right = if Buggy == 1 && left_val == mid_val {
                    0
                } else {
                    if mid_val == right_val { 1 } else { 0 }
                };
                compacted = 1;
            }

            invariant NoAdjacentEqual:
                compacted == 0
                || ((left_val > mid_val || mid_val > left_val || merged_left == 1)
                    && (mid_val > right_val || right_val > mid_val || merged_right == 1));
        }
    }
}

/// Printable ASCII pair fast path in `aterm_grapheme::find_first_cluster_end`.
///
/// Models a 2-character prefix `(c0_ascii, c1_ascii, c1_extend)`:
/// * At `Buggy = 0`, the $O(1)$ fast path returns `cluster_end = 1` only when
///   BOTH `c0_ascii == 1` and `c1_ascii == 1` (`c1_extend == 0`), falling back
///   to the full UAX #29 scanner when `c1` is a non-ASCII combining mark
///   (`c1_extend == 1`), proving `CombiningMarkNeverSplit`.
/// * At `Buggy = 1`, returning `cluster_end = 1` after checking only `c0_ascii == 1`
///   splits a base ASCII character from its following combining mark (`a\u{0301}`),
///   caught by `CombiningMarkNeverSplit`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ascii_grapheme_fast_path_model() -> Model {
    aterm_spec::ty_model! {
        AsciiGraphemeFastPath {
            const Buggy = 0;

            var c0_ascii = 1;
            var c1_ascii = 1;
            var c1_extend = 0;
            var decided = 0;
            var cluster_end = 0;

            action SecondIsCombiningMark when (decided == 0 && c1_ascii == 1) {
                c1_ascii = 0;
                c1_extend = 1;
            }

            action SecondIsAscii when (decided == 0 && c1_ascii == 0) {
                c1_ascii = 1;
                c1_extend = 0;
            }

            action FindClusterEnd when (decided == 0) {
                cluster_end = if Buggy == 1 {
                    if c0_ascii == 1 { 1 } else { 2 }
                } else {
                    if c0_ascii == 1 && c1_ascii == 1 {
                        1
                    } else {
                        if c1_extend == 1 { 2 } else { 1 }
                    }
                };
                decided = 1;
            }

            invariant CombiningMarkNeverSplit:
                decided == 0 || c1_extend == 0 || cluster_end == 2;
        }
    }
}
