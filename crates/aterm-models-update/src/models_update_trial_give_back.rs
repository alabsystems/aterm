// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Model of boot-trial launch give-backs by launch name under overlapping launches.

use aterm_spec::derive::Model;

/// Boot-trial launch count model under overlapping candidate and sibling launches.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_trial_give_back_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateTrialGiveBack {
            const Buggy = 0;
            var count = 1;
            var phase = 0;
            var before = 0;
            var at_boot = 0;
            var mine = 0;
            var sibling = 0;
            var theirs = 0;
            var sib_at = 0;
            var kept = 0;
            var owed = 0;

            action SiblingBoots when (sibling == 0) {
                sibling = 1;
                sib_at = phase;
                theirs = if phase == 2 { 0 } else { 1 };
                count = if phase == 2 {
                    count
                } else {
                    if Buggy == 1 { count + 1 } else { count + 1 - owed }
                };
                mine = if Buggy == 1 { mine } else { mine - owed };
                owed = if Buggy == 1 { owed } else { 0 };
            }
            action SiblingLaunchedCold when (sibling == 0) {
                sibling = 4;
                sib_at = phase;
                theirs = 1;
                count = if Buggy == 1 { count + 1 } else { count + 1 - owed };
                mine = if Buggy == 1 { mine } else { mine - owed };
                owed = if Buggy == 1 { owed } else { 0 };
            }
            action SiblingGivesBack when (sibling == 1) {
                sibling = 2;
                theirs = 0;
                count = count - theirs;
            }
            action SiblingDies when (sibling == 1) {
                sibling = 3;
            }
            action SiblingSettled when (sibling == 3) {
                sibling = 2;
                theirs = 0;
                count = count - theirs;
            }
            action SiblingCrashes when (sibling == 1 || sibling == 4) {
                sibling = 5;
                kept = theirs;
            }
            action Launch when (phase == 0) {
                phase = 1;
                before = count;
            }
            action Boots when (phase == 1) {
                phase = 2;
                at_boot = count;
                mine = if sibling == 1 || sibling == 4 { 0 } else { 1 };
                count = if sibling == 1 || sibling == 4 { count } else { count + 1 };
            }
            action ExitsByRule when (phase == 2) {
                phase = 3;
                count = if Buggy == 1 {
                    if count > 0 && (count > at_boot || at_boot > count) { count - 1 } else { count }
                } else {
                    count - mine
                };
                mine = if Buggy == 1 {
                    if count > 0 && (count > at_boot || at_boot > count) { 0 } else { mine }
                } else {
                    0
                };
            }
            action Killed when (phase == 2) {
                phase = 3;
            }
            action Settled when (phase == 3) {
                phase = 4;
                count = if Buggy == 1 {
                    if count > 0 && (count > before || before > count) { count - 1 } else { count }
                } else {
                    count - mine
                };
                mine = if Buggy == 1 {
                    if count > 0 && (count > before || before > count) { 0 } else { mine }
                } else {
                    0
                };
            }
            action SettledUnnamed when (phase == 3) {
                phase = 4;
                count = if Buggy == 1 || Buggy == 2 {
                    if count > 0 && (count > before || before > count) { count - 1 } else { count }
                } else {
                    if Buggy == 3 {
                        if mine == 1 || (theirs == 1 && (sibling == 3 || sibling == 5) && sib_at > 0) {
                            count - 1
                        } else {
                            count
                        }
                    } else {
                        count - mine
                    }
                };
                mine = if Buggy == 1 || Buggy == 2 {
                    if count > 0 && (count > before || before > count) { 0 } else { mine }
                } else {
                    if Buggy == 3 && theirs == 1 && (sibling == 3 || sibling == 5) && sib_at > 1 {
                        mine
                    } else {
                        0
                    }
                };
                theirs = if Buggy == 3
                    && theirs == 1
                    && (sibling == 3 || sibling == 5)
                    && sib_at > 0
                    && (sib_at > 1 || mine == 0)
                {
                    0
                } else {
                    theirs
                };
            }

            action SettledOwed when (phase == 3) {
                phase = 4;
                owed = mine;
            }
            action PaysOwed when (owed == 1 && (Buggy == 0 || Buggy > 1)) {
                count = count - 1;
                mine = 0;
                owed = 0;
            }

            invariant TheCountIsTheLaunchesOnIt: count == 1 + mine + theirs;
            invariant ASettledLaunchIsOffTheCount: phase <= 3 || mine == owed;
            invariant AKeptCrashStaysCounted: 1 + kept + mine <= count;
            invariant AnOwedLaunchIsPaidBeforeTheNextCount: sib_at <= 3 || owed == 0;
        }
    }
}
