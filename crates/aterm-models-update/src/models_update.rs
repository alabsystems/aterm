// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `native_update_*` models: update admission, handoff, apply ladder and worker.

use aterm_spec::derive::*;

/// Control-socket ownership handoff model across update commit and abort.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_control_socket_handoff_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateControlSocketHandoff {
            const Buggy = 0;
            var committed = 0;
            var parent_live = 1;
            var bound = 0;
            var rejected = 0;
            var foreign = 0;

            action Commit when (committed == 0 && rejected == 0) {
                committed = 1;
            }
            action Exit when (parent_live == 1) {
                parent_live = 0;
            }
            action Reject when (committed == 0) {
                rejected = 1;
            }
            action ForeignBind when (parent_live == 0 && bound == 0 && foreign == 0) {
                foreign = 1;
            }
            action Bind when (
                bound == 0 &&
                ((committed == 1 && parent_live == 0 && rejected == 0 && foreign == 0) || Buggy == 1)
            ) {
                bound = 1;
            }
            invariant CommitBeforeSocketMutation:
                bound == 0 || (committed == 1 && parent_live == 0 && rejected == 0 && foreign == 0);
        }
    }
}

/// Successor dial and secret-claim model for the out-of-band handoff rendezvous.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_rendezvous_claim_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateRendezvousClaim {
            const Buggy = 0;
            var same_uid = 1;
            var is_parent = 1;
            var parent_live = 1;
            var presented = 0;

            action ForeignUidListener when (presented == 0 && same_uid == 1) {
                same_uid = 0;
            }
            action SameUidImpostor when (presented == 0 && is_parent == 1) {
                is_parent = 0;
            }
            action ParentGone when (presented == 0 && parent_live == 1) {
                parent_live = 0;
            }
            action PresentClaim when (
                presented == 0 && same_uid == 1 && is_parent == 1 && parent_live == 1
            ) {
                presented = 1;
            }
            action PresentClaimUidOnly when (Buggy == 1 && presented == 0 && same_uid == 1) {
                presented = 1;
            }
            invariant ClaimOnlyToTheAttestedParent:
                presented == 0 || (same_uid == 1 && is_parent == 1 && parent_live == 1);
        }
    }
}

/// Outgoing peer-identity and secret admission model for the handoff rendezvous.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_rendezvous_admission_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateRendezvousAdmission {
            const Buggy = 0;
            var secret = 1;
            var code = 1;
            var answered = 1;
            var disowned = 0;
            var phase = 0;
            var filed = 0;

            action WrongSecret when (phase == 0 && secret == 1) {
                secret = 0;
            }
            action CopiedSecretOtherCode when (phase == 0 && code == 1) {
                code = 0;
            }
            action TrustdSilent when (phase == 0 && answered == 1) {
                answered = 0;
            }
            action TrustdAnswers when (phase == 0 && answered == 0) {
                answered = 1;
            }
            action LaunchServicesDisowns when (phase <= 1 && disowned == 0) {
                disowned = 1;
            }
            action Grant when (
                phase == 0 && secret == 1 && code == 1 && answered == 1 && disowned == 0
            ) {
                phase = 1;
            }
            action Refuse when (
                phase <= 1 && (secret == 0 || (answered == 1 && code == 0) || disowned == 1)
            ) {
                phase = 3;
            }
            action DeadlineWhileHeld when (phase == 0 && answered == 0) {
                phase = 4;
            }
            action Commit when (phase == 1 && disowned == 0) {
                phase = 2;
            }
            action FileByCause when (phase > 2 && filed == 0) {
                filed = 1;
            }
            action GrantOnSecretAlone when (Buggy == 1 && phase == 0 && secret == 1) {
                phase = 1;
            }
            action CommitOverDisowning when (Buggy == 1 && phase == 1) {
                phase = 2;
            }
            action RefuseUnanswered when (Buggy == 1 && phase == 0 && answered == 0) {
                phase = 3;
            }
            action FileAsUnexplainedDeath when (Buggy == 1 && phase == 3 && filed == 0) {
                filed = 2;
            }
            invariant GrantOnlyToTheSuccessorsCode:
                phase == 0 || phase > 2 || (secret == 1 && code == 1 && answered == 1);
            invariant NoCommitToADisownedDialer: phase <= 1 || phase > 2 || disowned == 0;
            invariant AnUnansweredCheckTurnsNoOneAway:
                phase <= 2 || phase == 4 || secret == 0 || disowned == 1
                    || (answered == 1 && code == 0);
            invariant ARefusalIsFiledByItsCause: filed <= 1;
        }
    }
}

/// Admission and handoff policy model for applying a verified native update.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_admission_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateAdmission {
            const Buggy = 0;
            const MaxAttempts = 2;
            var phase = 0;
            var verified = 1;
            var foreground_job = 0;
            var unsafe_native_state = 0;
            var seamless_available = 1;
            var decision = 0;
            var attempt_count = 0;
            var reexec_count = 0;
            var adopted_foreground = 0;
            var retry_eligible = 0;
            var image_kept = 0;
            action KeepImage when (
                phase == 0 && decision == 0 && image_kept == 0
            ) {
                image_kept = 1;
            }
            action ObserveForegroundJob when (
                phase == 0 && decision == 0 && foreground_job == 0
            ) {
                foreground_job = 1;
            }
            action ObserveUnsafeNativeState when (
                phase == 0 && decision == 0 && unsafe_native_state == 0
            ) {
                unsafe_native_state = 1;
            }
            action InvalidateArtifact when (
                phase == 0 && decision == 0 && verified == 1
            ) {
                verified = 0;
            }
            action LoseSeamlessLane when (
                phase == 0 && decision == 0 && seamless_available == 1
            ) {
                seamless_available = 0;
            }
            action ClassifySeamless when (
                phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 1
            ) {
                phase = 1;
                decision = 1;
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
                retry_eligible = 0;
            }
            action BlockForegroundDespiteSeamless when (
                Buggy == 1 && phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 1 &&
                foreground_job == 1
            ) {
                phase = 3;
                decision = 3;
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
                retry_eligible = 1;
            }
            action ClassifySeamlessOverUncertifiedState when (
                Buggy == 1 && phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 1 && seamless_available == 1
            ) {
                phase = 1;
                decision = 1;
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
                retry_eligible = 0;
            }
            action ClassifyCold when (
                phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 0 &&
                foreground_job == 0 && image_kept == 0
            ) {
                phase = 1;
                decision = 2;
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
            }
            action ClassifyColdOverKeptImage when (
                Buggy == 1 && phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 0 &&
                foreground_job == 0 && image_kept == 1
            ) {
                phase = 1;
                decision = 2;
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
            }
            action BlockKeptImageWithoutTerminal when (
                phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 0 &&
                foreground_job == 0 && image_kept == 1
            ) {
                phase = 3;
                decision = 3;
                retry_eligible = 1;
            }
            action ReexecColdOverLiveSessions when (
                Buggy == 1 && phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 0 &&
                foreground_job == 1
            ) {
                phase = 2;
                decision = 2;
                reexec_count = 1;
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
            }
            action BlockForegroundWithoutSeamless when (
                phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 0 &&
                foreground_job == 1
            ) {
                phase = 3;
                decision = 3;
                seamless_available = 0;
                retry_eligible = 1;
            }
            action BlockLatchesWithoutRetry when (
                Buggy == 1 && phase == 0 && decision == 0 && verified == 1 &&
                unsafe_native_state == 0 && seamless_available == 0 &&
                foreground_job == 1
            ) {
                phase = 3;
                decision = 3;
                seamless_available = 0;
                retry_eligible = 0;
            }
            action BlockUnverifiedArtifact when (
                phase == 0 && decision == 0 && verified == 0
            ) {
                phase = 3;
                decision = 3;
            }
            action BlockUnsafeNativeState when (
                phase == 0 && decision == 0 && unsafe_native_state == 1
            ) {
                phase = 3;
                decision = 3;
                retry_eligible = 1;
            }
            action CompleteSeamlessHandoff when (
                phase == 1 && decision == 1 && reexec_count == 0
            ) {
                phase = 2;
                reexec_count = 1;
                adopted_foreground = foreground_job;
            }
            action HandoffWithoutAdoptingForeground when (
                Buggy == 1 && phase == 1 && decision == 1 && reexec_count == 0 &&
                foreground_job == 1
            ) {
                phase = 2;
                reexec_count = 1;
                adopted_foreground = 0;
            }
            action CompleteColdFallback when (
                phase == 1 && decision == 2 && foreground_job == 0 &&
                reexec_count == 0
            ) {
                phase = 2;
                reexec_count = 1;
            }
            action AbortSeamlessAttempt when (
                phase == 1 && decision == 1 && reexec_count == 0
            ) {
                phase = 3;
                decision = 3;
                seamless_available = 0;
                retry_eligible = 1;
            }
            action AbortColdAttempt when (
                phase == 1 && decision == 2 && reexec_count == 0
            ) {
                phase = 3;
                decision = 3;
                retry_eligible = 1;
            }
            action RetryAfterHandoffFailure when (
                phase == 3 && decision == 3 && verified == 1 &&
                unsafe_native_state == 0 && retry_eligible == 1
            ) {
                phase = 0;
                decision = 0;
                seamless_available = 1;
                retry_eligible = 0;
            }
            action RepairNativeStateAndRetry when (
                phase == 3 && decision == 3 && verified == 1 &&
                unsafe_native_state == 1 && retry_eligible == 1
            ) {
                phase = 0;
                decision = 0;
                unsafe_native_state = 0;
                retry_eligible = 0;
            }
            invariant ForegroundJobsDoNotBlockSeamless:
                if phase == 3 && verified == 1 && foreground_job == 1 &&
                    unsafe_native_state == 0 && seamless_available == 1 {
                    decision <= 2
                } else {
                    attempt_count <= MaxAttempts
                };
            invariant ReplacementPreservesForeground:
                if phase == 2 && foreground_job == 1 {
                    decision == 1 && adopted_foreground == 1
                } else {
                    adopted_foreground <= foreground_job
                };
            invariant ColdFallbackNeverDropsForeground:
                if phase == 2 && decision == 2 {
                    foreground_job == 0
                } else {
                    foreground_job <= 1
                };
            invariant UnsafeStateNeverReexecutes:
                if unsafe_native_state == 1 {
                    reexec_count == 0
                } else {
                    if verified == 0 {
                        reexec_count == 0
                    } else {
                        reexec_count <= 1
                    }
                };
            invariant BlockedIsRetryableWithoutReexec:
                if phase == 3 && verified == 1 {
                    reexec_count == 0 && retry_eligible == 1
                } else {
                    decision <= 3
                };
            invariant KeptImageNeverReexecsCold:
                if image_kept == 1 {
                    decision <= 1 || decision == 3
                } else {
                    decision <= 3
                };
            bound AttemptsBounded: attempt_count <= MaxAttempts;
        }
    }
}

/// Ordering contract model for persistent automatic-update intent.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_auto_intent_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateAutoIntent {
            const Buggy = 0;
            const MaxAttempts = 2;
            const MaxDeferrals = 3;
            var phase = 0;
            var active_check = 0;
            var durable_stage = 0;
            var staged = 0;
            var intent = 0;
            var attempts = 0;
            var last_unsuccessful = 0;
            var accepted = 0;
            var manual_only = 0;
            var target = 0;
            var stale_wake_seen = 0;
            var quiet = 0;
            var parked = 0;
            var deferrals = 0;
            var grace_expired = 0;
            action StartManualCheck when (phase == 0 && active_check == 0) {
                phase = 1;
                active_check = 1;
            }
            action StageWakeDuringCheck when (
                phase == 1 && active_check == 1 && durable_stage == 0
            ) {
                durable_stage = 1;
                intent = 1;
                target = 1;
            }
            action StageWakeDroppingIntent when (
                Buggy == 1 && phase == 1 && active_check == 1 && durable_stage == 0
            ) {
                durable_stage = 1;
                intent = 0;
                target = 1;
            }
            action StageWakeIdle when (phase == 0 && active_check == 0) {
                phase = 2;
                durable_stage = 1;
                staged = 1;
                intent = 1;
                target = 1;
            }
            action ArmNewerIntent when (phase == 0 && intent == 0 && target == 0) {
                phase = 5;
                intent = 1;
                target = 2;
            }
            action ObserveStaleWake when (
                phase == 5 && intent == 1 && target == 2 && stale_wake_seen == 0
            ) {
                intent = 1;
                stale_wake_seen = 1;
            }
            action StaleWakeClearsNewerIntent when (
                Buggy == 1 && phase == 5 && intent == 1 && target == 2 &&
                stale_wake_seen == 0
            ) {
                intent = 0;
                stale_wake_seen = 1;
            }
            action ManualCheckCompletesAndImportsStage when (
                phase == 1 && active_check == 1 && durable_stage == 1
            ) {
                phase = 2;
                active_check = 0;
                staged = 1;
            }
            action ManualCheckCompletesNoStage when (
                phase == 1 && active_check == 1 && durable_stage == 0
            ) {
                phase = 0;
                active_check = 0;
            }
            action QuietElapsed when (
                phase == 2 && staged == 1 && intent == 1 && quiet == 0
            ) {
                quiet = 1;
            }
            action Activity when (
                phase == 2 && staged == 1 && intent == 1 &&
                deferrals <= MaxDeferrals - 1
            ) {
                quiet = 0;
                deferrals = deferrals + 1;
            }
            action DuplicateStageWake when (
                phase == 2 && staged == 1 && intent == 1
            ) {
                intent = 1;
            }
            action GraceWindowCloses when (
                phase == 2 && staged == 1 && intent == 1 &&
                quiet == 0 && grace_expired == 0
            ) {
                grace_expired = 1;
            }
            action Attempt when (
                phase == 2 && staged == 1 && intent == 1 &&
                (quiet == 1 || grace_expired == 1)
            ) {
                phase = 3;
                attempts = if attempts <= MaxAttempts - 1 {
                    attempts + 1
                } else {
                    attempts
                };
                last_unsuccessful = 0;
            }
            action HoldActivity when (
                phase == 3 && parked == 0 && deferrals <= MaxDeferrals - 1
            ) {
                quiet = 0;
                deferrals = deferrals + 1;
            }
            action HoldQuietElapsed when (phase == 3 && parked == 0 && quiet == 0) {
                quiet = 1;
            }
            action ParkReaders when (
                phase == 3 && parked == 0 && (quiet == 1 || grace_expired == 1)
            ) {
                parked = 1;
            }
            action ParkWithoutQuietOrGrace when (
                Buggy == 1 && phase == 3 && parked == 0 && quiet == 0 &&
                grace_expired == 0
            ) {
                parked = 1;
            }
            action ReparkAfterMissedBudget when (
                phase == 3 && parked == 1 && accepted == 0
            ) {
                parked = 0;
            }
            action HoldCapStandsDown when (
                phase == 3 && parked == 0 && quiet == 0 && grace_expired == 0
            ) {
                phase = 2;
                last_unsuccessful = 1;
            }
            action AttemptDidNotReplace when (
                phase == 3 && staged == 1 && intent == 1 && accepted == 0
            ) {
                phase = 2;
                last_unsuccessful = 1;
                parked = 0;
            }
            action AttemptDidNotReplaceSpendingIntent when (
                Buggy == 1 && phase == 3 && staged == 1 && intent == 1 &&
                accepted == 0
            ) {
                phase = 2;
                intent = 0;
                last_unsuccessful = 1;
                parked = 0;
            }
            action AttemptPhysicalFailure when (
                phase == 3 && staged == 1 && intent == 1 && accepted == 0
            ) {
                phase = 6;
                intent = 0;
                last_unsuccessful = 2;
                manual_only = 1;
                parked = 0;
            }
            action AttemptPhysicalFailureKeepingIntent when (
                Buggy == 1 && phase == 3 && staged == 1 && intent == 1 &&
                accepted == 0
            ) {
                phase = 6;
                last_unsuccessful = 2;
                manual_only = 1;
                parked = 0;
            }
            action AttemptAccepted when (
                phase == 3 && staged == 1 && intent == 1 && accepted == 0 &&
                parked == 1
            ) {
                phase = 4;
                intent = 0;
                accepted = 1;
            }
            action AcceptWithoutPark when (
                Buggy == 1 && phase == 3 && staged == 1 && intent == 1 &&
                accepted == 0 && parked == 0
            ) {
                phase = 4;
                intent = 0;
                accepted = 1;
            }
            invariant StageDuringCheckRetainsIntent:
                if phase == 1 && durable_stage == 1 {
                    intent == 1
                } else {
                    active_check <= 1
                };
            invariant ImportedStageIsEligible:
                if phase == 2 {
                    staged == 1 && intent == 1 && active_check == 0
                } else {
                    staged <= 1
                };
            invariant UnsuccessfulAttemptRetainsIntent:
                if last_unsuccessful == 1 {
                    phase == 2 && staged == 1 && intent == 1
                } else {
                    intent <= 1
                };
            invariant PhysicalFailureIsManualOnly:
                if manual_only == 1 {
                    phase == 6 && staged == 1 && intent == 0 && accepted == 0
                } else {
                    last_unsuccessful <= 1
                };
            invariant NewerIntentSurvivesStaleWake:
                if phase == 5 && target == 2 {
                    intent == 1
                } else {
                    stale_wake_seen <= 1
                };
            invariant AutomaticAttemptRequiresQuietOrClosedGraceWindow:
                if parked == 1 {
                    quiet == 1 || grace_expired == 1
                } else {
                    quiet <= 1
                };
            invariant AcceptedRequiresParkedReaders:
                if accepted == 1 {
                    parked == 1
                } else {
                    accepted == 0
                };
            bound DeferralsBounded: deferrals <= MaxDeferrals;
            bound AttemptsBounded: attempts <= MaxAttempts;
        }
    }
}

/// Staged native-update apply ladder model across launch, park, and cold fallback.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_apply_ladder_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateApplyLadder {
            const Buggy = 0;
            const Land = 3;
            const Hold = 2;
            var phase = 0;
            var quiet = 0;
            var keys = 1;
            var output = 0;
            var focused = 1;
            var manual_only = 0;
            var landed = 0;
            var latched = 0;
            var failures = 0;
            var reached = 0;
            var parked_phase = 0;
            var parked_quiet = 0;
            var parked_keys = 0;
            var parked_output = 0;
            var parked_focused = 0;
            var warmup = 0;
            var parked_warmup = 0;
            var refusing = 0;
            var degraded = 0;
            var refusals = 0;
            var due = 0;
            var swapped = 0;
            var converged = 0;
            var newer = 0;
            var looked = 0;
            var superseded = 0;
            var parked_newer = 0;
            var retry = 0;
            var passing = 0;
            action Advance when (landed == 0 && manual_only == 0 && phase <= Land - 1) {
                phase = phase + 1;
                reached = phase + 1;
            }
            action Busy when (landed == 0 && manual_only == 0) {
                quiet = 0;
                output = 0;
            }
            action Quiet when (landed == 0 && manual_only == 0) {
                quiet = 1;
                output = 1;
            }
            action KeysDown when (landed == 0 && manual_only == 0) {
                keys = 0;
                quiet = 0;
            }
            action KeysUp when (landed == 0 && manual_only == 0) {
                keys = 1;
            }
            action Blur when (landed == 0 && manual_only == 0) {
                focused = 0;
            }
            action Focus when (landed == 0 && manual_only == 0) {
                focused = 1;
            }
            action WarmupStarts when (landed == 0 && manual_only == 0 && warmup == 0) {
                warmup = 1;
            }
            action WarmupEnds when (landed == 0 && manual_only == 0 && warmup == 1) {
                warmup = 0;
            }
            action DeskRefuses when (landed == 0 && manual_only == 0) {
                refusing = 1;
            }
            action DeskChanges when (landed == 0 && manual_only == 0) {
                refusing = 0;
                degraded = 0;
            }
            action StandDown when (
                Buggy == 1 && landed == 0 && manual_only == 0 && quiet == 0 &&
                phase > Hold - 1
            ) {
                manual_only = 1;
            }
            action Park when (
                landed == 0 && manual_only == 0 && latched == 0 && warmup == 0 &&
                (refusing == 0 || degraded == 1) &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                landed = 1;
                parked_phase = phase;
                parked_quiet = quiet;
                parked_keys = keys;
                parked_output = output;
                parked_focused = focused;
                parked_warmup = warmup;
                parked_newer = superseded;
            }
            action ParkMissed when (
                landed == 0 && manual_only == 0 && latched == 0 && warmup == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                quiet = 0;
            }
            action CaptureRefused when (
                landed == 0 && manual_only == 0 && latched == 0 && warmup == 0 &&
                refusing == 1 && degraded == 0 && refusals == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                degraded = 1;
            }
            action CaptureRefusedAsActivity when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 0 &&
                warmup == 0 && refusing == 1 && degraded == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                quiet = 0;
                refusals = 1;
            }
            action PhysicalFailure when (
                landed == 0 && manual_only == 0 && latched == 0 && failures == 0 &&
                warmup == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                latched = 1;
                failures = 1;
            }
            action PassingFailure when (
                landed == 0 && manual_only == 0 && latched == 0 && failures == 0 &&
                warmup == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                latched = 1;
                failures = 1;
                passing = 1;
            }
            action PassingFailureConverges when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 1 &&
                passing == 1 && due == 0 && converged == 0
            ) {
                converged = 1;
            }
            action Due when (
                landed == 0 && manual_only == 0 && latched == 1 && due == 0 && converged == 0
            ) {
                due = 1;
            }
            action Lapse when (
                landed == 0 && manual_only == 0 && latched == 1 && due == 1
            ) {
                latched = 0;
            }
            action BundleSwap when (
                landed == 0 && manual_only == 0 && failures == 1 && swapped == 0
            ) {
                swapped = 1;
            }
            action BundleSwapClearsLatch when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 1 &&
                swapped == 0
            ) {
                swapped = 1;
                latched = 0;
            }
            action Converge when (
                landed == 0 && manual_only == 0 && latched == 1 && swapped == 1 &&
                due == 0 && converged == 0 && passing == 0
            ) {
                converged = 1;
            }
            action NewerRelease when (
                landed == 0 && manual_only == 0 && converged == 1 && newer == 0
            ) {
                newer = 1;
            }
            action Supersede when (
                landed == 0 && manual_only == 0 && converged == 1 && newer == 1 &&
                superseded == 0 && looked == 0 && retry == 0
            ) {
                latched = 0;
                superseded = 1;
                looked = 1;
                phase = 0;
                reached = 0;
            }
            action SupersedeRefused when (
                landed == 0 && manual_only == 0 && converged == 1 && newer == 1 &&
                superseded == 0 && looked == 0 && retry == 0
            ) {
                retry = 1;
            }
            action SupersedeRetryDue when (landed == 0 && manual_only == 0 && retry == 1) {
                retry = 0;
            }
            action NewerReleaseIgnored when (
                Buggy == 1 && landed == 0 && manual_only == 0 && converged == 1 &&
                newer == 1 && looked == 0 && superseded == 0 && retry == 0
            ) {
                looked = 1;
            }
            action NewerReleaseRetriesTheActivation when (
                Buggy == 1 && landed == 0 && manual_only == 0 && converged == 1 &&
                newer == 1 && looked == 0 && superseded == 0 && retry == 0
            ) {
                looked = 1;
                latched = 0;
            }
            action RefusedRetireSpendsTheRelease when (
                Buggy == 1 && landed == 0 && manual_only == 0 && converged == 1 &&
                newer == 1 && looked == 0 && superseded == 0 && retry == 0
            ) {
                looked = 1;
            }
            action ParkMissLatches when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 0 && warmup == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                manual_only = 1;
            }
            action LapseRestartsTheLadder when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 1 && due == 1
            ) {
                latched = 0;
                phase = 0;
            }
            action ParkWithoutTheRule when (Buggy == 1 && landed == 0 && manual_only == 0) {
                landed = 1;
                parked_phase = phase;
                parked_quiet = quiet;
                parked_keys = keys;
                parked_output = output;
                parked_focused = focused;
                parked_warmup = warmup;
                parked_newer = superseded;
            }
            invariant ActivityNeverLatchesManualOnly: manual_only == 0;
            invariant TheLadderNeverRestarts: reached <= phase;
            invariant RefusalNeverRetriesAsActivity: refusals == 0;
            invariant NoEarlyRelease:
                if failures == 1 && latched == 0 {
                    due == 1 || superseded == 1
                } else {
                    due <= 1
                };
            invariant APassingFailureNeverConverges:
                if passing == 1 { converged == 0 } else { converged <= 1 };
            invariant ANewerReleaseClearsAConvergedLatch:
                if looked == 1 { superseded == 1 } else { superseded <= 1 };
            invariant NeverLandsTheSupersededActivation:
                if landed == 1 && newer == 1 { parked_newer == 1 } else { parked_newer <= 1 };
            invariant ParkedOnlyWhenTheLadderAdmits:
                if landed == 1 {
                    parked_warmup == 0 && (parked_phase == Land ||
                        (parked_keys == 1 && (parked_phase == 2 ||
                            (parked_phase == 1 && (parked_focused == 0 || parked_output == 1)) ||
                            (parked_phase == 0 && parked_quiet == 1))))
                } else {
                    landed <= 1
                };
        }
    }
}

/// Liveness spec (`StagedUpdateResolves`) for [`native_update_apply_ladder_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_apply_ladder_model)]
pub fn native_update_apply_ladder_liveness() -> Liveness {
    Liveness {
        name: "TheLadderLands",
        goal: or_(
            eq(var("landed"), int(1)),
            and_(eq(var("converged"), int(1)), eq(var("newer"), int(0))),
        ),
        weak: vec!["Advance", "WarmupEnds", "Due", "Lapse", "SupersedeRetryDue"],
        strong: vec!["Park", "CaptureRefused", "Supersede"],
        mutants: vec![
            "StandDown",
            "ParkMissLatches",
            "CaptureRefusedAsActivity",
            "NewerReleaseIgnored",
            "RefusedRetireSpendsTheRelease",
        ],
    }
}

/// Automatic-update quiet admission model after hidden PTY output.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_hidden_output_quiet_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateHiddenOutputQuiet {
            const Buggy = 0;
            const QuietTicks = 2;
            var phase = 0;
            var now_tick = 0;
            var latest_output_tick = 0;
            var presentation_stamp = 0;
            var wake_handled = 0;
            var activity_quiet = 0;
            var retry_deadline = 0;
            var attempted = 0;
            action HiddenOutput when (phase == 0) {
                phase = 1;
                now_tick = 1;
                latest_output_tick = 1;
                presentation_stamp = 1;
            }
            action WakeHandledNoPresent when (
                phase == 1 && presentation_stamp == 1
            ) {
                phase = 2;
                wake_handled = 1;
            }
            action PollRecentActivity when (
                phase == 2 && retry_deadline == 0 &&
                now_tick <= latest_output_tick + QuietTicks - 1
            ) {
                activity_quiet = 0;
                retry_deadline = now_tick + 1;
            }
            action QuietEpochElapses when (
                phase == 2 && wake_handled == 1
            ) {
                now_tick = latest_output_tick + QuietTicks;
                activity_quiet = 1;
                phase = 3;
                retry_deadline = 0;
            }
            action QuietGatedOnPresentationSample when (
                Buggy == 1 && phase == 2 && wake_handled == 1 &&
                presentation_stamp == 1
            ) {
                now_tick = latest_output_tick + QuietTicks;
                activity_quiet = 0;
                phase = 2;
                retry_deadline = latest_output_tick + QuietTicks;
            }
            action Attempt when (
                phase == 3 && activity_quiet == 1 && attempted == 0
            ) {
                phase = 4;
                attempted = 1;
            }
            action QuietFromLastPresent when (
                Buggy == 1 && phase == 2 && wake_handled == 1 &&
                now_tick <= latest_output_tick + QuietTicks - 1
            ) {
                activity_quiet = 1;
                phase = 3;
                retry_deadline = 0;
            }
            action AckSampleOnHiddenWake when (
                Buggy == 1 && phase == 1 && presentation_stamp == 1
            ) {
                phase = 2;
                wake_handled = 1;
                presentation_stamp = 0;
            }
            invariant OldHiddenPresentationCannotGate:
                if wake_handled == 1 &&
                    now_tick > latest_output_tick + QuietTicks - 1 {
                    activity_quiet == 1 && phase > 2
                } else {
                    activity_quiet <= 1
                };
            invariant ActivityRetryIsStrictlyFuture:
                if phase == 2 && retry_deadline > 0 {
                    retry_deadline > now_tick
                } else {
                    retry_deadline <= QuietTicks + 1
                };
            invariant AttemptOnlyAfterAgedQuiet:
                if attempted == 1 {
                    phase == 4 && wake_handled == 1 &&
                    now_tick > latest_output_tick + QuietTicks - 1
                } else {
                    phase <= 3
                };
            invariant HiddenSampleRemainsUnacknowledged:
                if phase > 0 { presentation_stamp == 1 } else { presentation_stamp == 0 };
            bound Bounds:
                phase <= 4 && now_tick <= QuietTicks + 1 &&
                latest_output_tick <= 1 && presentation_stamp <= 1 &&
                wake_handled <= 1 && activity_quiet <= 1 &&
                retry_deadline <= QuietTicks + 1 && attempted <= 1;
        }
    }
}

/// Identity discipline model for retrying a failed updater process replacement.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_attempt_identity_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateAttemptIdentity {
            const Buggy = 0;
            const MaxNonce = 2;
            const MaxAborts = 2;
            var phase = 0;
            var attempt_nonce = 0;
            var active_attempt_nonce = 0;
            var old_attempt_nonce = 0;
            var live_authority = 0;
            var abort_count = 0;
            var wrong_abort = 0;
            action StartAttempt when (
                phase == 0 && live_authority == 0 &&
                attempt_nonce <= MaxNonce - 1
            ) {
                phase = 1;
                attempt_nonce = attempt_nonce + 1;
                active_attempt_nonce = attempt_nonce + 1;
                live_authority = 1;
            }
            action AbortCurrent when (
                phase == 1 && live_authority == 1 &&
                abort_count <= MaxAborts - 1
            ) {
                phase = 0;
                old_attempt_nonce = active_attempt_nonce;
                active_attempt_nonce = 0;
                live_authority = 0;
                abort_count = abort_count + 1;
            }
            action ReplayOldAbort when (
                phase == 1 && live_authority == 1 && old_attempt_nonce > 0 &&
                active_attempt_nonce > old_attempt_nonce
            ) {
                phase = phase;
            }
            action AcceptStaleAbort when (
                Buggy == 1 && phase == 1 && live_authority == 1 &&
                old_attempt_nonce > 0 && active_attempt_nonce > old_attempt_nonce
            ) {
                phase = 0;
                active_attempt_nonce = 0;
                live_authority = 0;
                wrong_abort = 1;
            }
            action RetryReusingFailedNonce when (
                Buggy == 1 && phase == 0 && live_authority == 0 &&
                old_attempt_nonce > 0
            ) {
                phase = 1;
                active_attempt_nonce = old_attempt_nonce;
                live_authority = 1;
            }
            action AbortKeepingActiveIdentity when (
                Buggy == 1 && phase == 1 && live_authority == 1 &&
                abort_count <= MaxAborts - 1
            ) {
                phase = 0;
                old_attempt_nonce = active_attempt_nonce;
                live_authority = 0;
                abort_count = abort_count + 1;
            }
            invariant ActiveIdentityIsCurrent:
                if phase == 1 {
                    live_authority == 1 &&
                    active_attempt_nonce == attempt_nonce
                } else {
                    live_authority == 0 && active_attempt_nonce == 0
                };
            invariant RetryUsesFreshIdentity:
                if phase == 1 && old_attempt_nonce > 0 {
                    active_attempt_nonce > old_attempt_nonce
                } else {
                    old_attempt_nonce <= attempt_nonce
                };
            invariant StaleAbortCannotCancelRetry: wrong_abort == 0;
            bound NonceBounded: attempt_nonce <= MaxNonce;
            bound AbortsBounded: abort_count <= MaxAborts;
        }
    }
}

/// Accounting model for the bounded native-update facts worker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_worker_queue_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateWorkerQueue {
            const Buggy = 0;
            var connected = 1;
            var filler = 0;
            var queued_intent = 0;
            var pending = 0;
            var completion_ready = 0;
            var live_intent = 0;
            var purpose = 0;
            var drain_edge = 0;
            var delivered = 0;
            var failed_explicitly = 0;
            var restarts = 0;
            var idle_proxy_wakes = 0;
            var idle_warnings = 0;

            action ParkIdle when (
                live_intent == 0 && pending == 0 && queued_intent == 0 &&
                completion_ready == 0 && idle_proxy_wakes == 0 && idle_warnings == 0
            ) {
                idle_proxy_wakes = 0;
                idle_warnings = 0;
            }
            action ParkIdleMaterializingProxy when (
                Buggy == 1 && live_intent == 0 && pending == 0 &&
                queued_intent == 0 && completion_ready == 0 &&
                idle_proxy_wakes == 0 && idle_warnings == 0
            ) {
                idle_proxy_wakes = 1;
                idle_warnings = 1;
            }

            action OccupyWorker when (
                connected == 1 && filler == 0 && queued_intent == 0 &&
                live_intent == 0 && delivered == 0 && failed_explicitly == 0
            ) {
                filler = 1;
            }
            action RequestStageEmpty when (
                connected == 1 && filler == 0 && queued_intent == 0 &&
                live_intent == 0 && delivered == 0 && failed_explicitly == 0
            ) {
                queued_intent = 1;
                live_intent = 1;
                purpose = 1;
            }
            action RequestApplyEmpty when (
                connected == 1 && filler == 0 && queued_intent == 0 &&
                live_intent == 0 && delivered == 0 && failed_explicitly == 0
            ) {
                queued_intent = 1;
                live_intent = 1;
                purpose = 2;
            }
            action RequestStageFull when (
                connected == 1 && filler == 1 && queued_intent == 0 &&
                live_intent == 0 && delivered == 0 && failed_explicitly == 0
            ) {
                live_intent = 1;
                purpose = 1;
                pending = 1;
            }
            action RequestApplyFull when (
                connected == 1 && filler == 1 && queued_intent == 0 &&
                live_intent == 0 && delivered == 0 && failed_explicitly == 0
            ) {
                live_intent = 1;
                purpose = 2;
                pending = 1;
            }
            action DropApplyLatchWhenFull when (
                Buggy == 1 && connected == 1 && filler == 1 && queued_intent == 0 &&
                live_intent == 0 && delivered == 0 && failed_explicitly == 0
            ) {
                live_intent = 1;
                purpose = 2;
                pending = 0;
            }
            action UpgradePendingToApply when (
                live_intent == 1 && pending == 1 && purpose == 1
            ) {
                purpose = 2;
            }
            action WorkerDrainsFiller when (
                connected == 1 && filler == 1 && queued_intent == 0
            ) {
                filler = 0;
                drain_edge = 1;
            }
            action DrainWithoutRetryEdge when (
                Buggy == 1 && connected == 1 && filler == 1 && queued_intent == 0
            ) {
                filler = 0;
                drain_edge = 0;
            }
            action RetryPendingOnDrain when (
                connected == 1 && pending == 1 && filler == 0 &&
                queued_intent == 0 && drain_edge == 1
            ) {
                pending = 0;
                queued_intent = 1;
                drain_edge = 0;
            }
            action WorkerCompletesIntent when (
                connected == 1 && queued_intent == 1 && completion_ready == 0
            ) {
                queued_intent = 0;
                completion_ready = 1;
                drain_edge = 1;
            }
            action ReduceCompletion when (
                live_intent == 1 && completion_ready == 1
            ) {
                completion_ready = 0;
                live_intent = 0;
                purpose = 0;
                delivered = 1;
            }
            action ConsumeDrainEdge when (
                drain_edge == 1 && pending == 0
            ) {
                drain_edge = 0;
            }
            action DisconnectWithPending when (
                connected == 1 && pending == 1 && queued_intent == 0
            ) {
                connected = 0;
                filler = 0;
                drain_edge = 0;
            }
            action RestartPendingSuccess when (
                connected == 0 && pending == 1 && restarts == 0
            ) {
                connected = 1;
                pending = 0;
                queued_intent = 1;
                restarts = 1;
            }
            action RestartPendingUnavailable when (
                connected == 0 && pending == 1 && restarts == 0
            ) {
                pending = 0;
                live_intent = 0;
                purpose = 0;
                failed_explicitly = 1;
                restarts = 1;
            }
            action ReportUnavailableKeepingLatch when (
                Buggy == 1 && connected == 0 && pending == 1 && restarts == 0 &&
                failed_explicitly == 0
            ) {
                failed_explicitly = 1;
            }
            action RetryRestartEveryTurn when (
                Buggy == 1 && connected == 0 && pending == 1 && restarts <= 1
            ) {
                restarts = restarts + 1;
            }
            action DisconnectIdle when (
                connected == 1 && live_intent == 0 && filler == 0 &&
                queued_intent == 0 && completion_ready == 0
            ) {
                connected = 0;
            }
            action RequestDisconnectedRestartSuccess when (
                connected == 0 && live_intent == 0 && restarts == 0 &&
                delivered == 0 && failed_explicitly == 0
            ) {
                connected = 1;
                queued_intent = 1;
                live_intent = 1;
                purpose = 1;
                restarts = 1;
            }
            action RequestDisconnectedUnavailable when (
                connected == 0 && live_intent == 0 && restarts == 0 &&
                delivered == 0 && failed_explicitly == 0
            ) {
                failed_explicitly = 1;
                restarts = 1;
            }
            action Settled when (
                live_intent == 0 && filler == 0 && queued_intent == 0 &&
                pending == 0 && completion_ready == 0 && drain_edge == 0
            ) {
                connected = connected;
            }

            invariant NoSilentlyLostAcceptedIntent:
                if live_intent == 1 {
                    pending + queued_intent + completion_ready == 1
                } else {
                    pending + queued_intent + completion_ready == 0
                };
            invariant PendingEmptyQueueHasRetryEdge:
                if pending == 1 && filler == 0 && queued_intent == 0 &&
                    connected == 1 {
                    drain_edge == 1
                } else {
                    drain_edge <= 1
                };
            invariant ApplyPurposeSurvivesCoalescing:
                if live_intent == 1 && purpose == 2 {
                    pending + queued_intent + completion_ready == 1
                } else {
                    purpose <= 2
                };
            invariant SettlementIsExplicit:
                if live_intent == 0 && delivered + failed_explicitly > 0 {
                    delivered + failed_explicitly == 1
                } else {
                    delivered + failed_explicitly <= 1
                };
            invariant RestartAtMostOnce: restarts <= 1;
            invariant IdleParkHasNoProxyWake: idle_proxy_wakes == 0;
            invariant IdleParkHasNoWarning: idle_warnings == 0;
        }
    }
}

/// Writer/reader contract model for the updater `FailedMark` artifact memo.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn native_update_failed_mark_suppression_model() -> Model {
    let probed = || eq(var("phase"), int(3));
    Model {
        name: "NativeUpdateFailedMarkSuppression",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "writer_kind",
                init: 1,
            },
            StateVar {
                name: "probe_now",
                init: 0,
            },
            StateVar {
                name: "candidate_matches",
                init: 0,
            },
            StateVar {
                name: "quarantined",
                init: 0,
            },
            StateVar {
                name: "deadline",
                init: 0,
            },
            StateVar {
                name: "suppressed",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickSuppressionInputs",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "writer_kind",
                        expr: in_range(int(1), int(2)),
                    },
                    Update {
                        var: "probe_now",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "candidate_matches",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "RecordQuarantine",
                guard: Some(and_(
                    eq(var("phase"), int(1)),
                    eq(var("writer_kind"), int(1)),
                )),
                updates: vec![
                    Update {
                        var: "quarantined",
                        expr: if_(eq(cst("Buggy"), int(1)), int(0), int(1)),
                    },
                    Update {
                        var: "deadline",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                name: "RecordStageFailure",
                guard: Some(and_(
                    eq(var("phase"), int(1)),
                    eq(var("writer_kind"), int(2)),
                )),
                updates: vec![
                    Update {
                        var: "quarantined",
                        expr: int(0),
                    },
                    Update {
                        var: "deadline",
                        expr: int(2),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                name: "ProbeSuppresses",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "suppressed",
                        expr: if_(
                            and_(
                                eq(var("candidate_matches"), int(1)),
                                or_(
                                    eq(var("quarantined"), int(1)),
                                    if_(
                                        and_(
                                            eq(cst("Buggy"), int(1)),
                                            eq(var("writer_kind"), int(2)),
                                        ),
                                        le(var("deadline"), var("probe_now")),
                                        gt(var("deadline"), var("probe_now")),
                                    ),
                                ),
                            ),
                            int(1),
                            int(0),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "QuarantineVerdictLandsInTheQuarantineField",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(gt(var("phase"), int(1)), eq(var("writer_kind"), int(1))),
                    eq(var("quarantined"), int(1)),
                    le(var("quarantined"), int(1)),
                ),
            },
            Invariant {
                name: "QuarantineSuppressesAtEveryProbe",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(
                        probed(),
                        and_(
                            eq(var("writer_kind"), int(1)),
                            eq(var("candidate_matches"), int(1)),
                        ),
                    ),
                    eq(var("suppressed"), int(1)),
                    le(var("suppressed"), int(1)),
                ),
            },
            Invariant {
                name: "BackoffSuppressesIffBeforeDeadline",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(
                        probed(),
                        and_(
                            eq(var("writer_kind"), int(2)),
                            eq(var("candidate_matches"), int(1)),
                        ),
                    ),
                    if_(
                        gt(var("deadline"), var("probe_now")),
                        eq(var("suppressed"), int(1)),
                        eq(var("suppressed"), int(0)),
                    ),
                    le(var("suppressed"), int(1)),
                ),
            },
        ],
    }
}

/// Per-session ownership discipline model for seamless update handoff.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_seamless_handoff_ownership_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateSeamlessHandoffOwnership {
            const Buggy = 0;
            var phase = 0;
            var out_live = 1;
            var out_readers = 1;
            var out_holds_a = 1;
            var out_holds_b = 1;
            var cand_live = 0;
            var cand_readers = 0;
            var cand_holds_a = 0;
            var cand_holds_b = 0;
            var owner_a = 1;
            var owner_b = 1;
            var captured = 0;
            var proof_matched = 0;
            var revoked = 0;
            var warrant = 0;
            var resumed = 0;
            var commits = 0;

            action ParkOutgoingReaders when (phase == 0 && out_readers == 1) {
                phase = 1;
                out_readers = 0;
            }
            action CaptureCheckpoints when (phase == 1 && captured == 0) {
                phase = 2;
                captured = 1;
            }
            action StartReaderlessCandidate when (
                phase == 2 && cand_live == 0 && revoked == 0 && warrant == 0
            ) {
                phase = 3;
                cand_live = 1;
            }
            action DuplicateDescriptorsToCandidate when (
                phase == 3 && cand_live == 1 && revoked == 0 &&
                cand_holds_a == 0 && cand_holds_b == 0
            ) {
                phase = 4;
                cand_holds_a = 1;
                cand_holds_b = 1;
            }
            action MatchAdoptionProof when (
                phase == 4 && cand_live == 1 && captured == 1 && revoked == 0 &&
                cand_holds_a == 1 && cand_holds_b == 1 && proof_matched == 0
            ) {
                phase = 5;
                proof_matched = 1;
            }
            action RevokeBeforeCommit when (
                phase > 0 && phase <= 5 && revoked == 0 && commits == 0
            ) {
                revoked = 1;
            }
            action CommitAtomically when (
                phase == 5 && proof_matched == 1 && revoked == 0 && commits == 0 &&
                cand_live == 1 && out_live == 1 && out_readers == 0
            ) {
                phase = 6;
                commits = 1;
                owner_a = if cand_holds_a == 1 { 2 } else { 0 };
                owner_b = if cand_holds_b == 1 { 2 } else { 0 };
                out_live = 0;
                out_readers = 0;
                out_holds_a = if Buggy == 2 { out_holds_a } else { 0 };
                out_holds_b = 0;
            }
            action CandidateReleasesReaderGate when (
                phase == 6 && commits == 1 && cand_live == 1 && cand_readers == 0
            ) {
                cand_readers = 1;
            }
            action ProveNoCandidateExists when (
                phase > 0 && phase <= 2 && cand_live == 0 && warrant == 0
            ) {
                warrant = 1;
            }
            action ProveCandidateTerminated when (
                phase > 2 && phase <= 5 && cand_live == 1 && commits == 0 && warrant == 0
            ) {
                cand_live = 0;
                cand_holds_a = 0;
                cand_holds_b = 0;
                warrant = 1;
            }
            action ResumeParkedReaders when (
                phase > 0 && phase <= 5 && warrant == 1 && commits == 0 &&
                cand_live == 0 && cand_readers == 0 && out_live == 1 &&
                out_holds_a == 1 && out_holds_b == 1 &&
                owner_a == 1 && owner_b == 1
            ) {
                phase = 7;
                out_readers = 1;
                resumed = 1;
            }
            action RetireRolledBackAttempt when (phase == 7 && resumed == 1) {
                phase = 8;
            }
            action SettledCommitted when (phase == 6 && cand_readers == 1) {
                phase = 6;
            }
            action SettledRolledBack when (phase == 8) {
                phase = 8;
            }

            action BuggyMoveDescriptorsInsteadOfDuplicating when (
                Buggy == 1 && phase == 3 && cand_live == 1 &&
                cand_holds_a == 0 && cand_holds_b == 0
            ) {
                phase = 4;
                cand_holds_a = 1;
                cand_holds_b = 1;
                out_holds_a = 0;
                out_holds_b = 0;
            }
            action BuggyMatchProofOverAPartialDescriptorSet when (
                Buggy == 1 && phase == 3 && cand_live == 1 && captured == 1 &&
                cand_holds_a == 0 && cand_holds_b == 0 && proof_matched == 0
            ) {
                phase = 5;
                cand_holds_a = 1;
                proof_matched = 1;
            }
            action BuggyCommitWithoutProof when (
                Buggy == 1 && phase == 4 && proof_matched == 0 && commits == 0 &&
                cand_live == 1 && out_live == 1
            ) {
                phase = 6;
                commits = 1;
                owner_a = 2;
                owner_b = 2;
                out_live = 0;
                out_readers = 0;
                out_holds_a = 0;
                out_holds_b = 0;
            }
            action BuggyCommitTwice when (Buggy == 1 && phase == 6 && commits == 1) {
                commits = 2;
            }
            action BuggyExitWithoutTransferringOwnership when (
                Buggy == 1 && phase > 0 && phase <= 5 && out_live == 1 && commits == 0
            ) {
                out_live = 0;
                out_readers = 0;
                out_holds_a = 0;
                out_holds_b = 0;
            }
            action BuggyCandidateReadsBeforeCommit when (
                Buggy == 1 && phase > 3 && phase <= 5 && cand_live == 1 &&
                cand_holds_a == 1 && cand_readers == 0 && commits == 0
            ) {
                cand_readers = 1;
            }
            action BuggyResumeWithoutTerminationProof when (
                Buggy == 1 && phase > 2 && phase <= 5 && cand_live == 1 &&
                out_live == 1 && out_readers == 0 && commits == 0
            ) {
                phase = 7;
                out_readers = 1;
                resumed = 1;
            }

            invariant NoSessionIsEverOrphaned:
                (if owner_a == 1 {
                    out_live
                } else if owner_a == 2 {
                    cand_live
                } else {
                    0
                }) == 1 &&
                (if owner_b == 1 {
                    out_live
                } else if owner_b == 2 {
                    cand_live
                } else {
                    0
                }) == 1;
            invariant AnOwnerHoldsItsMaster:
                (if owner_a == 1 {
                    out_holds_a
                } else if owner_a == 2 {
                    cand_holds_a
                } else {
                    0
                }) == 1 &&
                (if owner_b == 1 {
                    out_holds_b
                } else if owner_b == 2 {
                    cand_holds_b
                } else {
                    0
                }) == 1;
            invariant NeverTwoReadersOnOneMaster:
                (if out_readers == 1 && out_holds_a == 1 && cand_holds_a == 1 {
                    cand_readers
                } else {
                    0
                }) == 0 &&
                (if out_readers == 1 && out_holds_b == 1 && cand_holds_b == 1 {
                    cand_readers
                } else {
                    0
                }) == 0;
            invariant NoCommitWithoutProof:
                if commits > 0 {
                    proof_matched == 1 && captured == 1 && revoked == 0 &&
                    cand_live == 1 && out_live == 0 && phase > 5
                } else {
                    commits == 0
                };
            invariant AtMostOneCommitPerAttempt: commits <= 1;
            invariant RollbackRemainsAvailableBeforeCommit:
                if commits == 0 && phase > 0 && phase <= 5 {
                    out_live == 1 && out_readers == 0 &&
                    out_holds_a == 1 && out_holds_b == 1 &&
                    owner_a == 1 && owner_b == 1 && cand_readers == 0
                } else {
                    phase <= 8
                };
            invariant ResumeRequiresTerminationWarrant:
                if resumed == 1 {
                    warrant == 1 && cand_live == 0 && cand_readers == 0 &&
                    commits == 0 && out_live == 1 &&
                    owner_a == 1 && owner_b == 1
                } else {
                    resumed == 0
                };
            invariant CandidateStaysReaderlessUntilCommit:
                if cand_readers == 1 {
                    commits > 0 && out_live == 0 && out_readers == 0
                } else {
                    cand_readers == 0
                };
            invariant ExitedProcessReadsNothing:
                if out_live == 0 {
                    out_readers == 0 && out_holds_a == 0 && out_holds_b == 0
                } else {
                    out_live == 1
                };
            bound OwnershipBounds:
                phase <= 8 && out_live <= 1 && out_readers <= 1 &&
                out_holds_a <= 1 && out_holds_b <= 1 && cand_live <= 1 &&
                cand_readers <= 1 && cand_holds_a <= 1 && cand_holds_b <= 1 &&
                owner_a <= 2 && owner_b <= 2 && captured <= 1 &&
                proof_matched <= 1 && revoked <= 1 && warrant <= 1 &&
                resumed <= 1 && commits <= 2;
        }
    }
}

/// Convergence latch model for the automatic update apply lane.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_structural_latch_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateStructuralLatch {
            const Buggy = 0;
            const UnreadLooks = 3;
            const UnprovenLooks = 3;
            var phase = 0;
            var room = 0;
            var unread = 0;
            var looks = 0;
            var doubts = 0;
            var latched = 1;
            var pending = 0;
            var day = 0;
            var owed = 1;
            var spent = 0;
            var newer = 0;
            var newer_spent = 0;
            var said = 0;
            var escaped = 0;
            var superseding = 0;
            var superseded = 0;
            var refused = 0;
            var backoff = 0;
            var booted_old = 0;
            action TrialHasRoom when (phase == 0) {
                room = 1;
                phase = 1;
                pending = 1;
            }
            action TrialIsSpent when (phase == 0) {
                room = 0;
                phase = 1;
                pending = 1;
            }
            action TrialUnreadable when (phase == 1 && (unread == 0 || unread == 2) &&
                superseded == 0) {
                unread = 1;
                pending = 1;
            }
            action TrialUnproven when (phase == 1 && unread <= 1 && superseded == 0) {
                unread = 2;
                pending = 1;
            }
            action TrialReadAgain when (phase == 1 && unread > 0) {
                unread = 0;
                pending = 1;
            }
            action DayPasses when (phase == 1 && day == 0 && superseded == 0) {
                day = 1;
                pending = 1;
            }
            action NewerArrives when (phase == 1 && newer == 0) {
                newer = 1;
                pending = 1;
            }
            action Decide when (phase == 1 && latched == 1 && pending == 1 && superseding == 0) {
                superseding = if newer == 1 && newer_spent == 0 && backoff == 0 &&
                    (Buggy == 0 || day == 1) {
                    1
                } else {
                    superseding
                };
                latched = if (Buggy == 0 && (newer == 0 || newer_spent == 1) &&
                        day == 1 && owed == 1 && unread == 0 && room == 1) ||
                    (Buggy == 1 && newer == 1 && newer_spent == 0 && backoff == 0 &&
                        day == 0 && unread == 0 && room == 1) ||
                    (Buggy == 1 && (newer == 0 || newer_spent == 1) &&
                        day == 1 && owed == 1 && unread == 0 && room == 0) {
                    0
                } else {
                    1
                };
                booted_old = if Buggy == 1 && newer == 1 && newer_spent == 0 && backoff == 0 &&
                    day == 0 && unread == 0 && room == 1 {
                    1
                } else {
                    booted_old
                };
                owed = if (Buggy == 0 && (newer == 0 || newer_spent == 1) && unread == 0 &&
                        ((day == 1 && owed == 1) || room == 0)) ||
                    (Buggy == 0 && (newer == 0 || newer_spent == 1) && day == 1 &&
                        owed == 1 && unread == 1 && looks == UnreadLooks) ||
                    (Buggy == 0 && newer == 1 && newer_spent == 0 && backoff == 1 &&
                        unread == 0 && room == 0) ||
                    (Buggy == 1 && newer == 1 && newer_spent == 0 && backoff == 0 &&
                        day == 0 && unread == 0 && room == 0) ||
                    (Buggy == 1 && (newer == 0 || newer_spent == 1) &&
                        day == 1 && owed == 1 && unread == 0 && room == 0) {
                    0
                } else {
                    owed
                };
                spent = if (Buggy == 0 && (newer == 0 || newer_spent == 1) && owed == 1 &&
                        unread == 0 && (day == 1 || room == 0)) ||
                    (Buggy == 0 && (newer == 0 || newer_spent == 1) && day == 1 &&
                        owed == 1 && unread == 1 && looks == UnreadLooks) ||
                    (Buggy == 0 && newer == 1 && newer_spent == 0 && backoff == 1 &&
                        owed == 1 && unread == 0 && room == 0) ||
                    (Buggy == 1 && newer == 1 && newer_spent == 0 && backoff == 0 &&
                        day == 0 && owed == 1 && unread == 0 && room == 0) ||
                    (Buggy == 1 && (newer == 0 || newer_spent == 1) &&
                        day == 1 && owed == 1 && unread == 0 && room == 0) {
                    1
                } else {
                    spent
                };
                newer_spent = if newer == 1 && newer_spent == 0 && backoff == 0 {
                    1
                } else {
                    newer_spent
                };
                said = if (Buggy == 0 && (newer == 0 || newer_spent == 1 || backoff == 1) &&
                        owed == 1 && unread == 0 && room == 0) ||
                    (Buggy == 0 && (newer == 0 || newer_spent == 1) && day == 1 &&
                        owed == 1 && unread == 1 && looks == UnreadLooks) ||
                    (Buggy == 1 && newer == 1 && newer_spent == 0 && backoff == 0 &&
                        day == 0 && unread == 0 && room == 0) {
                    1
                } else {
                    said
                };
                looks = if (newer == 0 || newer_spent == 1) && day == 1 && owed == 1 &&
                    unread == 1 && looks <= UnreadLooks - 1 {
                    looks + 1
                } else if (unread == 1 &&
                    ((newer == 1 && newer_spent == 0) || day == 0 || owed == 0)) ||
                    unread == 2 {
                    looks
                } else {
                    0
                };
                doubts = if (newer == 0 || newer_spent == 1) && day == 1 && owed == 1 &&
                    unread == 2 &&
                    (doubts <= UnprovenLooks - 1 || (Buggy == 1 && doubts == UnprovenLooks)) {
                    doubts + 1
                } else if unread == 2 &&
                    ((newer == 1 && newer_spent == 0) || day == 0 || owed == 0) {
                    doubts
                } else {
                    0
                };
                day = if Buggy == 0 && (newer == 0 || newer_spent == 1) && day == 1 &&
                    owed == 1 && unread == 2 && doubts == UnprovenLooks {
                    0
                } else {
                    day
                };
                pending = if (newer == 0 || newer_spent == 1) && day == 1 && owed == 1 &&
                    ((unread == 1 && looks <= UnreadLooks - 1) ||
                        (unread == 2 && (doubts <= UnprovenLooks - 1 ||
                            (Buggy == 1 && doubts == UnprovenLooks)))) {
                    1
                } else {
                    0
                };
            }
            action Supersede when (phase == 1 && superseding == 1) {
                superseding = 0;
                superseded = 1;
                latched = 0;
            }
            action SupersedeRefused when (phase == 1 && superseding == 1) {
                superseding = 0;
                refused = 1;
                said = 1;
                newer_spent = if Buggy == 1 { newer_spent } else { 0 };
                backoff = if Buggy == 1 { backoff } else { 1 };
                pending = 1;
            }
            action RetryDue when (phase == 1 && backoff == 1) {
                backoff = 0;
                pending = 1;
            }
            action AttemptFails when (phase == 1 && latched == 0 && superseded == 0) {
                latched = 1;
                owed = if Buggy == 1 { 1 } else { owed };
                day = if Buggy == 1 { 0 } else { day };
            }
            action ArmSameBuild when (phase == 1 && latched == 1) {
                latched = if Buggy == 1 { 0 } else { latched };
                escaped = if Buggy == 1 { 1 } else { escaped };
            }
            invariant ANewerReleaseClearsTheLatch:
                if pending == 0 && latched == 1 && newer == 1 {
                    superseding == 1 || backoff == 1
                } else {
                    refused <= 1
                };
            invariant NeverBootsTheOlderActivation: booted_old == 0;
            invariant ResampleAfterADay:
                if pending == 0 && latched == 1 && room == 1 && day == 1 &&
                    superseding == 0 && backoff == 0 {
                    owed == 0
                } else {
                    owed <= 1
                };
            invariant ATrialHoldIsSaid:
                if pending == 0 && phase == 1 && unread == 0 && room == 0 && superseding == 0 &&
                    superseded == 0 && (day == 1 || newer == 1) {
                    said == 1
                } else {
                    said <= 1
                };
            invariant NoRetryPromisedPastTheTrial:
                if pending == 0 && phase == 1 && unread == 0 && room == 0 &&
                    superseding == 0 && superseded == 0 {
                    owed == 0
                } else {
                    owed <= 1
                };
            invariant NoRetryPromisedPastTheBound:
                if pending == 0 && latched == 1 && unread == 1 && day == 1 &&
                    (newer == 0 || newer_spent == 1) && superseding == 0 {
                    owed == 0
                } else {
                    owed <= 1
                };
            invariant UnprovenLooksAreBounded: doubts <= UnprovenLooks;
            invariant NeverSpendsTheRevertingLaunch:
                if room == 0 { latched == 1 || superseded == 1 } else { latched <= 1 };
            invariant ResampleAtMostOnce:
                if spent == 1 { owed == 0 } else { owed <= 1 };
            invariant SameBuildStaysLatched: escaped == 0;
            invariant ReleasedOnlyByAnEvent:
                if latched == 0 { day == 1 || newer == 1 } else { latched == 1 };
        }
    }
}
