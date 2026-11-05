// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The actual execution gate publishes a process-bound observation independently
/// of automatic rotation. An independent captured context also exposes attributed
/// work before any decision or gate receipt exists. Neither reader changes authority.
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
            // Separately scoped active work does not select the primary decision or gate.
            var work_visible = 0;
            var counts_unknown = 1; var deadline = 0; var authority = 1; var generic_safe = 1;
            // Retry schedules another display observation, not a provider attempt.
            var retry = 0;
            // Time may pass without another gate call. Wake is a display observation only.
            var elapsed = 0; var current = 0; var historical = 0; var wake = 0;
            var activity_rows_changed = 0;
            // Context: missing, current, replaced before publication, replaced after it.
            // Activity: empty, running work, unavailable observation lock, settled work.
            var context = 0; var context_record = 0; var activity = 0;
            var diagnostic_ok = 1;
            // A matching normal receipt: absent, current policy, retired policy.
            // Retirement is an input; AccountProgressPolicy owns its Off/On clock.
            var normal = 0; var forecasts = 0; var paused = 0;
            var retained = 1; var activity_history = 0;
            action Case when (stage == 0 && 22 > case) { case = case + 1; }
            action Elapse when (stage == 1 && elapsed == 0 && case <= 21) { elapsed = 1; }
            action ReplaceActivityRows when (stage == 1 && case == 20 && activity_rows_changed == 0) {
                activity_rows_changed = 1;
            }
            action RetainNormal when (case == 22 && stage == 0 && context == 0 && normal == 0) {
                normal = 1;
            }
            action RetireNormal when (case == 22 && stage == 0 && context == 0 && normal == 1) {
                normal = 2;
            }
            action CaptureContext when (case == 22 && stage == 0 && context == 0) {
                context = 1;
            }
            action ReplaceContext when (case == 22 && stage <= 1 && context == 1 && normal == 0) {
                context = if stage == 0 {2} else {3};
            }
            action WorkStarts when (case == 22 && stage <= 1 && activity == 0) { activity = 1; }
            action ActivityBusy when (case == 22 && stage <= 1 && activity == 0) { activity = 2; }
            action WorkSettles when (
                (case == 22 && stage <= 1 && activity == 1)
                || (case == 20 && stage == 1 && activity_rows_changed == 1 && activity == 0)
            ) { activity = 3; }
            action RejectDiagnostic when (stage <= 1 && activity == 3 && diagnostic_ok == 1) {
                diagnostic_ok = 0;
            }
            action PublishContext when (case == 22 && stage == 0) {
                stage = 1;
                context_record = if context == 1 || (Buggy == 21 && context == 2) {1} else {0};
            }
            action Observe when (stage == 0 && case <= 21) {
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
                work_visible = if Buggy == 39 && case == 22 && normal == 1 {0}
                    else if Buggy == 40 && case == 20 && activity_rows_changed == 1 && elapsed == 0 {0}
                    else if Buggy == 41 && case == 22 && context == 3 && activity == 1 {1}
                    else if case == 20 && activity_rows_changed == 1 && elapsed == 0 && activity == 0 {1}
                    else if case == 22 && normal == 1 && context == 1 && context_record == 1 && activity == 1 {1}
                    else {0};
                visible = if case == 22 {0}
                    else if (Buggy == 3 && case == 8) || (Buggy == 4 && case == 9)
                    || (Buggy == 5 && case == 10) || (Buggy == 6 && case == 11)
                    || (Buggy == 7 && case == 2) {1}
                    else if (Buggy == 1 && case == 0) || (Buggy == 8 && case == 13) {0}
                    else if case <= 1 || (4 <= case && case <= 6) || case == 13
                        || case == 14 || 17 <= case {1} else {0};
                view = if case == 22 {
                    if normal > 0 && activity == 2 {0}
                    else if normal == 1 {if Buggy == 35 && activity == 1 {7} else {1}}
                    else if normal == 2 {
                        if activity == 1 && context == 1 && context_record == 1 {
                            if Buggy == 30 {9} else {7}
                        } else {9}
                    }
                    else if activity == 1 && context_record == 1
                        && (context == 1 || Buggy == 23) {if Buggy == 22 {0} else {7}}
                    else if Buggy == 27 && context == 1 && activity == 0 {8} else {0}
                }
                    else if case == 20 && activity_rows_changed == 1 {
                        if elapsed == 0 {if Buggy == 37 {7} else {1}}
                        else if activity == 3 {8}
                        else {if Buggy == 36 {8} else {7}}
                    }
                    else if (Buggy == 3 && case == 8) || (Buggy == 4 && case == 9)
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
                counts_unknown = if case == 22 && normal > 0 {if activity == 2 {1} else {0}}
                    else if (Buggy == 9 && case == 0)
                    || (Buggy == 24 && case == 22) {0} else {1};
                deadline = if case == 22 {
                    if normal == 1 && (2 > activity || activity > 2) {1}
                    else if (normal == 0 && Buggy == 25) || (normal == 2 && Buggy == 31) {1} else {0}
                }
                    else if Buggy == 11 && case == 0 {0}
                    else if case == 20 {
                        if activity_rows_changed == 1 && elapsed == 1 {if Buggy == 38 {1} else {0}}
                        else {1}
                    }
                    else if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6) || case == 13
                        || case == 14 || 17 <= case) {1} else {0};
                generic_safe = if (Buggy == 14 && case == 14)
                    || (Buggy == 26 && case == 22) {0} else {1};
                retry = if activity == 3 && Buggy == 45 {1}
                    else if case == 22 {
                    if normal > 0 && activity == 2 {1}
                    else if normal == 1 {
                        if context == 1 && context_record == 1 && activity == 1 {1} else {0}
                    }
                    else if context == 1 && context_record == 1 && (activity == 1 || activity == 2) {
                        if (activity == 2 && Buggy == 28) || (activity == 1 && Buggy == 29) {0} else {1}
                    } else {0}
                }
                    else if case == 20 && activity == 3 {0}
                    else if Buggy == 16 && case == 21 {0}
                    else if case == 6 || 17 <= case {1} else {0};
                current = if case == 22 {0}
                    else if Buggy == 17 && case == 0 && elapsed == 1 {1}
                    else if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6)
                        || case == 13 || case == 14 || 17 <= case) {1} else {0};
                historical = if case == 22 {0}
                    else if Buggy == 19 && case == 0 && elapsed == 1 {0}
                    else if elapsed == 1 && (case <= 1 || (4 <= case && case <= 6)
                        || case == 13 || case == 14 || 17 <= case) {1} else {0};
                wake = if activity == 3 && Buggy == 45 {1}
                    else if case == 22 {
                    if normal > 0 && activity == 2 {1}
                    else if normal == 1 {
                        if context == 1 && context_record == 1 && activity == 1 {1} else {0}
                    }
                    else if context == 1 && context_record == 1 && (activity == 1 || activity == 2) {
                        if (activity == 2 && Buggy == 28) || (activity == 1 && Buggy == 29) {0} else {1}
                    } else {0}
                }
                    else if case == 20 && activity == 3 {if elapsed == 0 {1} else {0}}
                    else if Buggy == 18 && case == 0 && elapsed == 0 {0}
                    else if case == 6 || 17 <= case || (elapsed == 0 && (case <= 1
                        || case == 4 || case == 5 || case == 13 || case == 14)) {1} else {0};
                forecasts = if case == 22 && normal > 0 {
                    if normal == 1 && (2 > activity || activity > 2) {1}
                    else if normal == 2 && Buggy == 31 {1} else {0}
                } else {0};
                paused = if case == 22 && normal == 2 && (2 > activity || activity > 2) {
                    if Buggy == 32 {0} else {1}
                } else {0};
                retained = if case == 22 && normal > 0 && Buggy == 33 {0} else {1};
                activity_history = if case == 22 && normal == 2 && Buggy == 34 {1}
                    else if activity == 3 {
                        if Buggy == 42 {0} else if Buggy == 43 {1}
                        else if (diagnostic_ok == 1 || Buggy == 44)
                            && ((case == 22 && normal > 0 && context == 1 && context_record == 1)
                                || (case == 20 && activity_rows_changed == 1)) {1} else {0}
                    } else {0};
            }
            invariant ActualGateResult: stage == 0 || hold ==
                if case == 2 || case == 3 || case == 12 || case == 13 || case == 22 {0} else {1};
            invariant PublicationFenced: stage == 0 || published ==
                if case == 2 || case == 3 || case == 7 || case == 12 || case == 22 {0} else {1};
            invariant HoldVisibility: stage <= 1 || visible ==
                if case == 22 {0} else if case <= 1 || (4 <= case && case <= 6) || case == 13
                    || case == 14 || 17 <= case {1} else {0};
            invariant RotationOffKeepsGate: stage <= 1 || case > 0
                || (visible == 1 && view == if elapsed == 0 {1} else {8});
            invariant NormalPhaseRestored: case == 22 || stage <= 1 || view ==
                if case == 20 && activity_rows_changed == 1 && elapsed == 0 {1}
                else if case == 20 && activity == 3 {8}
                else if case == 6 {5}
                else if 17 <= case && case <= 20 {case - 13}
                else if elapsed == 1 && (case <= 1 || case == 4 || case == 5
                    || case == 13 || case == 14 || case == 21) {8}
                else if case <= 1 || case == 4 || case == 5 || case == 13
                    || case == 14 || case == 21 {1}
                else if case == 2 || case == 12 {2} else if case == 3 {3} else {0};
            invariant CurrentWorkVisible: case == 22 || stage <= 1 || case <= 5 || (7 <= case && case <= 16)
                || case == 21 || view == if case == 20 && activity_rows_changed == 1 && elapsed == 0 {1}
                    else if case == 20 && activity == 3 {8}
                    else if case == 6 {5} else {case - 13};
            invariant GuardCountsUnjudged: visible == 0 || counts_unknown == 1;
            invariant CurrentScopedWorkVisible: stage <= 1 || work_visible ==
                if case == 20 && activity_rows_changed == 1 && elapsed == 0 && activity == 0 {1}
                else if case == 22 && normal == 1 && context == 1 && context_record == 1 && activity == 1 {1}
                else {0};
            invariant ActualRecheckShown: stage <= 1 || deadline ==
                if case == 22 {if normal == 1 && (2 > activity || activity > 2) {1} else {0}}
                else if case == 20 {if activity_rows_changed == 1 && elapsed == 1 {0} else {1}}
                else if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6) || case == 13
                    || case == 14 || 17 <= case) {1} else {0};
            invariant AuthorityPreserved: authority == 1;
            invariant UnknownBirthIsGeneric: generic_safe == 1;
            invariant UnobservedActivityRechecks: stage <= 1 || retry ==
                if case == 22 {
                    if normal > 0 && activity == 2 {1} else if normal == 1 {
                        if context == 1 && context_record == 1 && activity == 1 {1} else {0}
                    }
                    else if context == 1 && context_record == 1 && (activity == 1 || activity == 2) {1} else {0}
                }
                else if case == 20 && activity == 3 {0}
                else if case == 6 || 17 <= case {1} else {0};
            invariant CurrentHoldRequiresUnexpiredLease: stage <= 1 || current ==
                if case == 22 {0} else if elapsed == 0 && (case <= 1 || (4 <= case && case <= 6)
                    || case == 13 || case == 14 || 17 <= case) {1} else {0};
            invariant ExpiredFindingIsHistorical: stage <= 1 || historical ==
                if case == 22 {0} else if elapsed == 1 && (case <= 1 || (4 <= case && case <= 6)
                    || case == 13 || case == 14 || 17 <= case) {1} else {0};
            invariant LeaseExpirySchedulesObservation: stage <= 1 || wake ==
                if case == 22 {
                    if normal > 0 && activity == 2 {1} else if normal == 1 {
                        if context == 1 && context_record == 1 && activity == 1 {1} else {0}
                    }
                    else if context == 1 && context_record == 1 && (activity == 1 || activity == 2) {1} else {0}
                }
                else if case == 20 && activity == 3 {if elapsed == 0 {1} else {0}}
                else if case == 6 || 17 <= case || (elapsed == 0 && (case <= 1
                    || case == 4 || case == 5 || case == 13 || case == 14)) {1} else {0};
            bound StageRange: stage <= 2;
            invariant ContextPublicationFenced: case <= 21 || stage == 0 || context_record ==
                if context == 1 || context == 3 {1} else {0};
            invariant IndependentWorkVisible: case <= 21 || stage <= 1 || normal == 1 || activity > 1
                || 1 > activity || 1 > context_record || 1 > context || context > 1 || view == 7;
            invariant ContextScopeRespected: case <= 21 || stage <= 1 || normal > 0 || context == 1 || view == 0;
            invariant ContextAloneUnobserved: case <= 21 || stage <= 1 || normal > 0
                || activity == 1 || activity == 2 || view == 0;
            invariant ContextKnowledgeUnjudged: case <= 21 || stage <= 1 || normal > 0
                || (counts_unknown == 1 && generic_safe == 1 && deadline == 0);
            invariant RetiredNormalWithdrawsForecasts: case <= 21 || stage <= 1 || 2 > normal
                || (forecasts == 0 && deadline == 0 && (activity == 2 || paused == 1));
            invariant RetainedNormalPreserved: retained == 1;
            // The former no-history rule admits only a settled, exact current-context
            // diagnostic alongside a retained decision; it grants no current work.
            invariant SettledHistoryUsesCurrentContext: activity_history ==
                if stage == 2 && activity == 3 && diagnostic_ok == 1
                    && ((case == 22 && normal > 0 && context == 1 && context_record == 1)
                        || (case == 20 && activity_rows_changed == 1)) {1} else {0};
            invariant ContextOnlyNoHistory: case <= 21 || normal > 0 || activity_history == 0;
            invariant SettledHistoryDoesNotPoll: 3 > activity || stage <= 1
                || (retry == 0 && work_visible == 0
                    && wake == if case == 20 && elapsed == 0 {1} else {0});
            invariant CurrentNormalKeepsItsContext: case <= 21 || stage <= 1 || 1 > normal || normal > 1
                || view == if activity == 2 {0} else {1};
            bound CaseRange: case <= 22;
            bound ElapsedRange: elapsed <= 1;
            bound ActivityRowsRange: activity_rows_changed <= 1;
            bound WorkRange: work_visible <= 1;
            bound ContextRange: context <= 3 && context_record <= 1 && activity <= 3 && diagnostic_ok <= 1;
            bound NormalRange: normal <= 2 && forecasts <= 1 && paused <= 1
                && retained <= 1 && activity_history <= 1;
        }
    }
}
