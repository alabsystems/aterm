// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Failed preparation excludes default and dedicated rows alike, while a pin
/// remains authoritative both before and after preparing an alternative.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_preparation_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolPreparation {
            const Buggy = 0;
            var moving = 0; var default = 0; var case = 0; var phase = 0;
            var returned = 0; var attempts = 0; var committed = 0; var target = 1;
            var details = 1; var failed = 0; var rows = 2; var prepared = 0; var retained = 1;
            action ChooseMove when (phase == 0 && moving == 0) { moving = 1; rows = 3; }
            action FailedDefault when (phase == 0 && default == 0) { default = 1; }
            action AnotherCase when (phase == 0 && case <= 10) { case = case + 1; }
            action FailFirst when (phase == 0) {
                attempts = if Buggy == 3 {3} else if Buggy == 4 {0} else {1};
                failed = if Buggy == 9 {0} else {1};
                retained = if Buggy == 11 {0} else {1};
                rows = if Buggy == 10 {rows - 1} else {rows};
                phase = if case == 2 || (moving == 1 && (case == 4 || case == 8)) {3} else {1};
            }
            action TryAlternative when (phase == 1) {
                attempts = if Buggy == 3 {3} else if Buggy == 4 {0} else {2};
                prepared = if case == 1 {0} else {1};
                phase = if case == 1 {3} else {2};
            }
            action Finish when (phase == 2) {
                phase = 3;
                returned = if Buggy == 1 {1} else if Buggy == 2 {0}
                    else if case == 0 || case == 7 || (moving == 0 && (case == 4 || case == 8)) {1} else {0};
                committed = if Buggy == 5 {1} else if Buggy == 6 {0}
                    else if moving == 1 && (case == 0 || case == 7) {1} else {0};
                target = if Buggy == 7 {0} else {1};
                details = if Buggy == 8 {0} else {1};
            }
            invariant Result: phase <= 2 || returned == if case == 0 || case == 7 || (moving == 0 && (case == 4 || case == 8)) {1} else {0};
            invariant Attempts: attempts <= 2 && (phase <= 2 || attempts == if case == 2 || (moving == 1 && (case == 4 || case == 8)) {1} else {2});
            invariant Commit: phase <= 2 || committed == if moving == 1 && (case == 0 || case == 7) {1} else {0};
            invariant Target: target == 1;
            invariant Details: details == 1;
            invariant FailureObserved: phase == 0 || failed == 1;
            invariant RosterBound: rows == 2 + moving;
            invariant Evidence: retained == 1;
        }
    }
}
