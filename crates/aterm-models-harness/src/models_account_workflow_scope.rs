// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Persisted task and attempt observations describe their own workflow, independently of
/// credential, lifetime and preparation observations. Native transitions use admitted Store edits.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_workflow_scope_model() -> Model {
    aterm_spec::ty_model! {
        AccountWorkflowScope {
            const Buggy = 0;
            var provider = 1; var edit = 1; var phase = 0; var read = 0;
            var task = 0; var attempt = 0; var blocked = 0; var token_changed = 0;
            var history = 1; var pure = 1; var accepted = 0; var preserved = 1;
            action AnotherProvider when (phase == 0 && provider == 1) { provider = 2; }
            action AnotherEdit when (phase == 0 && 13 > edit) {
                edit = if provider == 1 && edit == 7 {9} else {edit + 1};
            }
            action Seed when (phase == 0) { phase = 1; read = 0; }
            action Legacy when (phase == 0) { phase = 9; read = 0; }
            action Transform when (phase == 1 && read == 1) {
                phase = 2; read = 0;
                history = if Buggy == 12 {0} else {1};
            }
            action FreshTask when (phase == 2 && read == 1) { phase = 3; read = 0; }
            action FreshAttempt when (phase == 2 && read == 1) { phase = 4; read = 0; }
            action Activity when (phase == 2 && read == 1) { phase = 5; read = 0; }
            action Back when (phase == 2 && read == 1 && edit <= 8) { phase = 6; read = 0; }
            action StaleTask when (phase == 2 && read == 1) {
                phase = 7; read = 0;
                accepted = if Buggy == 13 {1} else {0};
                preserved = if Buggy == 15 {0} else {1};
            }
            action StaleAttempt when (phase == 2 && read == 1) {
                phase = 8; read = 0;
                accepted = if Buggy == 14 {1} else {0};
                preserved = if Buggy == 15 {0} else {1};
            }
            action Read when (phase > 0 && (phase <= 6 || phase == 9) && read == 0) {
                read = 1;
                task = if phase == 9 {if Buggy == 21 {1} else {0}}
                    else if (Buggy == 1 && edit <= 8 && phase == 2)
                    || (Buggy == 6 && edit <= 8 && phase == 4)
                    || (Buggy == 9 && phase == 6)
                    || (Buggy == 18 && edit <= 8 && phase == 5) {1}
                    else if (Buggy == 3 && edit > 8 && phase > 1)
                        || (Buggy == 7 && edit <= 8 && phase == 3)
                        || (Buggy == 16 && edit > 8 && phase == 5)
                        || (Buggy == 17 && edit == 12 && phase == 2) {0}
                    else if phase == 1 || edit > 8 || phase == 3 {1} else {0};
                attempt = if phase == 9 {0}
                    else if (Buggy == 2 && edit <= 8 && phase == 2)
                    || (Buggy == 5 && edit <= 8 && phase == 3)
                    || (Buggy == 9 && phase == 6) {1}
                    else if (Buggy == 4 && edit > 8 && phase > 1)
                        || (Buggy == 8 && edit <= 8 && phase == 4) {0}
                    else if phase == 1 || edit > 8 || phase == 4 {1} else {0};
                blocked = if phase == 9 {0}
                    else if Buggy == 19 && edit <= 8 && phase == 2 {1}
                    else if phase == 1 || edit > 8 || phase == 4 {1} else {0};
                token_changed = if phase == 9 {0}
                    else if Buggy == 10 && edit <= 8 && phase > 1 {0}
                    else if Buggy == 20 && edit > 8 && phase > 1 {1}
                    else if phase > 1 && edit <= 8 {1} else {0};
                pure = if Buggy == 11 {0} else {1};
            }
            invariant HistoricalGuidanceHidden: read == 0 || edit > 8 || phase <= 1
                || phase == 3 || phase == 4 || (task == 0 && attempt == 0 && blocked == 0);
            invariant CurrentGuidanceAvailable: read == 0 || phase == 9 || (phase > 1 && edit <= 8)
                || (task == 1 && attempt == 1 && blocked == 1);
            invariant LegacyIsUnknown: read == 0 || phase <= 8
                || (task == 0 && attempt == 0 && blocked == 0 && token_changed == 0);
            invariant FreshTaskCarriesIndependentIntent: read == 0 || 3 > phase || phase > 3
                || (task == 1 && (edit > 8 || (attempt == 0 && blocked == 0)));
            invariant FreshAttemptCarriesIndependentIntent: read == 0 || 4 > phase || phase > 4
                || (attempt == 1 && blocked == 1 && (edit > 8 || task == 0));
            invariant ABAStaysHistorical: read == 0 || phase <= 5
                || (task == 0 && attempt == 0 && blocked == 0);
            invariant ActivityPreservesApplicability: read == 0 || phase <= 4 || phase > 5
                || (task == (if edit > 8 {1} else {0})
                    && attempt == (if edit > 8 {1} else {0}));
            invariant ReplacementHasNewCustody: read == 0 || phase == 9 || edit > 8 || phase <= 1 || token_changed == 1;
            invariant NonPolicyEditsKeepCustody: read == 0 || edit <= 8 || token_changed == 0;
            invariant BlockedFollowsApplicableAttempt: read == 0 || blocked == attempt;
            invariant RawHistoryPreserved: history == 1;
            invariant ReadsDoNotWrite: pure == 1;
            invariant StaleFreshPublicationRefused: phase <= 6 || accepted == 0;
            invariant RefusalPreservesImages: preserved == 1;
        }
    }
}

/// Native guards use workflow custody and wall-clock person-task expiry without releasing
/// another hold, a newer claim or active work.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_workflow_runtime_model() -> Model {
    aterm_spec::ty_model! {
        AccountWorkflowRuntime {
            const Buggy = 0;
            var phase = 0; var case = 1; var held = 0; var preserved = 1;
            action AnotherCase when (phase == 0 && case <= 13) { case = case + 1; }
            action Read when (phase == 0) {
                phase = 1;
                held = if case > 8 {
                    if case == 9 { if Buggy == 10 {1} else {0} }
                    else if (case == 10 && Buggy == 11) || (case == 11 && Buggy == 12)
                        || (case == 12 && Buggy == 13) || (case == 13 && Buggy == 14)
                        || (case == 14 && Buggy == 15) {0} else {1}
                } else if case == 1 || case == 4 || case == 5 {
                    if (case == 1 && Buggy == 1) || (case == 4 && Buggy == 4)
                        || (case == 5 && Buggy == 5) {0} else {1}
                } else if (case == 2 && Buggy == 2) || (case == 3 && Buggy == 3)
                    || (case == 6 && Buggy == 6) || (case == 7 && Buggy == 7)
                    || (case == 8 && Buggy == 8) {1} else {0};
                preserved = if Buggy == 9 {0} else {1};
            }
            invariant CurrentHoldsRemain: phase == 0 || held == 1
                || (case > 1 && 4 > case) || case > 5;
            invariant RetiredHoldsGone: phase == 0 || case > 8
                || case == 1 || case == 4 || case == 5 || case == 6 || held == 0;
            invariant CompletionRequiresWorkflowCustody: phase == 0 || 5 > case || case > 6
                || held == if case == 5 {1} else {0};
            invariant OldFinishCannotRepark: phase == 0 || 8 > case || case > 8 || held == 0;
            invariant ReadsPreserveImages: preserved == 1;
            invariant CurrentPersonExpiryReleases: phase == 0 || 9 > case || case > 9 || held == 0;
            invariant PersonExpiryPreservesOtherHolds: phase == 0 || case <= 9 || held == 1;
        }
    }
}
