// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the herald's told-once memory: a need its owner was told of is not told again
//! until the agent takes it down or the owner takes a turn, a window restart between them
//! included (design §14.18).

use aterm_spec::derive::Model;

/// One session's herald over two needs, `a` and `b`, shown as `{a}`, `{b}` or `{a, b}` (`shown`
/// 1, 2, 3; 0 for none, or a need of another kind in front), each set under its own entry key
/// (`key` 1, 2, 3; 4 another kind's), alerts on and every limit past. `Swap` brings `{a, b}` in
/// under the key already shown: the told set changed under an entry that reads the same. A
/// transition (a new key, or a need neither told nor shown before) posts one notice naming a
/// need not told yet, one new to the view first, then marks every shown need told; one whose
/// every need was told posts nothing. `named_*` counts the notices naming each need since the
/// last forget, `new_*` whether the last action brought it into view, and `fresh` whether the
/// last action was the owner's turn (1) or the agent's take-down (2). `Restart` is a cold
/// restore: the told memory carries across it and the slot's view does not, so whatever is raised
/// in the new process is judged afresh against that memory, a need told before posting nothing.
/// `carried` marks a carried memory no need has met in the new process yet: a `Clear` then is
/// the restart's gap (nothing raised again yet), not the agent's take-down, so it keeps the memory
/// and the notices' counts; the first need shown, or another kind in front, ends it. Mutants: 1
/// keeps no memory (one notice per new key naming the first need, the herald before told once);
/// 2 keys the memory by the set's first need alone; 3 lets the owner's turn keep the memory; 4
/// lets the take-down keep it; 5 judges news only on a new key; 6 lets a restart forget it, and 7
/// lets the restart's gap forget it, so a need told before is told again in the new process.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn herald_told_once_model() -> Model {
    aterm_spec::ty_model! {
        HeraldToldOnce {
            const Buggy = 0;
            var shown = 0;
            var key = 0;
            var told_a = 0;
            var told_b = 0;
            var named_a = 0;
            var named_b = 0;
            var new_a = 0;
            var new_b = 0;
            var fresh = 0;
            var carried = 0;

            action ShowA {
                told_a = if (key <= 0 || key > 1
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && told_a == 0 && (shown == 0 || shown == 2)))
                    && (Buggy <= 0 || Buggy > 1) {
                    1
                } else { told_a };
                named_a = if (key <= 0 || key > 1
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && told_a == 0 && (shown == 0 || shown == 2)))
                    && (told_a == 0 || Buggy == 1) && named_a <= 1 {
                    named_a + 1
                } else { named_a };
                new_a = if shown == 0 || shown == 2 { 1 } else { 0 };
                new_b = 0;
                shown = 1;
                key = 1;
                fresh = 0;
                carried = 0;
            }

            action ShowB {
                told_b = if (key <= 1 || key > 2
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && told_b == 0 && (shown == 0 || shown == 1)))
                    && (Buggy <= 0 || Buggy > 1) {
                    1
                } else { told_b };
                named_b = if (key <= 1 || key > 2
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && told_b == 0 && (shown == 0 || shown == 1)))
                    && (told_b == 0 || Buggy == 1) && named_b <= 1 {
                    named_b + 1
                } else { named_b };
                new_a = 0;
                new_b = if shown == 0 || shown == 1 { 1 } else { 0 };
                shown = 2;
                key = 2;
                fresh = 0;
                carried = 0;
            }

            action ShowBoth {
                told_a = if (key <= 2 || key > 3
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && ((told_a == 0 && (shown == 0 || shown == 2))
                            || (told_b == 0 && (shown == 0 || shown == 1)))))
                    && (Buggy <= 0 || Buggy > 1) {
                    1
                } else { told_a };
                told_b = if (key <= 2 || key > 3
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && ((told_a == 0 && (shown == 0 || shown == 2))
                            || (told_b == 0 && (shown == 0 || shown == 1)))))
                    && ((Buggy <= 0 || Buggy > 1) && (Buggy <= 1 || Buggy > 2)) {
                    1
                } else { told_b };
                named_a = if (key <= 2 || key > 3
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && ((told_a == 0 && (shown == 0 || shown == 2))
                            || (told_b == 0 && (shown == 0 || shown == 1)))))
                    && (Buggy == 1
                        || (Buggy == 2 && told_a == 0)
                        || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 1 || Buggy > 2))
                            && ((told_a == 0 && (shown == 0 || shown == 2))
                                || (told_a == 0
                                    && (told_b == 1 || shown == 2 || shown == 3)))))
                    && named_a <= 1 {
                    named_a + 1
                } else { named_a };
                named_b = if (key <= 2 || key > 3
                    || (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                        && ((told_a == 0 && (shown == 0 || shown == 2))
                            || (told_b == 0 && (shown == 0 || shown == 1)))))
                    && ((Buggy <= 0 || Buggy > 1) && (Buggy <= 1 || Buggy > 2))
                    && told_b == 0
                    && (told_a == 1 || shown == 1 || shown == 3)
                    && (told_a == 1 || shown == 0 || shown == 1)
                    && named_b <= 1 {
                    named_b + 1
                } else { named_b };
                new_a = if shown == 0 || shown == 2 { 1 } else { 0 };
                new_b = if shown == 0 || shown == 1 { 1 } else { 0 };
                shown = 3;
                key = 3;
                fresh = 0;
                carried = 0;
            }

            action Swap when (shown == 1 || shown == 2) {
                told_a = if (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                    && ((told_a == 0 && shown == 2) || (told_b == 0 && shown == 1))) {
                    1
                } else { told_a };
                told_b = if (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                    && ((told_a == 0 && shown == 2) || (told_b == 0 && shown == 1)))
                    && ((Buggy <= 0 || Buggy > 1) && (Buggy <= 1 || Buggy > 2)
                        && (Buggy <= 4 || Buggy > 5)) {
                    1
                } else { told_b };
                named_a = if (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                    && ((told_a == 0 && shown == 2) || (told_b == 0 && shown == 1)))
                    && told_a == 0 && (shown == 2 || Buggy == 2) && named_a <= 1 {
                    named_a + 1
                } else { named_a };
                named_b = if (((Buggy <= 0 || Buggy > 1) && (Buggy <= 4 || Buggy > 5))
                    && ((told_a == 0 && shown == 2) || (told_b == 0 && shown == 1)))
                    && ((Buggy <= 0 || Buggy > 1) && (Buggy <= 1 || Buggy > 2)
                        && (Buggy <= 4 || Buggy > 5))
                    && told_b == 0 && shown == 1 && named_b <= 1 {
                    named_b + 1
                } else { named_b };
                new_a = if shown == 2 { 1 } else { 0 };
                new_b = if shown == 1 { 1 } else { 0 };
                shown = 3;
                fresh = 0;
                carried = 0;
            }

            action Displace {
                shown = 0;
                key = 4;
                new_a = 0;
                new_b = 0;
                fresh = 0;
                carried = 0;
            }

            action Clear {
                shown = 0;
                key = 0;
                told_a = if Buggy == 4 || (carried == 1 && (Buggy <= 6 || Buggy > 7)) {
                    told_a
                } else { 0 };
                told_b = if Buggy == 4 || (carried == 1 && (Buggy <= 6 || Buggy > 7)) {
                    told_b
                } else { 0 };
                named_a = if carried == 1 { named_a } else { 0 };
                named_b = if carried == 1 { named_b } else { 0 };
                new_a = 0;
                new_b = 0;
                fresh = if carried == 1 { 0 } else { 2 };
                carried = if Buggy == 7 { 0 } else { carried };
            }

            action Restart {
                told_a = if Buggy == 6 { 0 } else { told_a };
                told_b = if Buggy == 6 { 0 } else { told_b };
                carried = if (Buggy <= 5 || Buggy > 6) && (told_a == 1 || told_b == 1) {
                    1
                } else { 0 };
                shown = 0;
                key = 0;
                new_a = 0;
                new_b = 0;
                fresh = 0;
            }

            action OwnerTurn {
                told_a = if Buggy == 3 { told_a } else { 0 };
                told_b = if Buggy == 3 { told_b } else { 0 };
                named_a = 0;
                named_b = 0;
                new_a = 0;
                new_b = 0;
                fresh = 1;
            }

            invariant ToldOnce: named_a <= 1 && named_b <= 1;
            invariant ShownIsTold: (new_a == 0 || told_a == 1) && (new_b == 0 || told_b == 1);
            invariant TurnForgets: fresh <= 0 || fresh > 1 || (told_a == 0 && told_b == 0);
            invariant ClearForgets: fresh <= 1 || (told_a == 0 && told_b == 0);
        }
    }
}
