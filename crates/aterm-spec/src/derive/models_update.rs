// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `native_update_*` models: update admission, handoff, apply ladder and worker.

use super::*;

/// A fixed control endpoint belongs to the outgoing process until exact Commit and its
/// exit; a candidate may be rejected before Commit without changing any socket or token
/// bytes. Tier-1 drives the real GUI bind worker and reader gate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_control_socket_handoff_model() -> Model {
    crate::ty_model! {
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

/// The successor's dial of the out-of-band handoff rendezvous (the macOS LaunchServices
/// lane): the claim secret goes only to the attested outgoing process. A path is not an
/// identity, so the dialer presents the claim only when the kernel says the listener is
/// same-uid (`getpeereid`), is the attested pid (`LOCAL_PEERPID` on the dialing end), and
/// still has the attested birth witness (a recycled pid fails).
///
/// The three environment actions are the ways a listener can fail to be the parent.
/// `PresentClaimUidOnly` is the uid gate alone, which any same-uid listener at the
/// published path passes. Tier-1: `handoff_rendezvous::claim_presentation_conformance`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_rendezvous_claim_model() -> Model {
    crate::ty_model! {
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

/// The outgoing side of the same rendezvous: no descriptor leaves toward a dialer a
/// same-uid process could stand in for, nothing is committed to a dialer LaunchServices
/// disowns, a code check macOS did not answer turns no successor away, and a refusal is
/// filed by its cause.
///
/// The claim secret is no proof of who dials: the launch environment it travels in is
/// readable by every process of this uid (`KERN_PROCARGS2`), and LaunchServices names the
/// launched pid only at check-in, after the grant. What a stranger cannot assert is the
/// code it runs: the connection's audit token names the dialer, whose code must meet the
/// successor's requirement (`handoff_rendezvous::DialerCode`). Until Commit,
/// LaunchServices still corroborates, and disowns the dialer when it names another pid or
/// says the launched process exited before check-in while the dialer lives.
///
/// The code check waits on `trustd`, which can stop answering: an unanswered check is
/// neither pass nor refusal, so the gate holds the dialer and checks again until answered
/// or the dial deadline ends the wait (closing it would end the attempt, since the
/// successor exits at end of file). A turned-away successor exits by its own rule, so the
/// attempt is filed by the refusal's cause (an unrun check a passing failure, a refusal a
/// verdict on the code), never as an unexplained death.
///
/// State: `secret` — the dialer presented this attempt's claim; `code` — its code meets
/// the requirement; `answered` — `trustd` answers; `disowned` — LaunchServices disowns it;
/// `phase` — 0 holding the claim, 1 granted, 2 committed, 3 refused (before the grant the
/// connection is closed and the dialer proven gone; after, it is killed and reaped), 4 the
/// dial deadline ended a hold; `filed` — 0 not yet, 1 by cause, 2 as an unexplained death.
///
/// Mutants, each its own action: `GrantOnSecretAlone`, `CommitOverDisowning` ("exited
/// before it checked in" read as corroborating), `RefuseUnanswered`, and
/// `FileAsUnexplainedDeath` (`ChildDied` with nothing observed). Tier-1:
/// `app_update_handoff::launched_dialer_admission_conformance`, with real dialer
/// processes, an overdue `trustd` gate, and live, dead and unreaped dialers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_rendezvous_admission_model() -> Model {
    crate::ty_model! {
        NativeUpdateRendezvousAdmission {
            const Buggy = 0;
            var secret = 1;
            var code = 1;
            var answered = 1;
            var disowned = 0;
            var phase = 0;
            var filed = 0;

            // The environment: what a dialer, trustd and LaunchServices turn out to say.
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
            // The outgoing process.
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
            // Mutants, dead at Buggy = 0.
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

/// Admission and handoff policy for applying an already verified native update.
///
/// A foreground terminal job is not dirty native UI state: the seamless handoff adopts its
/// PTY, so it is admitted when that lane is available. If the lane cannot engage, a
/// foreground job forbids the destructive cold re-exec and leaves the update retryable.
/// Dirty or pending native documents remain a blocker.
///
/// `CompleteColdFallback` re-tests the foreground fact as the shipping cold arms do: the
/// Windows arm execs only `if live_ptys == 0`, while the unix arm's
/// `debug_assert!(live.is_empty())` is compiled out of a release build, so on macOS the
/// exec follows `classify` alone.
///
/// `Buggy=1` mutants, each its own action: `BlockForegroundDespiteSeamless`;
/// `HandoffWithoutAdoptingForeground`; `ReexecColdOverLiveSessions` (`classify`'s
/// live-session check dropped); `ClassifySeamlessOverUncertifiedState` (its native-state
/// check dropped); `BlockLatchesWithoutRetry` (a block filed as a failed handoff, latching
/// automatic apply off for the build).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_admission_model() -> Model {
    crate::ty_model! {
        NativeUpdateAdmission {
            const Buggy = 0;
            const MaxAttempts = 2;
            // phase: 0 Staged, 1 AuthorizedAttempt, 2 Replaced, 3 Blocked.
            // decision: 0 None, 1 Seamless, 2 ColdFallback, 3 Blocked.
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
                foreground_job == 0
            ) {
                phase = 1;
                decision = 2;
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
            }
            // Classification and exec in one action: in a release build the unix cold
            // arm's re-check is a compiled-out `debug_assert!`.
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
            bound AttemptsBounded: attempt_count <= MaxAttempts;
        }
    }
}

/// Ordering contract for persistent automatic-update intent.
///
/// A durable-stage wake may race an active manual check: the wake arms intent at once, and
/// completion imports the stage and makes that intent eligible. A blocked or failed
/// attempt returns to the same retryable ready state. Terminal idleness is a bounded
/// preference: activity defers the park only while the grace window is open
/// (`GraceWindowCloses`), so a never-quiet machine still updates.
///
/// `Buggy=1` mutants, each its own action: `StageWakeDroppingIntent` drops intent because
/// a manual check is active; `StaleWakeClearsNewerIntent`; `ParkWithoutQuietOrGrace`
/// (`AutomaticAttemptRequiresQuietOrClosedGraceWindow`); `AcceptWithoutPark` commits over
/// a screen nobody froze; `AttemptDidNotReplaceSpendingIntent` treats any non-accepted
/// result as consumed; `AttemptPhysicalFailureKeepingIntent` latches manual-only but
/// leaves the intent armed, so the next poll walks past the latch.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_auto_intent_model() -> Model {
    crate::ty_model! {
        NativeUpdateAutoIntent {
            const Buggy = 0;
            const MaxAttempts = 2;
            const MaxDeferrals = 3;
            // phase: 0 Idle, 1 ManualChecking, 2 Ready, 3 Attempting,
            // 4 Accepted, 5 NewerIntentPending, 6 ManualOnly.
            var phase = 0;
            var active_check = 0;
            var durable_stage = 0;
            var staged = 0;
            var intent = 0;
            var attempts = 0;
            var last_unsuccessful = 0;
            var accepted = 0;
            var manual_only = 0;
            // target: 0 None, 1 Incoming, 2 NewerAlreadyArmed.
            var target = 0;
            var stale_wake_seen = 0;
            var quiet = 0;
            // The park, not the attempt. On the launched lane the attempt starts by
            // launching the successor with every reader live; the park comes later, when
            // `ParkReaders` finds a moment. The quiet-or-past-grace invariant binds the
            // instant the terminal freezes; the launch keeps its own gate (`Attempt`'s
            // guard, bound to the real poll policy in conformance).
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
            // The bounded idle-preference window closes on a machine that keeps producing
            // activity: quiet is preferred, never required forever.
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
            // The hold between launch and park: the successor boots or holds its claim
            // with every reader live, so activity only postpones the park. A machine quiet
            // at authorization can be busy again when the successor is ready.
            action HoldActivity when (
                phase == 3 && parked == 0 && deferrals <= MaxDeferrals - 1
            ) {
                quiet = 0;
                deferrals = deferrals + 1;
            }
            action HoldQuietElapsed when (phase == 3 && parked == 0 && quiet == 0) {
                quiet = 1;
            }
            // The park, gated on the facts re-read at this instant
            // (`prelaunch_park_admitted`).
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
            // A park that missed its freeze budget: readers resume, nothing granted, the
            // successor still held, the next park gated afresh.
            action ReparkAfterMissedBudget when (
                phase == 3 && parked == 1 && accepted == 0
            ) {
                parked = 0;
            }
            // The hold cap: the terminal never offered a moment to pause. The intent is
            // retained and no reader was stopped.
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
            // Commit requires the park: no path to acceptance skips it.
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
            // The successor's proof is over a screen captured with every reader stopped,
            // so an accepted attempt that never parked would Commit over an unfrozen one.
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

/// The apply ladder (`docs/DESIGN-auto-apply-ladder-2026-09-21.md`): a verified staged
/// build lands within a bound on a machine that is never quiet, and activity delays the
/// landing without ever latching the lane manual-only.
///
/// `phase` is the ladder (0 PreferIdle, 1 PreferOutputGap, 2 KeysOnly, 3 Land) and
/// `Advance` the wall clock. `Busy`/`Quiet`, `KeysDown`/`KeysUp` and `Blur`/`Focus` are the
/// terminal. `Park` is the one decision, guarded by the ladder's rule for the live phase:
/// the shipping `automatic_park_refusal`, which Tier-1 (`native_updater_conformance`)
/// binds to this guard fact by fact.
///
/// `WarmupStarts`/`WarmupEnds` are the consent warm-up, the one hold no phase relaxes (or
/// a landing could arrive while the macOS dialog the person asked for is on screen). Every
/// admitted park requires it ended; `WarmupEnds` is always enabled while it holds (the
/// shipping hold is capped), so the ladder still lands. `warmup` also stands for a cold
/// restore's agents still being relaunched (`ActivityFacts::harness_restored_pending`,
/// capped by `RESTORED_HOLD`); the Tier-1 bind projects their disjunction onto it.
///
/// `ParkMissed` (freeze budget missed: readers back, nothing granted) is a fact about the
/// moment and keeps the lane where it is. `PhysicalFailure` latches for the schedule's
/// spacing while the clock keeps running, and `Lapse` resumes the ladder where the clock
/// is, only once `Due` (the latch's `retry_at`) has passed. `PassingFailure` (a fact that
/// changes by itself: `codesign` out of the apply budget, a sibling holding the apply lock,
/// a helper that would not start, a successor that exited 75 on a deferred swap) latches on
/// the transient schedule and never converges.
///
/// `DeskRefuses` puts a session into a state the capture cannot carry as it stands, and
/// `DeskChanges` moves the desk on. While the desk refuses, a park is admitted only once
/// `CaptureRefused` has carried the refusing session at a degraded rung (`degraded`).
///
/// `BundleSwap` is the failed candidate having boot-applied the bundle: the next reconcile
/// retires the download for the same update's activation and re-keys the latch, deadline
/// and all. `Converge` is the structural verdict on that activation with its re-sample
/// spent or held: no deadline comes. `NewerRelease` is a verified newer build behind it,
/// and the healthy look is `Supersede`: the running build takes the install path back, the
/// activation's trial is disarmed without a launch, and the newer build starts a ladder of
/// its own (the only place the phase starts over). `SupersedeRefused` (verification out of
/// budget on a captive portal, the apply lock held, a stage being replaced, a disk the
/// retire cannot undo) moves nothing and does not spend the release: it waits out `retry`
/// (cleared by `SupersedeRetryDue`) and is offered again.
///
/// `Buggy=1` mutants, each its own dead action:
/// * `StandDown` — a busy terminal past `Hold` ticks stands the lane down and the run
///   wedges (`ActivityNeverLatchesManualOnly`);
/// * `ParkWithoutTheRule` — a park the ladder's rule did not admit
///   (`ParkedOnlyWhenTheLadderAdmits`);
/// * `ParkMissLatches` — a park miss filed as a physical failure
///   (`ActivityNeverLatchesManualOnly`);
/// * `LapseRestartsTheLadder` — one 600 s latch buys a fresh ladder
///   (`TheLadderNeverRestarts`, a high-water mark);
/// * `CaptureRefusedAsActivity` — the refusal re-parked as activity, stood down as
///   `ActivityRevoked` and retried every fifteen minutes forever
///   (`RefusalNeverRetriesAsActivity`; `refusals` is a flag to keep the space finite);
/// * `BundleSwapClearsLatch` — the activation arms 500 ms later and the "confirming retry,
///   ten minutes out" runs within seconds, then converges for a day (`NoEarlyRelease`);
/// * `NewerReleaseIgnored` and `NewerReleaseRetriesTheActivation` — the look keeps the
///   latch, or opens it for the activation, whose successor refuses to boot-apply past
///   itself (`ANewerReleaseClearsAConvergedLatch`, `NeverLandsTheSupersededActivation`);
/// * `RefusedRetireSpendsTheRelease` — the release spent on a refusal
///   (`ANewerReleaseClearsAConvergedLatch`, and the liveness);
/// * `PassingFailureConverges` — the moment's verdict booked structural, holding a healthy
///   build for a day or for good (`APassingFailureNeverConverges`). The liveness cannot
///   catch it: a converged latch with nothing newer is Law 2's legitimate rest.
///
/// Law 1, the landing itself, is checked by [`native_update_apply_ladder_liveness`]: every
/// invariant here is a bad state and the deadlock check sees only a state with no way out,
/// while a lane that retries the same refusal or waits behind a latch no deadline lifts
/// has neither. `CaptureRefusedAsActivity` and `NewerReleaseIgnored` are genuine
/// livelocks: after the mutant's step the healthy answer is unavailable.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_apply_ladder_model() -> Model {
    crate::ty_model! {
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
            // The desk: a session the capture cannot carry as it stands, whether this
            // attempt's capture answered for it at a degraded rung, and how many
            // refusals were re-filed as activity (the mutant).
            var refusing = 0;
            var degraded = 0;
            var refusals = 0;
            // The latch's `retry_at` has passed; the failed candidate's bundle swap
            // retired the download for its activation.
            var due = 0;
            var swapped = 0;
            // The converged latch: the structural verdict on the activation with no
            // deadline left, a newer release behind it, whether the lane looked at it,
            // whether it took the activation's place, and whether a landing was it.
            var converged = 0;
            var newer = 0;
            var looked = 0;
            var superseded = 0;
            var parked_newer = 0;
            // A refused retire's release waits out its retry deadline.
            var retry = 0;
            // The latched failure was a passing moment (a check that did not finish, a
            // lock held past its wait, a successor that exited 75 on a deferred swap).
            var passing = 0;
            // The wall clock. Not guarded on the latch: the anchor keeps counting while a
            // physical-failure latch holds; `reached` is the high-water mark the phase may
            // never fall below.
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
            // The consent warm-up: held in every phase, and bounded.
            action WarmupStarts when (landed == 0 && manual_only == 0 && warmup == 0) {
                warmup = 1;
            }
            action WarmupEnds when (landed == 0 && manual_only == 0 && warmup == 1) {
                warmup = 0;
            }
            // A session enters a state the capture cannot carry as it stands (an OSC 7
            // with `%00`, a grid over the per-grid cap, a parser left mid-string).
            action DeskRefuses when (landed == 0 && manual_only == 0) {
                refusing = 1;
            }
            // The desk moves on (a session closed, resized or left the alt screen): a
            // carry answered for the old desk says nothing about the new one.
            action DeskChanges when (landed == 0 && manual_only == 0) {
                refusing = 0;
                degraded = 0;
            }
            // Mutant: a busy terminal past `Hold` ticks stands the lane down; nothing is
            // enabled afterwards.
            action StandDown when (
                Buggy == 1 && landed == 0 && manual_only == 0 && quiet == 0 &&
                phase > Hold - 1
            ) {
                manual_only = 1;
            }
            // A refusing desk parks only once the capture has answered for it at a
            // degraded rung.
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
            // A park that missed its freeze budget: readers back, nothing granted. A
            // fact about the moment; the lane keeps its phase and anchor and is gated
            // afresh, never latched.
            action ParkMissed when (
                landed == 0 && manual_only == 0 && latched == 0 && warmup == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                quiet = 0;
            }
            // A park the capture refused: a deterministic fact about the desk. The
            // healthy lane carries the refusing session at a degraded rung; what no rung
            // can lower is typed `CaptureRefused` and retried when the desk changes,
            // never on the activity spacing.
            //
            // `refusals == 0` always holds on the healthy lane. It makes the mutant a
            // loop rather than a detour: a lane that files a refusal as activity files
            // every refusal there, so it has no refusal lane to come back to.
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
            // Mutant: the refusal filed as a park miss, so the desk is never answered for
            // and the refusal returns every fifteen minutes forever. `TheLadderLands`
            // catches the loop, `RefusalNeverRetriesAsActivity` its first step.
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
            // A genuine physical failure of an admitted park (the successor died, the
            // proof did not match): latched for the physical schedule's spacing. Once.
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
            // A passing failure of an admitted park (verification out of budget, the apply
            // lock held past its wait, a helper that could not start, or the successor's
            // swap deferred for one of those: exit 75). Latched on the transient
            // schedule, never converged. Once, like `PhysicalFailure`.
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
            // Mutant: the moment's verdict booked structural (or as the installed copy's),
            // so the latch converges with no deadline and a healthy build is held for a
            // day, or until a person reinstalls.
            action PassingFailureConverges when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 1 &&
                passing == 1 && due == 0 && converged == 0
            ) {
                converged = 1;
            }
            // The latch's deadline passes (its `retry_at`, 600 s out for the
            // first failure).
            action Due when (
                landed == 0 && manual_only == 0 && latched == 1 && due == 0 && converged == 0
            ) {
                due = 1;
            }
            // The latch lapses at its deadline: the intent is re-armed and the ladder
            // resumes where the clock is.
            action Lapse when (
                landed == 0 && manual_only == 0 && latched == 1 && due == 1
            ) {
                latched = 0;
            }
            // The failed candidate had already swapped the bundle: the download retires
            // for the same update's activation and the latch is re-keyed to it, deadline
            // untouched. Once, to keep the space finite.
            action BundleSwap when (
                landed == 0 && manual_only == 0 && failures == 1 && swapped == 0
            ) {
                swapped = 1;
            }
            // Mutant: the retire arm clears the latch, and the activation arms half a
            // second after the failure.
            action BundleSwapClearsLatch when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 1 &&
                swapped == 0
            ) {
                swapped = 1;
                latched = 0;
            }
            // The structural convergence on the activation: the re-keyed latch stands
            // with no deadline left.
            action Converge when (
                landed == 0 && manual_only == 0 && latched == 1 && swapped == 1 &&
                due == 0 && converged == 0 && passing == 0
            ) {
                converged = 1;
            }
            // A verified newer release is staged behind it.
            action NewerRelease when (
                landed == 0 && manual_only == 0 && converged == 1 && newer == 0
            ) {
                newer = 1;
            }
            // The healthy look: the newer build takes the activation's place (the
            // running build back at the install path, the activation's trial disarmed,
            // never launched) and is armed as a new artifact on its own ladder, so the
            // phase starts over here and only here.
            //
            // `looked == 0` makes a look final (`AutoApplyStructuralVerdict::newer_spent`):
            // a look that kept the latch would leave the newer release waiting forever.
            // `retry == 0`: a refused release is offered only once its deadline passes.
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
            // The retire refused (the environment's): nothing moved, the latch stands,
            // and the release is not spent; it is offered again after a retry deadline.
            action SupersedeRefused when (
                landed == 0 && manual_only == 0 && converged == 1 && newer == 1 &&
                superseded == 0 && looked == 0 && retry == 0
            ) {
                retry = 1;
            }
            // The refused release's retry deadline passes
            // (`AutoApplyStructuralVerdict::newer_retry`).
            action SupersedeRetryDue when (landed == 0 && manual_only == 0 && retry == 1) {
                retry = 0;
            }
            // Mutant: the look keeps the latch and the newer release waits for good. A
            // livelock no deadlock check sees; `TheLadderLands` does.
            action NewerReleaseIgnored when (
                Buggy == 1 && landed == 0 && manual_only == 0 && converged == 1 &&
                newer == 1 && looked == 0 && superseded == 0 && retry == 0
            ) {
                looked = 1;
            }
            // Mutant: the latch opens for one more attempt at the activation, which is
            // what then lands.
            action NewerReleaseRetriesTheActivation when (
                Buggy == 1 && landed == 0 && manual_only == 0 && converged == 1 &&
                newer == 1 && looked == 0 && superseded == 0 && retry == 0
            ) {
                looked = 1;
                latched = 0;
            }
            // Mutant: a refused retire spends the release, whatever refused it, so the
            // latch stands over it for good (a livelock).
            action RefusedRetireSpendsTheRelease when (
                Buggy == 1 && landed == 0 && manual_only == 0 && converged == 1 &&
                newer == 1 && looked == 0 && superseded == 0 && retry == 0
            ) {
                looked = 1;
            }
            // Mutant: a park miss filed as a physical failure; the lane latches.
            action ParkMissLatches when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 0 && warmup == 0 &&
                (phase == Land ||
                    (keys == 1 && (phase == 2 ||
                        (phase == 1 && (focused == 0 || output == 1)) ||
                        (phase == 0 && quiet == 1))))
            ) {
                manual_only = 1;
            }
            // Mutant: a lapse that clears the anchor, so the 600 s latch buys a fresh ladder.
            action LapseRestartsTheLadder when (
                Buggy == 1 && landed == 0 && manual_only == 0 && latched == 1 && due == 1
            ) {
                latched = 0;
                phase = 0;
            }
            // Mutant: a park that ignores the ladder's rule.
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
            // A capture refusal is never retried as the machine being busy.
            invariant RefusalNeverRetriesAsActivity: refusals == 0;
            // After a physical failure, an unlatched lane is a due (or superseded) one.
            invariant NoEarlyRelease:
                if failures == 1 && latched == 0 {
                    due == 1 || superseded == 1
                } else {
                    due <= 1
                };
            // A passing moment is never a verdict: its latch always has a deadline coming.
            invariant APassingFailureNeverConverges:
                if passing == 1 { converged == 0 } else { converged <= 1 };
            // The look at a newer release behind a converged latch clears it by
            // superseding, never by keeping it or retrying the activation.
            invariant ANewerReleaseClearsAConvergedLatch:
                if looked == 1 { superseded == 1 } else { superseded <= 1 };
            // …and what lands is the newer build, never the superseded activation.
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

/// Law 1 of the apply ladder (`docs/DESIGN-auto-apply-ladder-2026-09-21.md`, *The law*): a
/// verified staged build applies by itself within a bounded wall-clock time of being
/// armed, on any machine, including one that is never idle.
///
/// The property is `[]<>(landed \/ (converged /\ ~newer))`. The one rest short of landing
/// is Law 2's: a genuine physical failure whose schedule converged, with no newer release
/// staged. A newer release behind that latch leaves the goal, so the newer build must
/// land. It is `[]<>` rather than `<>` so a behaviour that reaches the rest and leaves it
/// must still land.
///
/// The environment is not fair: output, keystrokes, focus, the warm-up's start, the desk
/// refusing or changing, a park missing its freeze budget, physical and passing failures,
/// the bundle swap, the convergence verdict and a newer release may each happen or not,
/// forever, and every state may stutter. What the verdict assumes:
/// * `Advance`, weakly fair — the wall clock moves while the Mac is awake;
/// * `WarmupEnds`, weakly fair — the consent warm-up hold is capped
///   (`[privacy] warmup_hold_ms`), as is the restored agents' hold
///   (`harness_host::RESTORED_HOLD`);
/// * `Due`, weakly fair — a latch's `retry_at` comes;
/// * `Lapse`, weakly fair — the lane acts on a due latch, and nothing disables it;
/// * `SupersedeRetryDue`, weakly fair — a refused retire's retry deadline comes;
/// * `Park`, `CaptureRefused` and `Supersede`, strongly fair — the lane re-reads its gate
///   at every instant and offers a refused release again at every retry deadline. This
///   also assumes a park does not miss its freeze budget (`ParkMissed`), and a retire is
///   not refused (`SupersedeRefused`), on every attempt forever; a Mac that never freezes
///   for a quarter second, or a disk or network that never lets the retire through, is
///   retried without end, each refusal said in `update status`.
///
/// Mutants, each alone on the healthy lane: `StandDown` and `ParkMissLatches` (wedges the
/// deadlock check also sees); `CaptureRefusedAsActivity`, `NewerReleaseIgnored` and
/// `RefusedRetireSpendsTheRelease` (livelocks only this property sees). Not
/// `PassingFailureConverges`: its converged latch is the goal's rest, so
/// `APassingFailureNeverConverges` carries that catch.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::native_update_apply_ladder_model)]
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

/// Automatic-update quiet admission after hidden PTY output.
///
/// A background tab can consume output and handle its wake without presenting, so its
/// first-edge presentation-latency sample stays armed. Admission follows the latest-output
/// clock, not that acknowledgement, and activity retries are strictly future.
/// `AttemptOnlyAfterAgedQuiet` reads the clock, not the quiet flag.
///
/// `Buggy=1` mutants, each its own action: `QuietGatedOnPresentationSample` gates on the
/// permanent sample and derives its retry from the expired output deadline;
/// `QuietFromLastPresent` reads idleness off the last present, so a hidden tab streaming
/// output is computed quiet; `AckSampleOnHiddenWake`, the tempting wrong fix, closes the
/// armed sample with no present, freeing the gate by fabricating a latency reading.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_hidden_output_quiet_model() -> Model {
    crate::ty_model! {
        NativeUpdateHiddenOutputQuiet {
            const Buggy = 0;
            const QuietTicks = 2;
            // phase: 0 Idle, 1 HiddenOutputRead, 2 WaitingForQuiet,
            // 3 QuietAdmitted, 4 Attempting.
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
            // The present clock of a hidden tab is as old as its last present,
            // so it reads quiet while the output clock is still inside the epoch.
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

/// Identity discipline for retrying a failed updater process replacement.
///
/// Every authorized attempt gets a fresh nonce. A failure may re-arm only the currently
/// active nonce; a replay of the first failure after a retry is live is inert.
///
/// `Buggy=1` mutants, each its own action: `AcceptStaleAbort` lets that replay cancel the
/// newer authority; `RetryReusingFailedNonce` re-arms under the failed attempt's nonce,
/// which a late replay still matches; `AbortKeepingActiveIdentity` returns to retryable
/// without clearing the dead attempt's nonce.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_attempt_identity_model() -> Model {
    crate::ty_model! {
        NativeUpdateAttemptIdentity {
            const Buggy = 0;
            const MaxNonce = 2;
            const MaxAborts = 2;
            // phase: 0 Staged/Retryable, 1 Applying.
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
            // The replay of an old failure is inert: its nonce is not the live one.
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
///
/// A nonblocking send that sees a full FIFO retains one coalesced purpose; every dequeue
/// emits a drain edge that gives that latch another chance. A dispatch-time disconnect gets
/// one restart, then an explicit unavailable result: never a silently dropped request or an
/// event-loop hot loop. Worker death after an accepted message is out of scope. `filler`
/// abstracts the `try_send(Full)` boundary and does not count the worker's active item, so
/// the model claims nothing about exact FIFO occupancy or processing time.
///
/// Each `Buggy=1` mutant is its own action: `DropApplyLatchWhenFull` (Full is success,
/// nothing retained); `DrainWithoutRetryEdge`; `ParkIdleMaterializingProxy` (a proxy clone,
/// CFRunLoop wake and warning on every idle park); `ReportUnavailableKeepingLatch` (the
/// next turn delivers the same request again); and `RetryRestartEveryTurn` (the hot loop
/// the one-restart budget refuses).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_worker_queue_model() -> Model {
    crate::ty_model! {
        NativeUpdateWorkerQueue {
            const Buggy = 0;
            var connected = 1;
            var filler = 0;
            var queued_intent = 0;
            var pending = 0;
            var completion_ready = 0;
            var live_intent = 0;
            // purpose: 0 none, 1 stage/startup hint, 2 explicit apply control.
            var purpose = 0;
            var drain_edge = 0;
            var delivered = 0;
            var failed_explicitly = 0;
            var restarts = 0;
            // Side effects of an event-loop park with no retained intent; both stay zero
            // (on macOS a proxy clone installs and wakes a CFRunLoop source, and a warning
            // feeds log I/O back into the same idle turn).
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

/// Reconciliation of the updater's durable status ledger with the running caller and the
/// canonical `Ready` marker.
///
/// A status ledger is evidence about the build that wrote it, never authority to relabel
/// the running process: the returned `current_build` always comes from the caller. A
/// persisted "staged" sentence is not a stage: with no Ready marker the claim is removed,
/// and a mismatched writer build with no Ready marker is normalized even when its prose is
/// innocuous, so an overlapping old/new pair cannot publish each other's stale outcome.
///
/// `PickStatusInputs` is the environment for `reconcile_status_outcome(running_build,
/// checked_from_build, ready_present, persisted)`. Each `Buggy=1` mutant is its own
/// reducer: `ReconcileTrustingLedger` relabels the caller with the ledger's build;
/// `ReconcileOnMarkerPresence` keeps an absent-Ready staged claim; and the
/// over-corrections, each dropping one conjunct of
/// `!ready_present && (checked_from_build != running_build || claims_staged)`:
/// `NeutralizeDespiteReady` rewrites a Ready stage's outcome, `NeutralizeEveryAbsentReady`
/// rewrites an honest "up to date" or "network check failed".
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn native_update_status_reconciliation_model() -> Model {
    let mismatched_or_claimed = || {
        or_(
            neq(var("ledger_build"), var("running_build")),
            eq(var("persisted_staged_claim"), int(1)),
        )
    };
    let stale_without_ready = || and_(eq(var("ready_present"), int(0)), mismatched_or_claimed());
    let settled = || eq(var("phase"), int(2));
    let reconcile = |name: &'static str, buggy: bool, build: Expr, neutralize: Expr| {
        let guard = eq(var("phase"), int(1));
        Action {
            name,
            guard: Some(if buggy {
                and_(eq(cst("Buggy"), int(1)), guard)
            } else {
                guard
            }),
            updates: vec![
                Update {
                    var: "reported_build",
                    expr: build,
                },
                // A neutralized outcome is "running build N; no update is staged",
                // which claims no stage; a preserved one repeats the ledger's claim.
                Update {
                    var: "reported_staged_claim",
                    expr: if_(neutralize.clone(), int(0), var("persisted_staged_claim")),
                },
                Update {
                    var: "neutralized",
                    expr: if_(neutralize, int(1), int(0)),
                },
                Update {
                    var: "phase",
                    expr: int(2),
                },
            ],
        }
    };

    Model {
        name: "NativeUpdateStatusReconciliation",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "running_build",
                init: 1,
            },
            StateVar {
                name: "ledger_build",
                init: 1,
            },
            StateVar {
                name: "ready_present",
                init: 0,
            },
            StateVar {
                name: "persisted_staged_claim",
                init: 0,
            },
            StateVar {
                name: "reported_build",
                init: 0,
            },
            StateVar {
                name: "reported_staged_claim",
                init: 0,
            },
            StateVar {
                name: "neutralized",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickStatusInputs",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "running_build",
                        expr: in_range(int(1), int(2)),
                    },
                    Update {
                        var: "ledger_build",
                        expr: in_range(int(1), int(2)),
                    },
                    Update {
                        var: "ready_present",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "persisted_staged_claim",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            reconcile(
                "ReconcileStatus",
                false,
                var("running_build"),
                stale_without_ready(),
            ),
            reconcile(
                "ReconcileTrustingLedger",
                true,
                var("ledger_build"),
                Expr::Bool(false),
            ),
            reconcile(
                "ReconcileOnMarkerPresence",
                true,
                var("running_build"),
                Expr::Bool(false),
            ),
            reconcile(
                "NeutralizeDespiteReady",
                true,
                var("running_build"),
                mismatched_or_claimed(),
            ),
            reconcile(
                "NeutralizeEveryAbsentReady",
                true,
                var("running_build"),
                eq(var("ready_present"), int(0)),
            ),
        ],
        invariants: vec![
            Invariant {
                name: "CallerBuildIsAuthoritative",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("reported_build"), var("running_build")),
                    eq(var("reported_build"), int(0)),
                ),
            },
            Invariant {
                name: "AbsentReadyCannotAdvertiseStage",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(settled(), eq(var("ready_present"), int(0))),
                    eq(var("reported_staged_claim"), int(0)),
                    le(var("reported_staged_claim"), int(1)),
                ),
            },
            Invariant {
                name: "MismatchedAbsentReadyIsNeutralized",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(settled(), stale_without_ready()),
                    eq(var("neutralized"), int(1)),
                    le(var("neutralized"), int(1)),
                ),
            },
            Invariant {
                name: "ReadyPreservesPersistedOutcome",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(settled(), eq(var("ready_present"), int(1))),
                    and_(
                        eq(var("reported_staged_claim"), var("persisted_staged_claim")),
                        eq(var("neutralized"), int(0)),
                    ),
                    le(var("reported_staged_claim"), int(1)),
                ),
            },
            Invariant {
                name: "HonestTerminalOutcomeIsPreserved",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(
                        and_(settled(), eq(var("ready_present"), int(0))),
                        and_(
                            eq(var("ledger_build"), var("running_build")),
                            eq(var("persisted_staged_claim"), int(0)),
                        ),
                    ),
                    eq(var("neutralized"), int(0)),
                    le(var("neutralized"), int(1)),
                ),
            },
        ],
    }
}

/// The writer/reader contract on the updater's failed-artifact memo (`FailedMark`): what a
/// verdict writer persists must mean the same to the `suppresses` reader at every future
/// probe time. A writer recording `retry_after = 0` for "never retry" against a reader
/// that takes a zero deadline as "already elapsed" re-applies a build that just
/// crash-looped.
///
/// `probe_now ∈ 0..3` are equivalence classes of the probe time relative to the recorded
/// deadline. The quarantine writer keeps the legacy `retry_after = 0` sentinel (class 0)
/// and carries its meaning in the `quarantined` field; a stage failure records an interior
/// deadline (class 2) so classes exist before, at and after it.
///
/// * `RecordQuarantine` / `QuarantineSuppressesAtEveryProbe`: after the quarantine writer,
///   `suppresses` holds at every probe class; the verdict never lapses.
/// * `RecordStageFailure` / `BackoffSuppressesIffBeforeDeadline`: with deadline D,
///   `suppresses` holds iff `now < D`. D comes from a verdict's schedule or, for a staging
///   moment, the gentler one (a first moment's D is its own `now`); both are the same timed
///   memo, and the Tier-1 sweep drives both schedules.
/// * `QuarantineVerdictLandsInTheQuarantineField` is the writer half alone.
///
/// `Buggy=1`: `RecordQuarantine` writes the verdict as a plain timed memo (the writer half
/// and `QuarantineSuppressesAtEveryProbe` fail), and `ProbeSuppresses` flips the deadline
/// comparison for stage-failure memos only (`BackoffSuppressesIffBeforeDeadline` fails),
/// so neither masks the other.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
            // 1 = quarantine verdict, 2 = download/stage failure backoff.
            StateVar {
                name: "writer_kind",
                init: 1,
            },
            // Timeline class of the probed `now` (0 smallest .. 3 past-deadline).
            StateVar {
                name: "probe_now",
                init: 0,
            },
            // Whether the probed (build, sha256) equals the recorded identity.
            StateVar {
                name: "candidate_matches",
                init: 0,
            },
            StateVar {
                name: "quarantined",
                init: 0,
            },
            // Timeline class of the recorded `retry_after` (0 = the legacy
            // "no deadline" sentinel, 2 = an interior backoff deadline).
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
                        // Buggy: the verdict as a plain timed memo (`retry_after = 0`, no
                        // field), which the reader may read as "already elapsed".
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
                                        // Buggy: the comparison flipped (suppress from
                                        // the deadline, not until it), scoped to the
                                        // stage-failure runs so it masks nothing.
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

/// Per-session ownership discipline of the seamless update handoff, over a two-session
/// pool.
///
/// `native_update_overlap_handoff_model` models the interleaving (the atomic arbiter,
/// kill/reap ordering, the legacy bridge) but counts readers globally and never says which
/// process owns which PTY. "Seamless" promises something about a session: a shell a
/// person is working in never belongs to nobody. So every invariant here is per session.
/// Two sessions is the smallest pool in which a partial descriptor transfer (which
/// orphans a session while the attempt looks healthy) differs from a complete one.
///
/// The linearization point is `CommitAtomically`, which moves ownership and exits the
/// outgoing process in one step, as `seamless::commit_and_exit` does: one atomic
/// `<=PIPE_BUF` write and `_exit(0)`, with no return on success. Splitting it would invent
/// an interval, absent from the code, in which a live session is owned by a dead process.
///
/// `App::start_unix_update_handoff` hands the candidate `F_DUPFD_CLOEXEC` duplicates and
/// holds the originals for the whole attempt (`_owned_masters`); that is what makes
/// rollback possible, so a descriptor move is one of the mutants.
///
/// `Buggy=1` admits, each catchable alone: a descriptor move; an adoption proof matched
/// over a partial descriptor set; a Commit with no proof; a second Commit; an `_exit` that
/// transfers no ownership; a candidate that unparks its readers before Commit; and a
/// rollback that resumes parked readers with no termination warrant. The last two together
/// are the two-readers-on-one-master interleaving (`NeverTwoReadersOnOneMaster`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_seamless_handoff_ownership_model() -> Model {
    crate::ty_model! {
        NativeUpdateSeamlessHandoffOwnership {
            const Buggy = 0;
            // phase: 0 Idle, 1 Parked, 2 Captured, 3 CandidateStarted,
            // 4 DescriptorsTransferred, 5 ProofMatched, 6 Committed,
            // 7 Resumed, 8 RolledBack.
            var phase = 0;
            // The outgoing process: alive, reading, and holding each master.
            var out_live = 1;
            var out_readers = 1;
            var out_holds_a = 1;
            var out_holds_b = 1;
            // The candidate: readerless until Commit releases its prebuilt gate.
            var cand_live = 0;
            var cand_readers = 0;
            var cand_holds_a = 0;
            var cand_holds_b = 0;
            // owner: 0 nobody (an orphaned session, which this model proves
            // unreachable), 1 the outgoing process, 2 the candidate.
            var owner_a = 1;
            var owner_b = 1;
            var captured = 0;
            var proof_matched = 0;
            // Any mutable pre-Commit admission fact that turned false (session set or
            // layout changed, activity epoch moved, native safety revoked, PTY peer dead,
            // Commit channel lost, resident operator busy or unfenced). They differ in
            // what they log and whether automatic mode retries, not in ownership.
            var revoked = 0;
            // The rollback warrant: the candidate is provably terminated, or was never
            // given anything. Nothing resumes a parked reader without it.
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
            // A warrant already licensed a rollback, so this attempt is over: it must not
            // start a candidate it would then have to prove dead again.
            action StartReaderlessCandidate when (
                phase == 2 && cand_live == 0 && revoked == 0 && warrant == 0
            ) {
                phase = 3;
                cand_live = 1;
            }
            // Duplicate, never move: the outgoing process keeps its own masters.
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
            // One step: ownership moves and the outgoing process is gone. Owner follows
            // who holds the master, so a session the candidate never received becomes
            // owner 0, an orphan, rather than being assumed to have travelled.
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
                out_holds_a = 0;
                out_holds_b = 0;
            }
            action CandidateReleasesReaderGate when (
                phase == 6 && commits == 1 && cand_live == 1 && cand_readers == 0
            ) {
                cand_readers = 1;
            }
            // Nothing left this process, so there is no candidate to prove anything
            // about (`HandoffRollbackWarrant::NoCandidate` / `NeverTransferred`).
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

            // Every session is owned by exactly one live process in every reachable state.
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
            // …and that owner still holds the descriptor, so it can serve the session.
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
            // Two readers on one master silently interleave and destroy the stream.
            // Stated per master: both processes holding a duplicate of one open file
            // description is what makes the overlap window dangerous.
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
            // Rollback is available from every pre-Commit state: the outgoing process is
            // alive, owns and holds every session, its readers are parked, and nothing
            // else reads. That `ResumeParkedReaders` is reachable from each such state is
            // checked in the Tier-1 bind; no state predicate can say it.
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
            uncaught ExitedProcessReadsNothing:
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

/// What a structural convergence of the automatic apply lane may still do: the latch the
/// lane took on build N's bytes after two handoffs the bytes answered for.
///
/// A launched-lane candidate installs its bundle before it dials, so a structural failure
/// lands on N's installed activation. The activation outranks every newer download while
/// the bundle is newer than the running process (a swap from the running image has no
/// rollback source, and N's unconfirmed boot trial owns the rollback path), so a latch
/// with no way out would hold every later release for a person. A newer release instead
/// takes N's place: the running build takes the install path back, N's trial is disarmed
/// at its count, and the newer build applies as an ordinary stage. N is never launched for
/// it (its successor would refuse to boot-apply past itself).
///
/// `Decide` is the shipping `structural_latch` decision, taken at every look after
/// something changed:
/// * `NewerArrives` — a verified download strictly newer than N is on disk and not yet
///   offered the install path. `Decide` starts the retire (`superseding`) whatever the
///   trial's `room`; it lands (`Supersede`) or is refused (`SupersedeRefused`: a
///   verification budget out on a captive portal, the apply lock held, a stage being
///   replaced). A refusal does not spend the release: it is offered again once its retry
///   deadline passes (`backoff`, cleared by `RetryDue`), and while it waits it still
///   outranks the day, though a promised re-sample the trial cannot afford is withdrawn.
/// * `DayPasses` — the one re-sample, 24 h after convergence, is due. It is released only
///   when one more launch of N keeps its boot trial under the revert threshold (`room`,
///   picked once by `TrialHasRoom`/`TrialIsSpent` from the sentinel's count): a structural
///   `ChildDied` keeps its counted launch, so after two the next launch of N is the one
///   `check_boot_health` reverts on, and an automatic attempt must never spend it. Without
///   room the re-sample is held and said (`said`). Convergence promises the re-sample
///   before any count is read, and a first count that rules it out withdraws it, said.
/// * `TrialUnreadable` — the installed facts stop reporting N (a channel floor above N
///   yanks it), so `room` is no reading (`TrialReadAgain` undoes it). A due look asks for
///   another observation and counts (`looks`); at `UnreadLooks` the re-sample is withdrawn
///   and the hold said, never promised every 75 s forever. The count is the host's
///   (`AutoApplyStructuralVerdict::unmeasured_looks`); `structural_latch` answers
///   `Unmeasured`.
/// * `TrialUnproven` — the latest read could not prove the installed bundle (`spctl` past
///   its ceiling on a captive portal), so `room` is no reading either (`unread = 2`). It is
///   no evidence the count is gone, so it spends none of `looks`; it counts `doubts`, and
///   at `UnprovenLooks` the re-sample moves to the next day (`day` cleared, still `owed`):
///   retried, never re-promised every 75 s. The move is recorded, not raised as a hold.
///
/// `AttemptFails` re-latches at once with no confirming retry. `ArmSameBuild` is an
/// artifact of build N (the same bytes, or a re-publish under another digest) offered while
/// the latch holds: it stays latched.
///
/// `Buggy=1` reproduces, each catchable alone on disjoint runs:
/// * newer release looked at before the day: with room, releasing the latch for one more
///   attempt at N (`NeverBootsTheOlderActivation`); without room, holding and spending the
///   release (`ANewerReleaseClearsTheLatch`);
/// * newer release looked at after the day: a refusal that spends the release and
///   schedules no retry (`ANewerReleaseClearsTheLatch`);
/// * no newer release: a latch with no deadline (`ResampleAfterADay`), or a trial-blind
///   release that spends the reverting launch in silence (`NeverSpendsTheRevertingLaunch`,
///   `ATrialHoldIsSaid`) and leaves the first count's promise standing
///   (`NoRetryPromisedPastTheTrial`);
/// * a verdict-less convergence buying a fresh re-sample per failure (`ResampleAtMostOnce`);
/// * a digest-keyed latch a re-publish escapes (`SameBuildStaysLatched`,
///   `ReleasedOnlyByAnEvent`);
/// * an unreadable count whose bound keeps the re-sample owed (`NoRetryPromisedPastTheBound`);
/// * an unproven bundle whose bound keeps the day due (`UnprovenLooksAreBounded`).
///
/// Liveness (a refused release is in the end retired over) is checked on the apply ladder
/// ([`native_update_apply_ladder_liveness`], mutant `RefusedRetireSpendsTheRelease`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_structural_latch_model() -> Model {
    crate::ty_model! {
        NativeUpdateStructuralLatch {
            const Buggy = 0;
            // How many due looks in a row may find the count unreadable before the lane
            // stops waiting for it (`STRUCTURAL_TRIAL_UNMEASURED_LOOKS`, asserted equal in
            // the bind).
            const UnreadLooks = 3;
            // How many due looks in a row may find the installed bundle unproven before
            // the re-sample moves to the next day (`STRUCTURAL_TRIAL_UNPROVEN_LOOKS`,
            // asserted equal in the bind).
            const UnprovenLooks = 3;
            // 0 = the boot trial's room is not yet picked, 1 = running.
            var phase = 0;
            // One more launch of N keeps its boot trial under the revert
            // threshold (the sentinel's count, measured).
            var room = 0;
            // The count cannot be read: 1 = the installed facts stopped reporting N (a
            // channel floor above it yanked it); 2 = the latest read could not prove the
            // installed bundle. `room` is then no reading.
            var unread = 0;
            // Due looks in a row that found the count unreadable (`unread == 1`),
            // each asking for an observation to read it.
            var looks = 0;
            // Due looks in a row over an unproven read (`unread == 2`), each
            // asking for an observation that might prove it.
            var doubts = 0;
            // The structural latch on build N holds.
            var latched = 1;
            // An observation the lane has not yet decided on.
            var pending = 0;
            // The one re-sample's deadline, 24 h after convergence, passed.
            var day = 0;
            // The one re-sample is still owed.
            var owed = 1;
            // The one re-sample was spent (taken, or held for the trial).
            var spent = 0;
            // A verified download strictly newer than N is on disk.
            var newer = 0;
            // That newer download is offered the install path (a retire of N
            // for it is running, or landed) — not merely refused once.
            var newer_spent = 0;
            // A hold was said (log, `update status`, notice).
            var said = 0;
            // An artifact of build N went through while the latch held.
            var escaped = 0;
            // The retire of N's activation for the newer release is running.
            var superseding = 0;
            // …and landed: the running build is back at the install path, N's
            // trial disarmed, the newer build the lane's stage.
            var superseded = 0;
            // …or was refused at least once, and said.
            var refused = 0;
            // A refused retire's release waits for its retry deadline.
            var backoff = 0;
            // N was launched for an attempt while a newer release waited.
            var booted_old = 0;
            // The first count read after the attempt that converged: a look like any other.
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
            // The installed facts stop reporting N — an observation, so a look.
            action TrialUnreadable when (phase == 1 && (unread == 0 || unread == 2) &&
                superseded == 0) {
                unread = 1;
                pending = 1;
            }
            // A read that could not prove the installed bundle at all — still a
            // look, but no observation of N.
            action TrialUnproven when (phase == 1 && unread <= 1 && superseded == 0) {
                unread = 2;
                pending = 1;
            }
            // …and report it again: the count is read once more.
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
            // A newer release is on offer at a look when it is on disk, not yet offered,
            // and not waiting out a refusal's retry deadline; either way it outranks the
            // day (`newer == 0 || newer_spent == 1` is "nothing newer is coming"). `room`
            // is a reading only while `unread == 0`; a due look with no reading asks for
            // another observation (`pending` stays set) and counts, and at `UnreadLooks`
            // the re-sample is withdrawn and the hold said.
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
                // Every look without room says so while there is something to say (the
                // promised re-sample withdrawn, a due one held, the newer release held);
                // so does a count that stayed unreadable to the bound.
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
                // Counted while the re-sample is due, nothing newer is coming and no count
                // can be read; kept by a look that is not due; cleared by a reading and by
                // the bound's decision. `Buggy` clears it at the bound and keeps the
                // re-sample owed, asking for nothing.
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
                // An unproven read's own count: kept by a look that is not due, cleared by
                // any proven read; at the bound the re-sample moves to the next day, still
                // owed. `Buggy` keeps the day due at the bound and asks again.
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
            // The retire landed: N's latch goes with N's bytes, and N is never
            // launched for it.
            action Supersede when (phase == 1 && superseding == 1) {
                superseding = 0;
                superseded = 1;
                latched = 0;
            }
            // The retire was refused and moved nothing: the latch stands, the refusal is
            // said, and the release is offered again once its retry deadline passes.
            action SupersedeRefused when (phase == 1 && superseding == 1) {
                superseding = 0;
                refused = 1;
                said = 1;
                newer_spent = if Buggy == 1 { newer_spent } else { 0 };
                backoff = if Buggy == 1 { backoff } else { 1 };
                pending = 1;
            }
            // A refused release's retry deadline passes: the next look offers
            // it again.
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
            // Once the lane has looked, a latch still holding over a newer release is
            // retiring N or waiting out a refusal's retry deadline, whatever the trial.
            invariant ANewerReleaseClearsTheLatch:
                if pending == 0 && latched == 1 && newer == 1 {
                    superseding == 1 || backoff == 1
                } else {
                    refused <= 1
                };
            // N is never launched for a release that could take its place.
            invariant NeverBootsTheOlderActivation: booted_old == 0;
            // Past the deadline, a look spends the re-sample (unless a newer
            // release is taking N's place, or waiting to, which moots it).
            invariant ResampleAfterADay:
                if pending == 0 && latched == 1 && room == 1 && day == 1 &&
                    superseding == 0 && backoff == 0 {
                    owed == 0
                } else {
                    owed <= 1
                };
            // An earned attempt the trial cannot afford is said, not dropped.
            invariant ATrialHoldIsSaid:
                if pending == 0 && phase == 1 && unread == 0 && room == 0 && superseding == 0 &&
                    superseded == 0 && (day == 1 || newer == 1) {
                    said == 1
                } else {
                    said <= 1
                };
            // Once a count has been looked at, no retry is promised that the
            // trial cannot afford.
            invariant NoRetryPromisedPastTheTrial:
                if pending == 0 && phase == 1 && unread == 0 && room == 0 &&
                    superseding == 0 && superseded == 0 {
                    owed == 0
                } else {
                    owed <= 1
                };
            // A count that cannot be read is decided within the bound: once the lane has
            // looked with the day due and nothing newer coming, no re-sample is promised
            // that no reading will earn; it is held, and said.
            invariant NoRetryPromisedPastTheBound:
                if pending == 0 && latched == 1 && unread == 1 && day == 1 &&
                    (newer == 0 || newer_spent == 1) && superseding == 0 {
                    owed == 0
                } else {
                    owed <= 1
                };
            // At `UnprovenLooks` due looks over unproven reads the re-sample moves to the
            // next day: never re-promised past the bound, never dropped.
            invariant UnprovenLooksAreBounded: doubts <= UnprovenLooks;
            // No automatic attempt spends the launch the boot trial reverts on:
            // without room the latch holds, or N is retired without a launch.
            invariant NeverSpendsTheRevertingLaunch:
                if room == 0 { latched == 1 || superseded == 1 } else { latched <= 1 };
            // One re-sample: once spent it is never owed again.
            invariant ResampleAtMostOnce:
                if spent == 1 { owed == 0 } else { owed <= 1 };
            // The latch covers build N, whatever the digest.
            invariant SameBuildStaysLatched: escaped == 0;
            // A release has a reason: the day, or a newer release.
            invariant ReleasedOnlyByAnEvent:
                if latched == 0 { day == 1 || newer == 1 } else { latched == 1 };
        }
    }
}
