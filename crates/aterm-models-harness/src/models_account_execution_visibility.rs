// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The actual execution gate publishes a process-bound observation independently
/// of automatic rotation. Its reader does not mutate account execution authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_execution_visibility_model() -> Model {
    aterm_spec::ty_model! {
        AccountExecutionVisibility {
            const Buggy = 0;
            var case = 0; var stage = 0;
            // Visible means the gate finding remains present, including during current work.
            // View: absent, waiting, disabled, watching, probing, switching, verifying, sign-in, unknown.
            var hold = 0; var published = 0; var visible = 0; var view = 0;
            var counts_unknown = 1; var deadline = 0; var authority = 1; var generic_safe = 1;
            // Retry schedules another display observation, not a provider attempt.
            var retry = 0;
            // Time may pass without another gate call. Wake is a display observation only.
            var elapsed = 0; var current = 0; var historical = 0; var wake = 0;
            action Case when (stage == 0 && 21 > case) { case = case + 1; }
            action Elapse when (stage == 1 && elapsed == 0) { elapsed = 1; }
            action Observe when (stage == 0) {
                stage = 1;
                hold = if Buggy == 10 && case == 0 {0}
                    else if case == 2 || case == 3 || case == 12 || case == 13 {0} else {1};
                published = if (Buggy == 2 && case == 7) || (Buggy == 7 && case == 2) {1}
                    else if Buggy == 8 && case == 13 {0}
                    else if case == 2 || case == 3 || case == 7 || case == 12 {0} else {1};
                authority = if Buggy == 12 {0} else {1};
            }
            action Project when (stage == 1) {
                stage = 2;
                visible = if (Buggy == 3 && case == 8) || (Buggy == 4 && case == 9)
                    || (Buggy == 5 && case == 10) || (Buggy == 6 && case == 11)
                    || (Buggy == 7 && case == 2) {1}
                    else if (Buggy == 1 && case == 0) || (Buggy == 8 && case == 13) {0}
                    else if case <= 1 || (4 <= case && case <= 6) || case == 13
                        || case == 14 || 17 <= case {1} else {0};
                view = if (Buggy == 3 && case == 8) || (Buggy == 4 && case == 9)
                    || (Buggy == 5 && case == 10) || (Buggy == 6 && case == 11)
                    || (Buggy == 7 && case == 2) {1}
                    else if (Buggy == 1 && case == 0) || (Buggy == 8 && case == 13) {2}
                    else if Buggy == 13 && case == 3 {0}
                    else if Buggy == 15 && case == 17 {1}
                    else if Buggy == 20 && case == 0 && elapsed == 1 {1}
                    else if case == 6 {5}
                    else if 17 <= case && case <= 20 {case - 13}
                    else if elapsed == 1 && (case <= 1 || case == 4 || case == 5
                        || case == 13 || case == 14 || case == 21) {8}
                    else if case <= 1 || case == 4 || case == 5 || case == 13
                        || case == 14 || case == 21 {1}
                    else if case == 2 || case == 12 {2} else if case == 3 {3} else {0};
                counts_unknown = if Buggy == 9 && case == 0 {0} else {1};
                deadline = if Buggy == 11 && case == 0 {0}
                    else if case == 20 {1}
                    else if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6) || case == 13
                        || case == 14 || 17 <= case) {1} else {0};
                generic_safe = if Buggy == 14 && case == 14 {0} else {1};
                retry = if Buggy == 16 && case == 21 {0}
                    else if case == 6 || 17 <= case {1} else {0};
                current = if Buggy == 17 && case == 0 && elapsed == 1 {1}
                    else if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6)
                        || case == 13 || case == 14 || 17 <= case) {1} else {0};
                historical = if Buggy == 19 && case == 0 && elapsed == 1 {0}
                    else if elapsed == 1 && (case <= 1 || (4 <= case && case <= 6)
                        || case == 13 || case == 14 || 17 <= case) {1} else {0};
                wake = if Buggy == 18 && case == 0 && elapsed == 0 {0}
                    else if case == 6 || 17 <= case || (elapsed == 0 && (case <= 1
                        || case == 4 || case == 5 || case == 13 || case == 14)) {1} else {0};
            }
            invariant ActualGateResult: stage == 0 || hold ==
                if case == 2 || case == 3 || case == 12 || case == 13 {0} else {1};
            invariant PublicationFenced: stage == 0 || published ==
                if case == 2 || case == 3 || case == 7 || case == 12 {0} else {1};
            invariant HoldVisibility: stage <= 1 || visible ==
                if case <= 1 || (4 <= case && case <= 6) || case == 13
                    || case == 14 || 17 <= case {1} else {0};
            invariant RotationOffKeepsGate: stage <= 1 || case > 0
                || (visible == 1 && view == if elapsed == 0 {1} else {8});
            invariant NormalPhaseRestored: stage <= 1 || view ==
                if case == 6 {5}
                else if 17 <= case && case <= 20 {case - 13}
                else if elapsed == 1 && (case <= 1 || case == 4 || case == 5
                    || case == 13 || case == 14 || case == 21) {8}
                else if case <= 1 || case == 4 || case == 5 || case == 13
                    || case == 14 || case == 21 {1}
                else if case == 2 || case == 12 {2} else if case == 3 {3} else {0};
            invariant CurrentWorkVisible: stage <= 1 || case <= 5 || (7 <= case && case <= 16)
                || case == 21 || view == if case == 6 {5} else {case - 13};
            invariant GuardCountsUnjudged: visible == 0 || counts_unknown == 1;
            invariant ActualRecheckShown: stage <= 1 || deadline ==
                if case == 20 {1} else if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6) || case == 13
                    || case == 14 || 17 <= case) {1} else {0};
            invariant AuthorityPreserved: authority == 1;
            invariant UnknownBirthIsGeneric: generic_safe == 1;
            invariant UnobservedActivityRechecks: stage <= 1 || retry ==
                if case == 6 || 17 <= case {1} else {0};
            invariant CurrentHoldRequiresUnexpiredLease: stage <= 1 || current ==
                if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6)
                    || case == 13 || case == 14 || 17 <= case) {1} else {0};
            invariant ExpiredFindingIsHistorical: stage <= 1 || historical ==
                if elapsed == 1 && (case <= 1 || (4 <= case && case <= 6)
                    || case == 13 || case == 14 || 17 <= case) {1} else {0};
            invariant LeaseExpirySchedulesObservation: stage <= 1 || wake ==
                if case == 6 || 17 <= case || (elapsed == 0 && (case <= 1
                    || case == 4 || case == 5 || case == 13 || case == 14)) {1} else {0};
            bound StageRange: stage <= 2;
            bound CaseRange: case <= 21;
            bound ElapsedRange: elapsed <= 1;
        }
    }
}
