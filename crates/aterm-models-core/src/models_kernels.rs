// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Models for RLE local compaction and the grapheme scanner's ASCII fast path.

use aterm_spec::derive::Model;

/// Local compaction around the spliced `mid` run in `Rle::split_range_*`.
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

/// Concurrent gate protocol of `once_cell::sync::OnceCell::get_or_try_init`.
///
/// Models two concurrent callers over the fast-path `OnceLock::get` check, the
/// `Mutex<()>` critical section (`gate`), the under-lock recheck, fallible vs
/// succeeding user closures, and `OnceLock::set`. `Buggy = 1` removes `gate`
/// (leaving check, compute, `OnceLock::set`, `OnceLock::get`); `Buggy = 2`
/// additionally returns each caller's own computed value instead of re-reading
/// the cell after `OnceLock::set`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn once_cell_gate_protocol_model() -> Model {
    aterm_spec::ty_model! {
        OnceCellGateProtocol {
            const Buggy = 0;

            var cell = 0;
            var gate = 0;
            var ok_runs = 0;
            var ran_ok1 = 0;
            var ran_ok2 = 0;
            var p1 = 0;
            var p2 = 0;
            var obs1 = 0;
            var obs2 = 0;

            action Call1 when (p1 == 0) {
                obs1 = if cell > 0 { cell } else { obs1 };
                p1 = if cell > 0 { 3 } else { 1 };
            }

            action Call2 when (p2 == 0) {
                obs2 = if cell > 0 { cell } else { obs2 };
                p2 = if cell > 0 { 3 } else { 1 };
            }

            action Enter1 when (p1 == 1 && (Buggy == 1 || Buggy == 2 || gate == 0)) {
                gate = if Buggy == 1 || Buggy == 2 {
                    gate
                } else {
                    if cell > 0 { 0 } else { 1 }
                };
                obs1 = if Buggy == 0 && cell > 0 { cell } else { obs1 };
                p1 = if Buggy == 0 && cell > 0 { 3 } else { 2 };
            }

            action Enter2 when (p2 == 1 && (Buggy == 1 || Buggy == 2 || gate == 0)) {
                gate = if Buggy == 1 || Buggy == 2 {
                    gate
                } else {
                    if cell > 0 { 0 } else { 2 }
                };
                obs2 = if Buggy == 0 && cell > 0 { cell } else { obs2 };
                p2 = if Buggy == 0 && cell > 0 { 3 } else { 2 };
            }

            action InitErr1 when (p1 == 2) {
                gate = if gate == 1 { 0 } else { gate };
                p1 = 0;
            }

            action InitErr2 when (p2 == 2) {
                gate = if gate == 2 { 0 } else { gate };
                p2 = 0;
            }

            action InitOk1 when (p1 == 2 && ok_runs <= 1) {
                cell = if cell == 0 { 1 } else { cell };
                obs1 = if Buggy == 2 || cell == 0 { 1 } else { cell };
                ok_runs = ok_runs + 1;
                ran_ok1 = 1;
                gate = if gate == 1 { 0 } else { gate };
                p1 = 3;
            }

            action InitOk2 when (p2 == 2 && ok_runs <= 1) {
                cell = if cell == 0 { 2 } else { cell };
                obs2 = if Buggy == 2 || cell == 0 { 2 } else { cell };
                ok_runs = ok_runs + 1;
                ran_ok2 = 1;
                gate = if gate == 2 { 0 } else { gate };
                p2 = 3;
            }

            bound DomainBounds:
                cell <= 2
                && gate <= 2
                && ok_runs <= 2
                && ran_ok1 <= 1
                && ran_ok2 <= 1
                && p1 <= 3
                && p2 <= 3
                && obs1 <= 2
                && obs2 <= 2;

            invariant ReturnedValueMatchesCell:
                (p1 <= 2 || (cell > 0 && obs1 == cell))
                && (p2 <= 2 || (cell > 0 && obs2 == cell));

            invariant AtMostOneRunningInitializer:
                p1 <= 1 || p1 > 2 || p2 <= 1 || p2 > 2;

            invariant AtMostOneSucceededInitializer:
                ok_runs <= 1;

            invariant SucceededInitializerSetCell:
                (ran_ok1 == 0 || cell == 1) && (ran_ok2 == 0 || cell == 2);
        }
    }
}

/// Three-rule expansion model of `cfg_if::cfg_if!` (`entry`, `@__arms`, `@__group`).
///
/// Models a chain of three arms (`p1`, `p2`, and `p3` or `else` controlled by `has_else`)
/// where each arm body carries two items:
/// - `Buggy = 1` omits `not(any(seen))` from subsequent arm guards (the last-match-wins /
///   overlapping-arm tripwire).
/// - `Buggy = 2` attaches `#[cfg(g_i)]` directly to the arm's first item instead of a
///   single `@__group` splice, leaking the second item of dead arms (upstream issue #90).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cfg_if_expansion_model() -> Model {
    aterm_spec::ty_model! {
        CfgIfExpansion {
            const Buggy = 0;

            var p1 = 0;
            var p2 = 0;
            var p3 = 0;
            var has_else = 1;
            var phase = 0;
            var seen_true = 0;
            var selected = 0;
            var emitted_arms = 0;
            var leaked_dead_items = 0;
            var depth = 0;

            action SetP1 when (phase == 0 && p1 == 0) {
                p1 = 1;
            }

            action SetP2 when (phase == 0 && p2 == 0) {
                p2 = 1;
            }

            action SetP3 when (phase == 0 && p3 == 0) {
                p3 = 1;
            }

            action OmitElse when (phase == 0 && has_else == 1) {
                has_else = 0;
            }

            action Enter when (phase == 0) {
                phase = 1;
                depth = 1;
            }

            action StepArm1 when (phase == 1) {
                selected = if p1 == 1 { 1 } else { 0 };
                emitted_arms = if p1 == 1 { 1 } else { 0 };
                leaked_dead_items = if Buggy == 2 && p1 == 0 {
                    leaked_dead_items + 1
                } else {
                    leaked_dead_items
                };
                seen_true = if p1 == 1 { 1 } else { 0 };
                depth = depth + 1;
                phase = 2;
            }

            action StepArm2 when (phase == 2) {
                selected = if p2 == 1 && (Buggy == 1 || seen_true == 0) {
                    2
                } else {
                    selected
                };
                emitted_arms = if p2 == 1 && (Buggy == 1 || seen_true == 0) {
                    emitted_arms + 1
                } else {
                    emitted_arms
                };
                leaked_dead_items = if Buggy == 2 && (p2 == 0 || seen_true == 1) {
                    leaked_dead_items + 1
                } else {
                    leaked_dead_items
                };
                seen_true = if seen_true == 1 || p2 == 1 { 1 } else { 0 };
                depth = depth + 1;
                phase = 3;
            }

            action StepArm3 when (phase == 3) {
                selected = if (has_else == 1 || p3 == 1) && (Buggy == 1 || seen_true == 0) {
                    3
                } else {
                    selected
                };
                emitted_arms = if (has_else == 1 || p3 == 1) && (Buggy == 1 || seen_true == 0) {
                    emitted_arms + 1
                } else {
                    emitted_arms
                };
                leaked_dead_items = if Buggy == 2 && ((has_else == 0 && p3 == 0) || seen_true == 1) {
                    leaked_dead_items + 1
                } else {
                    leaked_dead_items
                };
                seen_true = if seen_true == 1 || has_else == 1 || p3 == 1 { 1 } else { 0 };
                depth = depth + 1;
                phase = 4;
            }

            action BaseEmpty when (phase == 4) {
                depth = depth + 2;
                phase = 5;
            }

            bound LinearRecursionDepth:
                p1 <= 1
                && p2 <= 1
                && p3 <= 1
                && has_else <= 1
                && phase <= 5
                && seen_true <= 1
                && selected <= 3
                && emitted_arms <= 3
                && leaked_dead_items <= 3
                && depth <= 6;

            invariant FirstMatchSelected:
                phase <= 4
                || ((p1 == 0 || (selected == 1 && emitted_arms == 1))
                    && (p1 == 1 || p2 == 0 || (selected == 2 && emitted_arms == 1))
                    && (p1 == 1 || p2 == 1 || (has_else == 0 && p3 == 0) || (selected == 3 && emitted_arms == 1))
                    && (p1 == 1 || p2 == 1 || has_else == 1 || p3 == 1 || (selected == 0 && emitted_arms == 0)));

            invariant DeadArmFullyAbsent:
                leaked_dead_items == 0;
        }
    }
}
