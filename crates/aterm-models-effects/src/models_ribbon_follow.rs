// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow ribbon follow-pass twin-bit and verdict model.

use aterm_spec::derive::Model;

/// The twin contract of the follow pass.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ribbon_follow_twin_model() -> Model {
    aterm_spec::ty_model! {
        RibbonFollowTwin {
            const Buggy = 0;
            const N = 4;
            var i = 0;
            var done = 0;
            var arr = 0;
            var stand = 0;
            var gone = 0;
            var first_arr = 0;
            var last_arr = 0;
            var gap = 0;
            var split = 0;
            var run = 0;
            var lead_run = 0;
            var tail_run = 0;
            var tail_open = 0;
            var found = 0;
            var neutral = 0;
            var after = 0;
            var broken = 0;
            var rlead = 0;
            var lo = 0;
            var hi = 0;
            var named = 0;
            var missed = 0;

            action ScanArrived when (N > i) {
                i = i + 1;
                arr = arr + 1;
                first_arr = if first_arr == 0 { i + 1 } else { first_arr };
                last_arr = i + 1;
                split = if gap == 1 { 1 } else { split };
                lead_run = if arr == 0 { run } else { lead_run };
                run = 0;
                tail_run = 0;
                tail_open = 1;
                found = found + 1;
                broken = if after == 1 { 1 } else { broken };
                lo = if after == 0 && lo == 0 {
                    if rlead == 0 { i + 1 } else { rlead }
                } else { lo };
                hi = if after == 0 { i + 1 } else { hi };
                rlead = if after == 0 { 0 } else { rlead };
            }
            action ScanArrivedGone when (N > i) {
                i = i + 1;
                gone = gone + 1;
                arr = if Buggy == 3 { arr + 2 } else { arr + 1 };
                first_arr = if first_arr == 0 { i + 1 } else { first_arr };
                last_arr = i + 1;
                split = if gap == 1 { 1 } else { split };
                lead_run = if arr == 0 { run } else { lead_run };
                run = 0;
                tail_run = 0;
                tail_open = 1;
                found = found + 1;
                broken = if after == 1 { 1 } else { broken };
                lo = if after == 0 && lo == 0 {
                    if rlead == 0 { i + 1 } else { rlead }
                } else { lo };
                hi = if after == 0 { i + 1 } else { hi };
                rlead = if after == 0 { 0 } else { rlead };
            }
            action ScanStanding when (N > i) {
                i = i + 1;
                stand = stand + 1;
                run = run + 1;
                tail_run = if tail_open == 1 { tail_run + 1 } else { tail_run };
                neutral = if Buggy == 1 { neutral } else { neutral + 1 };
                after = if Buggy == 1 && found > 0 { 1 } else { after };
                rlead = if found == 0 {
                    if Buggy == 1 { 0 } else { if rlead == 0 { i + 1 } else { rlead } }
                } else { rlead };
                hi = if Buggy == 0 && found > 0 && after == 0 { i + 1 } else { hi };
            }
            action ScanStandingGone when (N > i) {
                i = i + 1;
                gone = gone + 1;
                stand = stand + 1;
                run = run + 1;
                tail_run = if tail_open == 1 { tail_run + 1 } else { tail_run };
                neutral = if Buggy == 1 { neutral } else { neutral + 1 };
                after = if Buggy == 1 && found > 0 { 1 } else { after };
                rlead = if found == 0 {
                    if Buggy == 1 { 0 } else { if rlead == 0 { i + 1 } else { rlead } }
                } else { rlead };
                hi = if Buggy == 0 && found > 0 && after == 0 { i + 1 } else { hi };
            }
            action ScanAbsent when (N > i) {
                i = i + 1;
                gap = if arr > 0 { 1 } else { gap };
                run = 0;
                tail_open = 0;
                after = if found > 0 { 1 } else { after };
                rlead = if found == 0 { 0 } else { rlead };
            }
            action ScanAbsentGone when (N > i) {
                i = i + 1;
                gone = gone + 1;
                gap = if arr > 0 { 1 } else { gap };
                run = 0;
                tail_open = 0;
                after = if found > 0 { 1 } else { after };
                rlead = if found == 0 { 0 } else { rlead };
            }
            action Decide when (i == N && done == 0) {
                done = 1;
                named = if broken == 0 && (found > 1 || Buggy == 2) && N <= found + found + neutral
                    && gone > 1 && N <= gone + gone { 1 } else { 0 };
                missed = if broken == 1 && found > 1 && N <= found + found + neutral
                    && gone > 1 && N <= gone + gone { gone } else { 0 };
            }

            invariant AllStandingNeverFollows:
                if done == 1 && arr == 0 { named == 0 } else { named <= 1 };
            invariant AnArrivedBlockFollows:
                if done == 1 && split == 0 && arr > 1 && N <= arr + arr + stand
                    && gone > 1 && N <= gone + gone {
                    named == 1 && lo + lead_run == first_arr && hi == last_arr + tail_run
                } else { named <= 1 };
            invariant OnlyAnArrivedBlockFollows:
                if named == 1 {
                    done == 1 && split == 0 && arr > 1 && N <= arr + arr + stand
                        && gone > 1 && N <= gone + gone
                } else { named == 0 };
            invariant MissedIsAnArrivalNotNamed:
                if done == 1 && split == 1 && arr > 1 && N <= arr + arr + stand
                    && gone > 1 && N <= gone + gone {
                    missed == gone
                } else { missed == 0 };
            invariant StateBounded:
                i <= N && done <= 1 && arr + stand <= i && gone <= i
                    && found <= i && neutral <= i && lo <= N && hi <= N
                    && rlead <= N && named <= 1 && missed <= N;
        }
    }
}

/// The arrival evidence the witness writes: the writer half of [`ribbon_follow_twin_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ribbon_arrival_evidence_model() -> Model {
    aterm_spec::ty_model! {
        RibbonArrivalEvidence {
            const Buggy = 0;
            const TwinMin = 2;
            const T = 5;
            var armed = 0;
            var t = 0;
            var first = 0;
            var streak = 0;
            var stood = 0;
            var clear = 0;
            var twin = 0;
            var pending = 0;
            var pend = 0;

            action ArmSame when (armed == 0) {
                armed = 1;
                first = 1;
                clear = if Buggy == 2 { 0 } else { 1 };
                twin = if Buggy == 3 { 0 } else { 1 };
            }
            action ArmDiff when (armed == 0) {
                armed = 1;
                clear = 1;
            }
            action ArmUnsampled when (armed == 0) {
                armed = 1;
                clear = if Buggy == 2 { 0 } else { 1 };
            }
            action SameAfterOne when (armed == 1 && t + 1 <= T) {
                t = t + 1;
                streak = if streak == 0 { t + 1 } else { streak };
                stood = if streak > 0 && TwinMin <= t + 1 - streak { 1 } else { stood };
                pend = if pending == 0 { t + 1 } else { pend };
                twin = if Buggy == 1 || Buggy == 3 { twin } else {
                    if pending == 1 && TwinMin <= t + 1 - pend { 1 } else { twin }
                };
                pending = 1;
            }
            action SameAfterTwo when (armed == 1 && t + 2 <= T) {
                t = t + 2;
                streak = if streak == 0 { t + 2 } else { streak };
                stood = if streak > 0 && TwinMin <= t + 2 - streak { 1 } else { stood };
                pend = if pending == 0 { t + 2 } else { pend };
                twin = if Buggy == 1 || Buggy == 3 { twin } else {
                    if pending == 1 && TwinMin <= t + 2 - pend { 1 } else { twin }
                };
                pending = 1;
            }
            action DiffAfterOne when (armed == 1 && t + 1 <= T) {
                t = t + 1;
                streak = 0;
                clear = 1;
                pending = 0;
                pend = 0;
            }
            action DiffAfterTwo when (armed == 1 && t + 2 <= T) {
                t = t + 2;
                streak = 0;
                clear = 1;
                pending = 0;
                pend = 0;
            }
            action UnsampledAfterOne when (armed == 1 && t + 1 <= T) {
                t = t + 1;
            }
            action UnsampledAfterTwo when (armed == 1 && t + 2 <= T) {
                t = t + 2;
            }

            invariant CopyThereFirstIsNoArrival:
                if first == 1 { twin == 1 } else { twin <= 1 };
            invariant ACopyThatStoodIsNoArrival:
                if stood == 1 { twin == 1 } else { twin <= 1 };
            invariant AnythingElseIsAnArrival:
                if armed == 1 && first == 0 && stood == 0 {
                    clear == 1 && twin == 0
                } else { clear <= 1 };
            bound StateBounded:
                armed <= 1 && t <= T && first <= 1 && stood <= 1 && clear <= 1
                    && twin <= 1 && pending <= 1 && pend <= T + 1 && streak <= T + 1;
        }
    }
}
