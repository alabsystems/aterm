// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Whole-fleet admission never repairs malformed assignments into a different priority.
/// Valid legacy names retain their normalized order, distinct membership and explicit class.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fleet_machine_assignment_model() -> Model {
    aterm_spec::ty_model! {
        FleetMachineAssignment {
            const Buggy = 0;
            var case = 0; var accepted = 0; var exact = 1;
            action Missing when (case == 0) {
                case = 1; accepted = if Buggy == 1 {1} else {0};
            }
            action InvalidMachine when (case == 0) {
                case = 2; accepted = if Buggy == 2 {1} else {0};
            }
            action WrongType when (case == 0) {
                case = 3; accepted = if Buggy == 3 {1} else {0};
            }
            action InvalidClass when (case == 0) {
                case = 4; accepted = if Buggy == 4 {1} else {0};
            }
            action Scalar when (case == 0) {
                case = 5; accepted = if Buggy == 5 {0} else {1};
                exact = if Buggy == 6 {0} else {1};
            }
            action Distinct when (case == 0) {
                case = 6; accepted = if Buggy == 9 {0} else {1};
                exact = if Buggy == 7 {0} else {1};
            }
            action Duplicate when (case == 0) {
                case = 7; accepted = 1; exact = if Buggy == 8 {0} else {1};
            }
            action Override when (case == 0) {
                case = 8; accepted = 1; exact = if Buggy == 10 {0} else {1};
            }
            invariant ValidMachines: case == 0 || case > 3 || accepted == 0;
            invariant ValidClass: 4 > case || case > 4 || accepted == 0;
            invariant ValidAssignment: 5 > case || accepted == 1;
            invariant ExactAssignment: accepted == 0 || exact == 1;
        }
    }
}
