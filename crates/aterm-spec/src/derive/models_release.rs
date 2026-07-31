// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Release / updater channel state-machine models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// The process-global updater is single-flight and generation-stamped. Only a
/// current verified artifact may become Staged, and Apply additionally requires
/// close preflight. Install-on-clean-quit is a policy bit over the same apply
/// transition, never a second application path. Mutants stage a stale completion
/// or re-exec twice for one accepted artifact.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_updater_model() -> Model {
    crate::ty_model! {
        NativeUpdater {
            const Buggy = 0;
            const MaxGeneration = 3;
            // phase: 0 Idle, 1 Checking, 2 Available, 3 Downloading,
            // 4 Staged, 5 Applying, 6 Failed.
            var phase = 0;
            var request_generation = 0;
            var work_generation = 0;
            var artifact_generation = 0;
            var active_work = 0;
            var stale_completion_pending = 0;
            var verified = 0;
            var close_preflight = 0;
            var install_on_clean_quit = 0;
            var reexec_count = 0;
            var stale_staged = 0;
            action StartCheck when (
                phase == 0 && request_generation <= MaxGeneration - 1
            ) {
                phase = 1;
                request_generation = request_generation + 1;
                active_work = 1;
            }
            action RetryCheck when (
                phase == 6 && request_generation <= MaxGeneration - 1
            ) {
                phase = 1;
                request_generation = request_generation + 1;
                active_work = 1;
            }
            action CheckAvailable when (phase == 1 && active_work == 1) {
                phase = 2;
                active_work = 0;
            }
            action CheckUpToDate when (phase == 1 && active_work == 1) {
                phase = 0;
                active_work = 0;
            }
            action CheckFailed when (phase == 1 && active_work == 1) {
                phase = 6;
                active_work = 0;
            }
            action StartDownload when (phase == 2 && active_work == 0) {
                phase = 3;
                work_generation = request_generation;
                active_work = 1;
            }
            action SupersedeDownload when (
                phase == 3 && request_generation <= MaxGeneration - 1
            ) {
                phase = 1;
                request_generation = request_generation + 1;
                active_work = 1;
                stale_completion_pending = 1;
            }
            action DropStaleDownload when (
                stale_completion_pending == 1 &&
                request_generation > work_generation
            ) {
                phase = if Buggy == 1 { 4 } else { phase };
                artifact_generation = if Buggy == 1 {
                    work_generation
                } else {
                    artifact_generation
                };
                active_work = if Buggy == 1 { 0 } else { active_work };
                stale_completion_pending = 0;
                verified = if Buggy == 1 { 1 } else { verified };
                stale_staged = if Buggy == 1 { 1 } else { stale_staged };
            }
            action CompleteDownload when (
                phase == 3 && active_work == 1 &&
                work_generation == request_generation
            ) {
                phase = 4;
                artifact_generation = work_generation;
                active_work = 0;
                verified = 1;
            }
            action MarkCloseReady when (phase == 4) {
                close_preflight = 1;
            }
            action InstallOnCleanQuit when (phase == 4) {
                install_on_clean_quit = 1;
            }
            action Apply when (
                phase == if reexec_count == 0 { 4 } else { 5 } &&
                reexec_count <= if Buggy == 1 { 1 } else { 0 } &&
                verified == 1 &&
                artifact_generation == request_generation &&
                close_preflight == 1
            ) {
                phase = 5;
                reexec_count = reexec_count + 1;
            }
            action AbortApply when (
                phase == 5 && verified == 1 &&
                artifact_generation == request_generation &&
                close_preflight == 1 && reexec_count == 1
            ) {
                phase = 4;
                close_preflight = 0;
                install_on_clean_quit = 0;
                reexec_count = 0;
            }
            invariant SingleFlight: active_work <= 1;
            invariant CurrentStagedArtifact:
                if phase == 4 {
                    verified == 1 && artifact_generation == request_generation &&
                    stale_staged == 0
                } else {
                    request_generation <= MaxGeneration
                };
            invariant SafeApply:
                if phase == 5 {
                    verified == 1 && artifact_generation == request_generation &&
                    close_preflight == 1 && reexec_count == 1
                } else {
                    request_generation <= MaxGeneration
                };
            invariant OneLiveApplyAuthority: reexec_count <= 1;
            invariant GenerationBounded: request_generation <= MaxGeneration;
            invariant QuitPolicyDoesNotApply: install_on_clean_quit <= 1;
        }
    }
}

/// Durable one-shot authority for GitHub draft-create and asset-upload POSTs.
///
/// Each non-idempotent request is preceded by an atomically persisted intent.
/// A crash may occur before the POST or after a landed POST whose response was
/// lost. In both cases a resumed process with an issued intent is forbidden from
/// posting again; it can only wait for, then converge through, the exact visible
/// object. Asset upload starts only after the immutable draft object converges.
///
/// `Buggy=1` weakens both the persist-before-POST guard and the one-shot bound,
/// reproducing an unjournaled request or a duplicate retry after crash/resume.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_durable_post_intent_model() -> Model {
    crate::ty_model! {
        ReleaseDurablePostIntent {
            const Buggy = 0;
            const MaxPosts = 2;
            const MaxCrashes = 2;
            var attached = 1;
            var create_intent = 0;
            var create_post_authority = 0;
            var create_permit_lost = 0;
            var create_posts = 0;
            var create_visible = 0;
            var create_converged = 0;
            var upload_intent = 0;
            var upload_post_authority = 0;
            var upload_permit_lost = 0;
            var upload_posts = 0;
            var upload_visible = 0;
            var upload_converged = 0;
            var crashes = 0;

            action PersistCreateIntent when (
                attached == 1 && create_intent == 0 && create_visible == 0
            ) {
                create_intent = 1;
                create_post_authority = 1;
            }
            action IssueCreatePost when (
                attached == 1 && create_visible == 0 &&
                create_posts <= if Buggy == 1 { MaxPosts - 1 } else { 0 } &&
                create_post_authority + Buggy > 0
            ) {
                create_posts = create_posts + 1;
                create_post_authority = 0;
            }
            action RevealCreatedDraft when (
                create_posts > 0 && create_visible == 0
            ) {
                create_visible = 1;
            }
            action ConvergeCreatedDraft when (
                attached == 1 && create_visible == 1 && create_converged == 0
            ) {
                create_converged = 1;
            }
            action PersistUploadIntent when (
                attached == 1 && create_converged == 1 &&
                upload_intent == 0 && upload_visible == 0
            ) {
                upload_intent = 1;
                upload_post_authority = 1;
            }
            action IssueUploadPost when (
                attached == 1 && create_converged == 1 && upload_visible == 0 &&
                upload_posts <= if Buggy == 1 { MaxPosts - 1 } else { 0 } &&
                upload_post_authority + Buggy > 0
            ) {
                upload_posts = upload_posts + 1;
                upload_post_authority = 0;
            }
            action RevealUploadedAsset when (
                upload_posts > 0 && upload_visible == 0
            ) {
                upload_visible = 1;
            }
            action ConvergeUploadedAsset when (
                attached == 1 && upload_visible == 1 && upload_converged == 0
            ) {
                upload_converged = 1;
            }
            action Crash when (
                attached == 1 && create_intent + upload_intent > 0 &&
                crashes <= MaxCrashes - 1
            ) {
                attached = 0;
                create_permit_lost = if create_post_authority == 1 {
                    1
                } else {
                    create_permit_lost
                };
                upload_permit_lost = if upload_post_authority == 1 {
                    1
                } else {
                    upload_permit_lost
                };
                create_post_authority = 0;
                upload_post_authority = 0;
                crashes = crashes + 1;
            }
            action Resume when (attached == 0) {
                attached = 1;
            }

            invariant CreatePostRequiresDurableIntent:
                if create_posts > 0 { create_intent == 1 } else { create_intent <= 1 };
            invariant CreateAuthorityIsDurableAndProcessLocal:
                if create_post_authority == 1 {
                    create_intent == 1 && attached == 1 && create_posts == 0
                } else {
                    create_post_authority == 0
                };
            invariant CreatePostIsOneShot: create_posts <= 1;
            invariant LostCreatePermitCannotPost:
                if create_permit_lost == 1 { create_posts == 0 } else { create_posts <= 1 };
            invariant CreateConvergenceRequiresVisibility:
                if create_converged == 1 { create_visible == 1 } else { create_visible <= 1 };
            invariant UploadPostRequiresDurableIntent:
                if upload_posts > 0 { upload_intent == 1 } else { upload_intent <= 1 };
            invariant UploadAuthorityIsDurableAndProcessLocal:
                if upload_post_authority == 1 {
                    upload_intent == 1 && attached == 1 && upload_posts == 0
                } else {
                    upload_post_authority == 0
                };
            invariant UploadPostIsOneShot: upload_posts <= 1;
            invariant LostUploadPermitCannotPost:
                if upload_permit_lost == 1 { upload_posts == 0 } else { upload_posts <= 1 };
            invariant UploadRequiresConvergedDraft:
                if upload_intent + upload_posts + upload_visible + upload_converged > 0 {
                    create_converged == 1
                } else {
                    create_converged <= 1
                };
            invariant UploadConvergenceRequiresVisibility:
                if upload_converged == 1 { upload_visible == 1 } else { upload_visible <= 1 };
            invariant DurableIntentStateBounded:
                attached <= 1 && create_intent <= 1 && create_post_authority <= 1 &&
                create_permit_lost <= 1 &&
                create_posts <= MaxPosts && create_visible <= 1 &&
                create_converged <= 1 && upload_intent <= 1 && upload_post_authority <= 1 &&
                upload_permit_lost <= 1 &&
                upload_posts <= MaxPosts && upload_visible <= 1 &&
                upload_converged <= 1 && crashes <= MaxCrashes;
        }
    }
}

/// Release-channel floor carry-forward and late-race policy.
///
/// A cut resolves its manifest floor as the canonical maximum of the operator
/// request and the newest live channel manifest, bounded by its claimed build. The
/// resolved value is frozen in the resume journal. Before visibility, the cutter
/// holds a cross-machine release lease while it re-reads the live channel and
/// publishes: a floor that advanced beyond the frozen value aborts the cut, while a
/// covered floor permits publication without a post-check race. The exact-commit
/// lease remains held after flip through archive, cask, and verify; only the final
/// journaled unlock releases it. This models the whole scan → freeze/crash/resume →
/// lease → revalidate → publish → archive/cask/verify → unlock lifecycle rather than
/// testing the arithmetic decisions in isolation.
///
/// `Buggy=1` enables four independent non-vacuity controls:
/// `ResolveOperatorOnly` drops the observed channel input (the retired
/// operator-only policy), `PublishUnchecked` skips late revalidation,
/// `BypassLeaseAdvance` lets the channel change after a covered verdict despite
/// lease ownership, and `UnlockBeforeVerification` drops the owner before
/// downstream release steps complete. Tier-1 binds the resolver, journal,
/// `PublishChecked`, exact-owner acquire/resume, and CAS unlock production seams.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_channel_floor_model() -> Model {
    crate::ty_model! {
        ReleaseChannelFloor {
            const Buggy = 0;
            const MaxFloor = 4;
            // phase: 0 Inputs, 1 Frozen, 2 Revalidated under lease,
            // 3 Published, 4 Aborted, 5 ResumePending, 6 Archived,
            // 7 CaskPinned, 8 Verified, 9 Completed/Unlocked.
            var phase = 0;
            var operator_floor = 0;
            var observed_floor = 0;
            var claimed_build = 0;
            var frozen_floor = 0;
            var journal_floor = 0;
            var latest_floor = 0;
            var late_checked = 0;
            var resumed = 0;
            var lease_owned = 0;
            var lease_bypassed = 0;
            var archive_done = 0;
            var cask_done = 0;
            var verify_done = 0;
            var unlock_bypassed = 0;
            var advanced_rejected = 0;
            var abandon_done = 0;

            // Input-spreading actions make every bounded resolver tuple reachable.
            action RaiseOperator when (
                phase == 0 && operator_floor <= MaxFloor - 1
            ) {
                operator_floor = operator_floor + 1;
            }
            action RaiseObserved when (
                phase == 0 && observed_floor <= MaxFloor - 1
            ) {
                observed_floor = observed_floor + 1;
                latest_floor = observed_floor + 1;
            }
            action RaiseClaim when (
                phase == 0 && claimed_build <= MaxFloor - 1
            ) {
                claimed_build = claimed_build + 1;
            }
            action Resolve when (
                phase == 0 && operator_floor <= claimed_build &&
                observed_floor <= claimed_build
            ) {
                phase = 1;
                frozen_floor = if operator_floor > observed_floor {
                    operator_floor
                } else {
                    observed_floor
                };
                journal_floor = if operator_floor > observed_floor {
                    operator_floor
                } else {
                    observed_floor
                };
            }
            action ResolveOperatorOnly when (
                Buggy == 1 && phase == 0 &&
                operator_floor <= claimed_build &&
                observed_floor <= claimed_build
            ) {
                phase = 1;
                frozen_floor = operator_floor;
                journal_floor = operator_floor;
            }
            action RejectOperatorAboveClaim when (
                phase == 0 && operator_floor > claimed_build
            ) {
                phase = 4;
            }
            action RejectObservedAboveClaim when (
                phase == 0 && observed_floor > claimed_build
            ) {
                phase = 4;
            }
            // A process crash loses the runtime copy but not the atomic journal.
            action CrashBeforeResume when (
                phase == 1 && resumed == 0 && lease_owned == 0
            ) {
                phase = 5;
                frozen_floor = 0;
            }
            // Resume reconstructs the runtime policy from real persisted state.
            action ResumeFrozen when (phase == 5) {
                phase = 1;
                frozen_floor = journal_floor;
                resumed = 1;
            }
            // Another publisher may raise the live floor before lease acquisition.
            action RaiseChannelFloor when (
                phase == 1 && lease_owned == 0 && latest_floor <= MaxFloor - 1
            ) {
                latest_floor = latest_floor + 1;
            }
            action AcquireLease when (phase == 1 && lease_owned == 0) {
                lease_owned = 1;
            }
            action ConfirmCovered when (
                phase == 1 && lease_owned == 1 && latest_floor <= frozen_floor
            ) {
                phase = 2;
                late_checked = 1;
            }
            action RejectAdvanced when (
                phase == 1 && lease_owned == 1 && latest_floor > frozen_floor
            ) {
                phase = 4;
                late_checked = 1;
                advanced_rejected = 1;
            }
            // A failed late guard leaves the persistent remote lease in place.
            // Only an explicit abandon/CAS cleanup may release that authority.
            action AbandonRejected when (
                phase == 4 && lease_owned == 1 && advanced_rejected == 1 &&
                abandon_done == 0
            ) {
                lease_owned = 0;
                abandon_done = 1;
            }
            action PublishChecked when (phase == 2 && lease_owned == 1) {
                phase = 3;
            }
            action PublishUnchecked when (Buggy == 1 && phase == 1) {
                phase = 3;
            }
            action ArchiveAfterPublish when (phase == 3 && lease_owned == 1) {
                phase = 6;
                archive_done = 1;
            }
            action PinCask when (
                phase == 6 && lease_owned == 1 && archive_done == 1
            ) {
                phase = 7;
                cask_done = 1;
            }
            action VerifyRelease when (
                phase == 7 && lease_owned == 1 && archive_done == 1 &&
                cask_done == 1
            ) {
                phase = 8;
                verify_done = 1;
            }
            action Unlock when (
                phase == 8 && lease_owned == 1 && archive_done == 1 &&
                cask_done == 1 && verify_done == 1
            ) {
                phase = 9;
                lease_owned = 0;
            }
            action UnlockBeforeVerification when (
                Buggy == 1 && phase == 3 && lease_owned == 1
            ) {
                phase = 9;
                lease_owned = 0;
                unlock_bypassed = 1;
            }
            // Regression control for a missing/non-shared lease: the channel moves
            // after a covered verdict but before visibility.
            action BypassLeaseAdvance when (
                Buggy == 1 && phase == 2 && lease_owned == 1 &&
                latest_floor <= MaxFloor - 1
            ) {
                latest_floor = latest_floor + 1;
                lease_bypassed = 1;
            }

            invariant FrozenCoversInitialInputs:
                if phase > 0 && phase <= 3 {
                    operator_floor <= frozen_floor &&
                    observed_floor <= frozen_floor
                } else if phase > 5 {
                    operator_floor <= frozen_floor &&
                    observed_floor <= frozen_floor
                } else {
                    phase <= 9
                };
            invariant FrozenFloorFitsClaim:
                if phase > 0 && phase <= 3 {
                    frozen_floor <= claimed_build
                } else if phase > 5 {
                    frozen_floor <= claimed_build
                } else {
                    phase <= 9
                };
            invariant RuntimeMatchesFrozenJournal:
                if phase > 0 && phase <= 3 {
                    frozen_floor == journal_floor
                } else if phase > 5 {
                    frozen_floor == journal_floor
                } else {
                    phase <= 9
                };
            invariant JournalSurvivesCrash:
                if phase == 5 {
                    operator_floor <= journal_floor &&
                    observed_floor <= journal_floor &&
                    journal_floor <= claimed_build && frozen_floor == 0
                } else {
                    phase <= 9
                };
            invariant PublishedNeverLowersLatest:
                if phase == 3 {
                    latest_floor <= frozen_floor
                } else if phase > 5 {
                    latest_floor <= frozen_floor
                } else {
                    phase <= 9
                };
            invariant PublishedRequiresLateGuard:
                if phase == 3 {
                    late_checked == 1
                } else if phase > 5 {
                    late_checked == 1
                } else {
                    late_checked <= 1
                };
            invariant RevalidatedOwnsLease:
                if phase == 2 { lease_owned == 1 } else { lease_owned <= 1 };
            invariant VisibleWorkOwnsLease:
                if phase == 3 {
                    lease_owned == 1
                } else if phase > 5 && phase <= 8 {
                    lease_owned == 1
                } else {
                    lease_owned <= 1
                };
            invariant CompletedReleasesLease:
                if phase == 9 { lease_owned == 0 } else { lease_owned <= 1 };
            invariant RejectionCannotSilentlyDropLease:
                if advanced_rejected == 1 && abandon_done == 0 {
                    phase == 4 && lease_owned == 1
                } else {
                    lease_owned <= 1
                };
            invariant AbandonIsExplicitAndTerminal:
                if abandon_done == 1 {
                    phase == 4 && advanced_rejected == 1 && lease_owned == 0
                } else {
                    abandon_done == 0
                };
            invariant CompletionRequiresPostPublishSteps:
                if phase == 9 {
                    archive_done == 1 && cask_done == 1 && verify_done == 1
                } else {
                    phase <= 9
                };
            invariant LeaseCannotBeBypassed: lease_bypassed == 0;
            invariant UnlockCannotBeBypassed: unlock_bypassed == 0;
            invariant FloorStateBounds:
                phase <= 9 && operator_floor <= MaxFloor &&
                observed_floor <= MaxFloor && claimed_build <= MaxFloor &&
                frozen_floor <= MaxFloor && journal_floor <= MaxFloor &&
                latest_floor <= MaxFloor && late_checked <= 1 && resumed <= 1 &&
                lease_owned <= 1 && lease_bypassed <= 1 && archive_done <= 1 &&
                cask_done <= 1 && verify_done <= 1 && unlock_bypassed <= 1 &&
                advanced_rejected <= 1 && abandon_done <= 1;
        }
    }
}

/// Canonical-prefix release journal and crash/resume ordering.
///
/// A current journal may admit only a known, unique, gap-free prefix of the
/// mutation pipeline under a canonical version and full claim identity. Resume
/// starts at the first incomplete step and advances in order; a crash drops only
/// the process-local attachment. Gapped membership must never let a later
/// pre-marked remote mutation (especially visibility/archive/unlock) be skipped.
///
/// Four abstract steps represent lock, previsibility preparation, visible-channel
/// convergence, and final verify/unlock. `Buggy=1` admits a gapped/unknown/duplicate
/// or bad-identity journal and can skip preparation after resume.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_journal_prefix_model() -> Model {
    crate::ty_model! {
        ReleaseJournalPrefix {
            const Buggy = 0;
            const MaxCursor = 4;
            // phase: 0 persisted input, 1 admitted/resumable, 2 complete.
            var phase = 0;
            var done_lock = 0;
            var done_prepare = 0;
            var done_visible = 0;
            var done_unlock = 0;
            var unknown_step = 0;
            var duplicate_step = 0;
            var version_valid = 1;
            var owner_valid = 1;
            var resume_cursor = 0;
            var attached = 0;
            var crashed = 0;
            var corruption_bypassed = 0;
            var ordering_bypassed = 0;

            // Input-spreading actions make every done-bit subset reachable.
            action InputLock when (phase == 0 && done_lock == 0) {
                done_lock = 1;
            }
            action InputPrepare when (phase == 0 && done_prepare == 0) {
                done_prepare = 1;
            }
            action InputVisible when (phase == 0 && done_visible == 0) {
                done_visible = 1;
            }
            action InputUnlock when (phase == 0 && done_unlock == 0) {
                done_unlock = 1;
            }
            action InputUnknown when (phase == 0 && unknown_step == 0) {
                unknown_step = 1;
            }
            action InputDuplicate when (phase == 0 && duplicate_step == 0) {
                duplicate_step = 1;
            }
            action InputBadVersion when (phase == 0 && version_valid == 1) {
                version_valid = 0;
            }
            action InputBadOwner when (phase == 0 && owner_valid == 1) {
                owner_valid = 0;
            }

            action AdmitEmptyPrefix when (
                phase == 0 && done_lock == 0 && done_prepare == 0 &&
                done_visible == 0 && done_unlock == 0 && unknown_step == 0 &&
                duplicate_step == 0 && version_valid == 1 && owner_valid == 1
            ) {
                phase = 1;
                resume_cursor = 0;
                attached = 1;
            }
            action AdmitLockPrefix when (
                phase == 0 && done_lock == 1 && done_prepare == 0 &&
                done_visible == 0 && done_unlock == 0 && unknown_step == 0 &&
                duplicate_step == 0 && version_valid == 1 && owner_valid == 1
            ) {
                phase = 1;
                resume_cursor = 1;
                attached = 1;
            }
            action AdmitPreparePrefix when (
                phase == 0 && done_lock == 1 && done_prepare == 1 &&
                done_visible == 0 && done_unlock == 0 && unknown_step == 0 &&
                duplicate_step == 0 && version_valid == 1 && owner_valid == 1
            ) {
                phase = 1;
                resume_cursor = 2;
                attached = 1;
            }
            action AdmitVisiblePrefix when (
                phase == 0 && done_lock == 1 && done_prepare == 1 &&
                done_visible == 1 && done_unlock == 0 && unknown_step == 0 &&
                duplicate_step == 0 && version_valid == 1 && owner_valid == 1
            ) {
                phase = 1;
                resume_cursor = 3;
                attached = 1;
            }
            action AdmitCompletePrefix when (
                phase == 0 && done_lock == 1 && done_prepare == 1 &&
                done_visible == 1 && done_unlock == 1 && unknown_step == 0 &&
                duplicate_step == 0 && version_valid == 1 && owner_valid == 1
            ) {
                phase = 2;
                resume_cursor = 4;
            }
            action RunLock when (
                phase == 1 && attached == 1 && resume_cursor == 0 &&
                done_lock == 0 && done_prepare == 0 && done_visible == 0 &&
                done_unlock == 0
            ) {
                done_lock = 1;
                resume_cursor = 1;
            }
            action RunPrepare when (
                phase == 1 && attached == 1 && resume_cursor == 1 &&
                done_lock == 1 && done_prepare == 0 && done_visible == 0 &&
                done_unlock == 0
            ) {
                done_prepare = 1;
                resume_cursor = 2;
            }
            action RunVisibleConvergence when (
                phase == 1 && attached == 1 && resume_cursor == 2 &&
                done_lock == 1 && done_prepare == 1 && done_visible == 0 &&
                done_unlock == 0
            ) {
                done_visible = 1;
                resume_cursor = 3;
            }
            action RunVerifyAndUnlock when (
                phase == 1 && attached == 1 && resume_cursor == 3 &&
                done_lock == 1 && done_prepare == 1 && done_visible == 1 &&
                done_unlock == 0
            ) {
                phase = 2;
                done_unlock = 1;
                resume_cursor = 4;
                attached = 0;
            }
            action CrashAfterAdmission when (
                phase == 1 && attached == 1 && crashed == 0
            ) {
                attached = 0;
                crashed = 1;
            }
            action ReattachCanonicalPrefix when (
                phase == 1 && attached == 0 && crashed == 1
            ) {
                attached = 1;
            }

            action AdmitGappedJournal when (
                Buggy == 1 && phase == 0 && done_lock == 1 &&
                done_prepare == 0 && done_visible == 1 && done_unlock == 0
            ) {
                phase = 1;
                resume_cursor = 1;
                attached = 1;
                corruption_bypassed = 1;
            }
            action AdmitUnknownJournal when (
                Buggy == 1 && phase == 0 && unknown_step == 1
            ) {
                phase = 1;
                attached = 1;
                corruption_bypassed = 1;
            }
            action AdmitDuplicateJournal when (
                Buggy == 1 && phase == 0 && duplicate_step == 1
            ) {
                phase = 1;
                attached = 1;
                corruption_bypassed = 1;
            }
            action AdmitBadIdentityJournal when (
                Buggy == 1 && phase == 0 && version_valid == 0 &&
                owner_valid == 0
            ) {
                phase = 1;
                attached = 1;
                corruption_bypassed = 1;
            }
            action SkipPreparationAfterResume when (
                Buggy == 1 && phase == 1 && attached == 1 &&
                done_lock == 1 && done_prepare == 0 && done_visible == 0 &&
                done_unlock == 0
            ) {
                done_visible = 1;
                resume_cursor = 3;
                ordering_bypassed = 1;
            }

            invariant AdmittedDoneIsCanonicalPrefix:
                if phase > 0 {
                    done_prepare <= done_lock && done_visible <= done_prepare &&
                    done_unlock <= done_visible && unknown_step == 0 &&
                    duplicate_step == 0 && version_valid == 1 && owner_valid == 1
                } else {
                    phase == 0
                };
            invariant CursorIsFirstIncomplete:
                if phase == 1 {
                    if done_lock == 0 {
                        resume_cursor == 0
                    } else if done_prepare == 0 {
                        resume_cursor == 1
                    } else if done_visible == 0 {
                        resume_cursor == 2
                    } else {
                        done_unlock == 0 && resume_cursor == 3
                    }
                } else if phase == 2 {
                    resume_cursor == 4
                } else {
                    resume_cursor == 0
                };
            invariant CompletionRequiresEveryStep:
                if phase == 2 {
                    done_lock == 1 && done_prepare == 1 &&
                    done_visible == 1 && done_unlock == 1
                } else {
                    phase <= 1
                };
            invariant CorruptJournalCannotResume: corruption_bypassed == 0;
            invariant ResumeCannotSkipOrderedMutation: ordering_bypassed == 0;
            invariant JournalPrefixBounds:
                phase <= 2 && done_lock <= 1 && done_prepare <= 1 &&
                done_visible <= 1 && done_unlock <= 1 && unknown_step <= 1 &&
                duplicate_step <= 1 && version_valid <= 1 && owner_valid <= 1 &&
                resume_cursor <= MaxCursor && attached <= 1 && crashed <= 1 &&
                corruption_bypassed <= 1 && ordering_bypassed <= 1;
        }
    }
}

/// Unique per-process publisher fencing layered over the persistent claim lease.
///
/// The lease owner identifies a recoverable release journal, so two simultaneous
/// resumes of the same commit share it. A separate annotated-tag token admits only
/// one live mutation session. Lost-machine recovery has an explicit operator
/// precondition that the old publisher was proved stopped, then rotates that token
/// by exact CAS. The model retains A's token data after `StopA` so it can prove that
/// a residual stale guard cannot later mutate, rotate, or delete B. It deliberately
/// does not claim that a Git ref rotation can cancel an external request already in
/// flight; the stopped-process precondition closes that cross-system TOCTOU window.
/// Ambiguous/malformed remote observations refuse authority. Final or ordinary
/// cleanup deletes only an exact observed token, so a successor created after an
/// uncertain response remains untouched.
///
/// `Buggy=1` exposes independent stale mutation, stale delete/rotation,
/// stopped-proof reuse, lease-loss mutation, and ambiguity bypass controls. Tier-1
/// (`publisher_fence_model.rs`) drives the real annotated ref, create/rotation CAS,
/// session assertion, stale exact-token cleanup, ordinary release, and atomic final
/// owner+token delete against a bare Git remote.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_publisher_fence_model() -> Model {
    crate::ty_model! {
        ReleasePublisherFence {
            const Buggy = 0;
            // Token 0=None, 1=session A, 2=session B. Both peel to claim owner 1.
            var remote_token = 0;
            var remote_fence_owner = 0;
            var local_a_token = 0;
            var local_b_token = 0;
            var old_process_stopped = 0;
            var lease_owner = 1;
            var ambiguous_remote = 0;
            var refused = 0;
            var a_mutated = 0;
            var b_mutated = 0;
            var b_lost_create = 0;
            var rotations = 0;
            var uncertain_delete = 0;
            var atomic_delete_uncertain = 0;
            var stale_release_observed = 0;
            var incoherent_remote = 0;
            var incoherent_accepted = 0;
            var stale_mutation_bypassed = 0;
            var stale_release_bypassed = 0;
            var stale_rotation_bypassed = 0;
            var ambiguous_bypassed = 0;
            var lease_bypassed = 0;
            var unsafe_rotation_bypassed = 0;
            var stale_stop_reused = 0;

            action ObserveAmbiguousRemote when (
                ambiguous_remote == 0 && refused == 0
            ) {
                ambiguous_remote = 1;
            }
            action RefuseAmbiguousRemote when (
                ambiguous_remote == 1 && refused == 0
            ) {
                refused = 1;
            }
            action AcquireA when (
                remote_token == 0 && ambiguous_remote == 0 &&
                local_a_token == 0 && lease_owner == 1
            ) {
                remote_token = 1;
                remote_fence_owner = 1;
                local_a_token = 1;
                // A stopped-process acknowledgement belongs to one concrete
                // publisher invocation. Re-entering A after an ordinary
                // release must obtain a fresh acknowledgement before recovery.
                old_process_stopped = 0;
            }
            action AcquireAReusingStoppedProof when (
                Buggy == 1 && remote_token == 0 && ambiguous_remote == 0 &&
                local_a_token == 0 && lease_owner == 1 &&
                old_process_stopped == 1
            ) {
                remote_token = 1;
                remote_fence_owner = 1;
                local_a_token = 1;
                stale_stop_reused = 1;
            }
            action AcquireB when (
                remote_token == 0 && ambiguous_remote == 0 &&
                local_b_token == 0 && lease_owner == 1
            ) {
                remote_token = 2;
                remote_fence_owner = 1;
                local_b_token = 2;
            }
            action LoseBCreateRace when (
                remote_token == 1 && local_b_token == 0 &&
                b_lost_create == 0
            ) {
                b_lost_create = 1;
            }
            action MutateA when (
                remote_token == 1 && local_a_token == 1 &&
                remote_fence_owner == 1 && lease_owner == 1 &&
                ambiguous_remote == 0 && a_mutated == 0 &&
                old_process_stopped == 0
            ) {
                a_mutated = 1;
            }
            action MutateB when (
                remote_token == 2 && local_b_token == 2 &&
                remote_fence_owner == lease_owner && lease_owner > 0 &&
                ambiguous_remote == 0 && b_mutated == 0
            ) {
                b_mutated = 1;
            }
            action StopA when (
                local_a_token == 1 && old_process_stopped == 0
            ) {
                old_process_stopped = 1;
            }
            // After the external stopped-process proof, exact-CAS recovery
            // installs B. A's residual token data becomes stale immediately.
            action RotateAtoB when (
                remote_token == 1 && local_a_token == 1 &&
                local_b_token == 0 && lease_owner == 1 &&
                ambiguous_remote == 0 && rotations == 0 &&
                old_process_stopped == 1
            ) {
                remote_token = 2;
                local_b_token = 2;
                rotations = 1;
            }
            action ReleaseA when (
                remote_token == 1 && local_a_token == 1 &&
                ambiguous_remote == 0
            ) {
                remote_token = 0;
                remote_fence_owner = 0;
                local_a_token = 0;
            }
            action ReleaseB when (
                remote_token == 2 && local_b_token == 2 &&
                ambiguous_remote == 0
            ) {
                remote_token = 0;
                remote_fence_owner = 0;
                local_b_token = 0;
            }
            action AtomicFinalDeleteA when (
                remote_token == 1 && remote_fence_owner == 1 &&
                local_a_token == 1 && lease_owner == 1 &&
                ambiguous_remote == 0
            ) {
                remote_token = 0;
                remote_fence_owner = 0;
                lease_owner = 0;
                local_a_token = 0;
            }
            action AtomicFinalDeleteB when (
                remote_token == 2 && remote_fence_owner == 1 &&
                local_b_token == 2 && lease_owner == 1 &&
                ambiguous_remote == 0
            ) {
                remote_token = 0;
                remote_fence_owner = 0;
                lease_owner = 0;
                local_b_token = 0;
            }
            // The delete landed but its response/journal mark was lost. A keeps
            // a stale local guard while the remote slot is legitimately free.
            action DeleteALandsResponseLost when (
                remote_token == 1 && local_a_token == 1 &&
                ambiguous_remote == 0 && uncertain_delete == 0
            ) {
                remote_token = 0;
                remote_fence_owner = 0;
                uncertain_delete = 1;
            }
            // Final atomic owner+fence deletion landed, but the response/journal
            // mark was lost. A coherent foreign successor may then acquire both.
            action AtomicFinalDeleteAResponseLost when (
                remote_token == 1 && remote_fence_owner == 1 &&
                local_a_token == 1 && lease_owner == 1 &&
                ambiguous_remote == 0 && atomic_delete_uncertain == 0
            ) {
                remote_token = 0;
                remote_fence_owner = 0;
                lease_owner = 0;
                atomic_delete_uncertain = 1;
            }
            action AcquireSuccessorB when (
                remote_token == 0 && remote_fence_owner == 0 &&
                lease_owner == 0 && local_b_token == 0 &&
                ambiguous_remote == 0
            ) {
                remote_token = 2;
                remote_fence_owner = 2;
                lease_owner = 2;
                local_b_token = 2;
            }
            action ObserveStaleARelease when (
                remote_token == 2 && local_a_token == 1 &&
                ambiguous_remote == 0 && stale_release_observed == 0
            ) {
                stale_release_observed = 1;
            }
            action LosePersistentLease when (
                remote_token > 0 && lease_owner == 1
            ) {
                lease_owner = 0;
                incoherent_remote = 1;
            }
            action ObserveIncoherentSuccessor when (
                remote_token == 0 && lease_owner == 1 &&
                incoherent_remote == 0
            ) {
                remote_token = 2;
                remote_fence_owner = 1;
                lease_owner = 2;
                incoherent_remote = 1;
            }
            action RefuseIncoherentRemote when (
                incoherent_remote == 1 && refused == 0
            ) {
                refused = 1;
            }

            action MutateStaleA when (
                Buggy == 1 && remote_token == 2 && local_a_token == 1 &&
                stale_mutation_bypassed == 0
            ) {
                a_mutated = 1;
                stale_mutation_bypassed = 1;
            }
            action StaleADeletesB when (
                Buggy == 1 && remote_token == 2 && local_a_token == 1 &&
                stale_release_bypassed == 0
            ) {
                remote_token = 0;
                remote_fence_owner = 0;
                stale_release_bypassed = 1;
            }
            action StaleARotatesB when (
                Buggy == 1 && remote_token == 2 && local_a_token == 1 &&
                stale_rotation_bypassed == 0
            ) {
                remote_token = 1;
                remote_fence_owner = 1;
                stale_rotation_bypassed = 1;
            }
            action AcquireAThroughAmbiguity when (
                Buggy == 1 && remote_token == 0 && ambiguous_remote == 1 &&
                local_a_token == 0
            ) {
                remote_token = 1;
                remote_fence_owner = 1;
                local_a_token = 1;
                ambiguous_bypassed = 1;
            }
            action MutateAThroughAmbiguity when (
                Buggy == 1 && remote_token == 1 && local_a_token == 1 &&
                ambiguous_remote == 1 && ambiguous_bypassed == 0
            ) {
                a_mutated = 1;
                ambiguous_bypassed = 1;
            }
            action MutateAAfterLeaseLoss when (
                Buggy == 1 && remote_token == 1 && local_a_token == 1 &&
                lease_owner == 0 && lease_bypassed == 0
            ) {
                a_mutated = 1;
                lease_bypassed = 1;
            }
            action AcceptIncoherentSuccessor when (
                Buggy == 1 && incoherent_remote == 1 &&
                incoherent_accepted == 0
            ) {
                incoherent_accepted = 1;
            }
            action RotateLiveAtoB when (
                Buggy == 1 && remote_token == 1 && local_a_token == 1 &&
                local_b_token == 0 && lease_owner == 1 &&
                ambiguous_remote == 0 && rotations == 0 &&
                old_process_stopped == 0
            ) {
                remote_token = 2;
                local_b_token = 2;
                rotations = 1;
                unsafe_rotation_bypassed = 1;
            }

            invariant RefusalHasObservedTransportFault:
                if refused == 1 {
                    if ambiguous_remote == 1 {
                        ambiguous_remote == 1
                    } else {
                        incoherent_remote == 1
                    }
                } else {
                    refused == 0
                };
            invariant StaleSessionCannotMutate:
                stale_mutation_bypassed == 0;
            invariant StaleSessionCannotDeleteWinner:
                stale_release_bypassed == 0;
            invariant StaleSessionCannotRotateWinner:
                stale_rotation_bypassed == 0;
            invariant AmbiguousTransportCannotBeBypassed:
                ambiguous_bypassed == 0;
            invariant MutationRequiresPersistentLease:
                lease_bypassed == 0;
            invariant RecoveryRequiresStoppedOldProcess:
                unsafe_rotation_bypassed == 0;
            invariant StoppedProofIsPerProcess: stale_stop_reused == 0;
            invariant IncoherentSuccessorCannotConverge:
                incoherent_accepted == 0;
            invariant FenceStateBounds:
                remote_token <= 2 && remote_fence_owner <= 2 &&
                local_a_token <= 1 && local_b_token <= 2 &&
                old_process_stopped <= 1 && lease_owner <= 2 &&
                ambiguous_remote <= 1 && refused <= 1 && a_mutated <= 1 &&
                b_mutated <= 1 && b_lost_create <= 1 && rotations <= 1 &&
                uncertain_delete <= 1 && atomic_delete_uncertain <= 1 &&
                stale_release_observed <= 1 && incoherent_remote <= 1 &&
                incoherent_accepted <= 1 &&
                stale_mutation_bypassed <= 1 && stale_release_bypassed <= 1 &&
                stale_rotation_bypassed <= 1 && ambiguous_bypassed <= 1 &&
                lease_bypassed <= 1 && unsafe_rotation_bypassed <= 1 &&
                stale_stop_reused <= 1;
        }
    }
}

/// One-time lost-update-key epoch transition for v0.55.
///
/// The retired historical fingerprint is preserved in a committed one-shot record
/// together with the newly generated canonical public key/fingerprint and exact
/// target version. The shipped v0.55 binary must embed that same key and its
/// manifest must be validly signed by it before publication. Closing the epoch
/// consumes the only transition; there is deliberately no generic rotation flag.
///
/// `Buggy=1` exposes retirement without replacement, a wrong embedded/signing key,
/// an unsigned publication, and a second silent rotation after the epoch closes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_key_epoch_transition_model() -> Model {
    crate::ty_model! {
        ReleaseKeyEpochTransition {
            const Buggy = 0;
            const OldFingerprint = 1;
            const NewKey = 2;
            const WrongKey = 3;
            // phase: 0 OldKeyLost, 1 ExplicitlyAuthorized, 2 RecordPersisted,
            // 3 BinaryPinned, 4 ManifestSigned, 5 Published, 6 EpochClosed.
            var phase = 0;
            var observed_old_fingerprint = 1;
            var retired_old_fingerprint = 0;
            var retired_evidence = 0;
            var repo_current_key = 0;
            var repo_current_fingerprint = 0;
            var target_v055 = 0;
            var transition_count = 0;
            var binary_pin = 0;
            var actual_signing_key = 0;
            var manifest_signing_key = 0;
            var signature_valid = 0;
            var epoch_consumed = 0;
            var retirement_bypassed = 0;
            var silent_key_change = 0;
            var unsigned_bypassed = 0;
            var generic_rotation_bypassed = 0;
            var evidence_erased = 0;

            action AuthorizeLostKeyEpoch when (
                phase == 0 && transition_count == 0 && epoch_consumed == 0
            ) {
                phase = 1;
                transition_count = 1;
                target_v055 = 1;
            }
            action PersistOneShotEpochRecord when (
                phase == 1 && target_v055 == 1 && transition_count == 1
            ) {
                phase = 2;
                retired_old_fingerprint = OldFingerprint;
                retired_evidence = 1;
                repo_current_key = NewKey;
                repo_current_fingerprint = NewKey;
            }
            action BuildV055WithPersistedPin when (
                phase == 2 && repo_current_key == NewKey &&
                repo_current_fingerprint == NewKey && target_v055 == 1
            ) {
                phase = 3;
                binary_pin = NewKey;
            }
            action SignV055Manifest when (
                phase == 3 && binary_pin == repo_current_key
            ) {
                phase = 4;
                actual_signing_key = NewKey;
                manifest_signing_key = NewKey;
                signature_valid = 1;
            }
            action PublishV055Epoch when (
                phase == 4 && retired_old_fingerprint == OldFingerprint &&
                repo_current_key == NewKey && repo_current_fingerprint == NewKey &&
                binary_pin == NewKey && actual_signing_key == NewKey &&
                manifest_signing_key == NewKey && signature_valid == 1 &&
                target_v055 == 1 && transition_count == 1
            ) {
                phase = 5;
            }
            action CloseOneShotEpoch when (phase == 5) {
                phase = 6;
                epoch_consumed = 1;
            }

            action RetireOldWithoutReplacement when (
                Buggy == 1 && phase == 0
            ) {
                phase = 2;
                retired_old_fingerprint = OldFingerprint;
                retirement_bypassed = 1;
            }
            action BuildV055WithWrongPin when (
                Buggy == 1 && phase == 2
            ) {
                phase = 3;
                binary_pin = WrongKey;
                silent_key_change = 1;
            }
            action SignWithSubstitutedKey when (
                Buggy == 1 && phase == 3
            ) {
                phase = 4;
                actual_signing_key = WrongKey;
                manifest_signing_key = WrongKey;
                signature_valid = 1;
                silent_key_change = 1;
            }
            action PublishUnsignedV055 when (
                Buggy == 1 && phase == 3
            ) {
                phase = 5;
                unsigned_bypassed = 1;
            }
            action GenericRotateAfterClose when (
                Buggy == 1 && phase == 6
            ) {
                repo_current_key = WrongKey;
                repo_current_fingerprint = WrongKey;
                transition_count = 2;
                generic_rotation_bypassed = 1;
            }
            action EraseRetiredKeyEvidence when (
                Buggy == 1 && phase > 1 && retired_evidence == 1 &&
                evidence_erased == 0
            ) {
                observed_old_fingerprint = 0;
                retired_old_fingerprint = 0;
                retired_evidence = 0;
                evidence_erased = 1;
            }

            invariant OldFingerprintIsNeverErased:
                observed_old_fingerprint == OldFingerprint;
            invariant PersistedEpochRetainsRetiredEvidence:
                if phase > 1 {
                    retired_old_fingerprint == OldFingerprint &&
                    retired_evidence == 1
                } else {
                    retired_evidence == 0
                };
            invariant RetirementIsAtomicWithReplacement:
                if retired_old_fingerprint == OldFingerprint {
                    repo_current_key == NewKey &&
                    repo_current_fingerprint == NewKey && target_v055 == 1
                } else {
                    retired_old_fingerprint == 0
                };
            invariant PublishedEpochUsesOneExactKey:
                if phase > 4 {
                    repo_current_key == NewKey &&
                    repo_current_fingerprint == NewKey &&
                    binary_pin == NewKey && actual_signing_key == NewKey &&
                    manifest_signing_key == NewKey && signature_valid == 1 &&
                    target_v055 == 1 && transition_count == 1
                } else {
                    phase <= 4
                };
            invariant EpochIsOneShot: transition_count <= 1;
            invariant ConsumedEpochIsClosed:
                if epoch_consumed == 1 { phase == 6 } else { epoch_consumed == 0 };
            invariant RetirementCannotBeBypassed: retirement_bypassed == 0;
            invariant KeyIdentityCannotChangeSilently: silent_key_change == 0;
            invariant UnsignedEpochCannotPublish: unsigned_bypassed == 0;
            invariant GenericRotationDoesNotExist:
                generic_rotation_bypassed == 0;
            invariant HistoricalEvidenceCannotBeErased: evidence_erased == 0;
            invariant KeyEpochBounds:
                phase <= 6 && observed_old_fingerprint <= WrongKey &&
                retired_old_fingerprint <= WrongKey && repo_current_key <= WrongKey &&
                retired_evidence <= 1 && repo_current_fingerprint <= WrongKey &&
                target_v055 <= 1 &&
                transition_count <= 2 && binary_pin <= WrongKey &&
                actual_signing_key <= WrongKey && manifest_signing_key <= WrongKey &&
                signature_valid <= 1 && epoch_consumed <= 1 &&
                retirement_bypassed <= 1 && silent_key_change <= 1 &&
                unsigned_bypassed <= 1 && generic_rotation_bypassed <= 1 &&
                evidence_erased <= 1;
        }
    }
}

/// Historical release recovery is not historical publication.
///
/// A stranded pre-activation owner may converge in exactly two ways: delete an
/// exact observed draft (or abandon when a current journal proves no POST was
/// issued), or finish bookkeeping for an exact release that was already published.
/// Unknown/issued intent plus an absent listing retains the owner because a
/// delayed draft may still appear. A signed historical release is verified with the
/// retired public key; the explicit unsigned bootstrap remains unsigned. Neither
/// branch rebuilds, uploads, tags, or flips the retired version.
///
/// `Buggy=1` exposes five independent prohibited controls: publishing during
/// recovery, accepting the current key for a signed retired-epoch release,
/// unlocking on unknown or issued-but-absent visibility, and deleting a draft
/// without durable issued intent.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_historical_recovery_model() -> Model {
    crate::ty_model! {
        ReleaseHistoricalRecovery {
            const Buggy = 0;
            const RetiredKey = 1;
            const CurrentKey = 2;
            // phase: 0 stranded/unpublished, 1 observed already-published,
            // 2 safely abandoned, 3 post-publish bookkeeping finished.
            var phase = 0;
            var owner_held = 1;
            var signature_required = 0;
            var selected_key = 0;
            var publication_mutation = 0;
            var wrong_key_bypassed = 0;
            // 0 unknown/lost journal, 1 current journal proves no POST,
            // 2 durable create intent issued.
            var create_knowledge = 0;
            var draft_observed = 0;
            var draft_deleted = 0;
            var unsafe_absent_unlock = 0;
            var orphan_draft = 0;

            action LearnNoPostFromCurrentJournal when (
                phase == 0 && create_knowledge == 0
            ) {
                create_knowledge = 1;
            }
            action LearnIssuedIntentFromCurrentJournal when (
                phase == 0 && create_knowledge == 0
            ) {
                create_knowledge = 2;
            }
            action ObserveExactDraft when (
                phase == 0 && owner_held == 1 && draft_observed == 0
            ) {
                draft_observed = 1;
            }
            action DeleteExactDraft when (
                phase == 0 && owner_held == 1 && draft_observed == 1 &&
                draft_deleted == 0 && create_knowledge == 2
            ) {
                draft_deleted = 1;
            }
            action AbandonProvenNoPost when (
                phase == 0 && owner_held == 1 && create_knowledge == 1 &&
                draft_observed == 0
            ) {
                phase = 2;
                owner_held = 0;
            }
            action AbandonDeletedIssuedDraft when (
                phase == 0 && owner_held == 1 && create_knowledge == 2 &&
                draft_deleted == 1
            ) {
                phase = 2;
                owner_held = 0;
            }
            action ObserveUnsignedPublishedLegacy when (
                phase == 0 && owner_held == 1
            ) {
                phase = 1;
                signature_required = 0;
                selected_key = 0;
            }
            action ObserveSignedPublishedLegacy when (
                phase == 0 && owner_held == 1
            ) {
                phase = 1;
                signature_required = 1;
                selected_key = RetiredKey;
            }
            action FinishUnsignedPublishedLegacy when (
                phase == 1 && owner_held == 1 &&
                signature_required == 0 && selected_key == 0
            ) {
                phase = 3;
                owner_held = 0;
            }
            action FinishSignedPublishedLegacy when (
                phase == 1 && owner_held == 1 &&
                signature_required == 1 && selected_key == RetiredKey
            ) {
                phase = 3;
                owner_held = 0;
            }

            action RepublishLegacyDuringRecovery when (
                Buggy == 1 && phase == 0 && owner_held == 1
            ) {
                phase = 1;
                publication_mutation = 1;
            }
            action FinishSignedLegacyWithCurrentKey when (
                Buggy == 1 && phase == 1 && owner_held == 1 &&
                signature_required == 1
            ) {
                phase = 3;
                owner_held = 0;
                selected_key = CurrentKey;
                wrong_key_bypassed = 1;
            }
            action AbandonUnknownAbsent when (
                Buggy == 1 && phase == 0 && owner_held == 1 &&
                create_knowledge == 0 && draft_deleted == 0
            ) {
                phase = 2;
                owner_held = 0;
                unsafe_absent_unlock = 1;
                orphan_draft = 1;
            }
            action AbandonIssuedAbsent when (
                Buggy == 1 && phase == 0 && owner_held == 1 &&
                create_knowledge == 2 && draft_deleted == 0
            ) {
                phase = 2;
                owner_held = 0;
                unsafe_absent_unlock = 1;
                orphan_draft = 1;
            }
            action DeleteUnknownDraft when (
                Buggy == 1 && phase == 0 && owner_held == 1 &&
                create_knowledge == 0 && draft_observed == 1 &&
                draft_deleted == 0
            ) {
                draft_deleted = 1;
            }

            invariant RecoveryNeverPublishesRetiredEpoch:
                publication_mutation == 0;
            invariant SignedLegacyUsesOnlyRetiredKey:
                if signature_required == 1 {
                    selected_key == RetiredKey
                } else {
                    selected_key == 0
                };
            invariant HistoricalKeySubstitutionCannotBeBypassed:
                wrong_key_bypassed == 0;
            invariant AmbiguousAbsenceRetainsOwner:
                unsafe_absent_unlock == 0;
            invariant NoDelayedDraftAfterUnlock: orphan_draft == 0;
            invariant DraftDeletionRequiresIssuedIntent:
                if draft_deleted == 1 {
                    create_knowledge == 2
                } else {
                    draft_deleted == 0
                };
            invariant CompletionReleasesOwner:
                if phase > 1 { owner_held == 0 } else { owner_held == 1 };
            invariant HistoricalRecoveryBounds:
                phase <= 3 && owner_held <= 1 && signature_required <= 1 &&
                selected_key <= CurrentKey && publication_mutation <= 1 &&
                wrong_key_bypassed <= 1 && create_knowledge <= 2 &&
                draft_observed <= 1 && draft_deleted <= 1 &&
                unsafe_absent_unlock <= 1 && orphan_draft <= 1;
        }
    }
}

/// Published-history identity separates GitHub's tag-creation hint from the
/// immutable code binding.
///
/// A historical release may have captured a symbolic `target_commitish` (for
/// example `main`) and is still valid when its exact tag resolves to the
/// manifest commit. A destructive mutation, however, must re-read the complete
/// captured release-object snapshot and the tag binding; target or tag drift
/// refuses deletion. Current draft/claim paths retain their separate SHA-target
/// capability invariant.
///
/// `Buggy=1` reproduces both regressions: rejecting valid symbolic history by
/// equating the creation hint with the manifest commit, and deleting after
/// ignoring target/tag drift.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_published_identity_model() -> Model {
    crate::ty_model! {
        ReleasePublishedIdentity {
            const Buggy = 0;
            const SymbolicTarget = 1;
            const ShaTarget = 2;
            const ManifestCommit = 1;
            var snapshot_target = 1;
            var observed_target = 1;
            var resolved_tag_commit = 1;
            var history_accepted = 0;
            var delete_authorized = 0;
            var deleted = 0;
            var refused = 0;
            var false_rejection = 0;
            var unbound_symbolic_accepted = 0;
            var target_drift_bypassed = 0;
            var tag_drift_bypassed = 0;

            action AcceptSymbolicHistory when (
                history_accepted == 0 &&
                observed_target == snapshot_target &&
                resolved_tag_commit == ManifestCommit
            ) {
                history_accepted = 1;
            }
            action DriftCapturedTarget when (
                history_accepted == 1 && deleted == 0 &&
                observed_target == SymbolicTarget
            ) {
                observed_target = ShaTarget;
            }
            action DriftResolvedTag when (
                history_accepted == 1 && deleted == 0 &&
                resolved_tag_commit == ManifestCommit
            ) {
                resolved_tag_commit = 0;
            }
            action RefuseTargetDrift when (
                history_accepted == 1 && observed_target > snapshot_target &&
                refused == 0
            ) {
                refused = 1;
            }
            action RefuseTagDrift when (
                history_accepted == 1 && resolved_tag_commit == 0 &&
                refused == 0
            ) {
                refused = 1;
            }
            action DeleteWithExactPublishedIdentity when (
                history_accepted == 1 && deleted == 0 &&
                observed_target == snapshot_target &&
                resolved_tag_commit == ManifestCommit
            ) {
                delete_authorized = 1;
                deleted = 1;
            }

            action RejectValidSymbolicHistoryAsNonSha when (
                Buggy == 1 && history_accepted == 0 &&
                snapshot_target == SymbolicTarget &&
                observed_target == snapshot_target &&
                resolved_tag_commit == ManifestCommit
            ) {
                false_rejection = 1;
            }
            action AcceptUnboundSymbolicWithoutTag when (
                Buggy == 1 && history_accepted == 0 &&
                snapshot_target == SymbolicTarget &&
                observed_target == snapshot_target
            ) {
                unbound_symbolic_accepted = 1;
            }
            action DeleteIgnoringTargetDrift when (
                Buggy == 1 && history_accepted == 1 && deleted == 0 &&
                observed_target > snapshot_target &&
                resolved_tag_commit == ManifestCommit
            ) {
                deleted = 1;
                target_drift_bypassed = 1;
            }
            action DeleteIgnoringTagDrift when (
                Buggy == 1 && history_accepted == 1 && deleted == 0 &&
                observed_target == snapshot_target &&
                resolved_tag_commit == 0
            ) {
                deleted = 1;
                tag_drift_bypassed = 1;
            }

            invariant ValidSymbolicHistoryIsNotRejected: false_rejection == 0;
            invariant UnboundSymbolicHistoryFailsClosed:
                unbound_symbolic_accepted == 0;
            invariant DeleteRequiresExactSnapshotAndTag:
                if deleted == 1 {
                    delete_authorized == 1 &&
                    observed_target == snapshot_target &&
                    resolved_tag_commit == ManifestCommit
                } else {
                    deleted == 0
                };
            invariant TargetDriftCannotBeBypassed: target_drift_bypassed == 0;
            invariant TagDriftCannotBeBypassed: tag_drift_bypassed == 0;
            invariant PublishedIdentityBounds:
                snapshot_target <= ShaTarget && observed_target <= ShaTarget &&
                resolved_tag_commit <= ManifestCommit && history_accepted <= 1 &&
                delete_authorized <= 1 && deleted <= 1 && refused <= 1 &&
                false_rejection <= 1 && unbound_symbolic_accepted <= 1 &&
                target_drift_bypassed <= 1 &&
                tag_drift_bypassed <= 1;
        }
    }
}

/// Successor-first, crash-convergent yank cleanup.
///
/// The bad release remains discoverable until its exact tag has been removed by
/// CAS. Every destructive edge requires a fully verified, strictly newer
/// successor whose build is newer and whose minimum-build floor poisons the bad
/// build. Before either destructive edge, cleanup acquires the verified
/// successor commit as the persistent release lease plus a unique publisher
/// fence. Tag-first ordering leaves the published bad manifest as a durable
/// cleanup receipt: after a crash the process can rediscover its exact identity,
/// explicitly recover the stopped publisher session, re-prove the successor,
/// and finish the convergent release delete. Only after the tag is gone may the
/// release disappear, and clean completion atomically releases both session refs.
///
/// `Buggy=1` exposes delete-before-successor, weak-floor cleanup, wrong-identity
/// cleanup, cleanup after lease/fence loss, premature session release, and the
/// release-first crash cut that strands a tag after destroying the only remotely
/// discoverable receipt.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_yank_successor_first_model() -> Model {
    crate::ty_model! {
        ReleaseYankSuccessorFirst {
            const Buggy = 0;
            const BadOrder = 1;
            const BadBuild = 1;
            const RequiredFloor = 2;
            const SuccessorOrder = 2;
            const SuccessorBuild = 2;
            const MaxCrashes = 2;
            var bad_release_present = 1;
            var bad_tag_present = 1;
            var target_identity_valid = 1;
            var target_known = 1;
            var successor_order = 0;
            var successor_build = 0;
            var successor_floor = 0;
            var successor_signature_valid = 0;
            var successor_artifact_valid = 0;
            // This process-local proof is deliberately lost on crash and after
            // each mutation, forcing a fresh replay before the next cleanup edge.
            var successor_proof = 0;
            var cleanup_lease_owned = 0;
            var cleanup_fence_owned = 0;
            var cleanup_guard_attached = 0;
            var cleanup_publisher_stopped = 0;
            var cleanup_lease_lost = 0;
            var cleanup_session_released = 0;
            var tag_deleted_with_session = 0;
            var release_deleted_with_session = 0;
            var cleanup_complete = 0;
            var refused = 0;
            var crashes = 0;
            var tag_response_uncertain = 0;
            var release_response_uncertain = 0;
            var delete_before_successor_bypassed = 0;
            var weak_floor_bypassed = 0;
            var identity_bypassed = 0;
            var release_first_bypassed = 0;
            var cleanup_session_bypassed = 0;
            var early_session_release_bypassed = 0;

            action PublishVerifiedSuccessor when (
                bad_release_present == 1 && target_known == 1 &&
                target_identity_valid == 1 && successor_order == 0
            ) {
                successor_order = SuccessorOrder;
                successor_build = SuccessorBuild;
                successor_floor = RequiredFloor;
                successor_signature_valid = 1;
                successor_artifact_valid = 1;
                successor_proof = 1;
            }
            action ReproveVerifiedSuccessor when (
                successor_order > BadOrder && successor_build > BadBuild &&
                RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 &&
                successor_artifact_valid == 1 && successor_proof == 0
            ) {
                successor_proof = 1;
            }
            action AcquireCleanupLease when (
                cleanup_complete == 0 &&
                target_known == 1 && target_identity_valid == 1 &&
                successor_order > BadOrder && successor_build > BadBuild &&
                RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1 &&
                cleanup_lease_owned == 0 && cleanup_fence_owned == 0 &&
                cleanup_guard_attached == 0
            ) {
                cleanup_lease_owned = 1;
                cleanup_session_released = 0;
                // Acquiring the cross-machine session is a remote boundary;
                // destructive work must replay the successor afterwards.
                successor_proof = 0;
            }
            action AcquireCleanupFence when (
                cleanup_lease_owned == 1 && cleanup_fence_owned == 0 &&
                cleanup_guard_attached == 0
            ) {
                cleanup_fence_owned = 1;
                cleanup_guard_attached = 1;
                cleanup_publisher_stopped = 0;
            }
            action ObserveTargetIdentityMismatch when (
                bad_release_present == 1 && target_known == 1 &&
                target_identity_valid == 1 && bad_tag_present == 1
            ) {
                target_identity_valid = 0;
            }
            action RefuseTargetIdentityMismatch when (
                target_identity_valid == 0 && refused == 0
            ) {
                refused = 1;
            }
            action DeleteExactTagAfterSuccessor when (
                bad_tag_present == 1 && bad_release_present == 1 &&
                target_known == 1 && target_identity_valid == 1 &&
                successor_proof == 1 && successor_order > BadOrder &&
                successor_build > BadBuild && RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1 &&
                cleanup_lease_owned == 1 && cleanup_fence_owned == 1 &&
                cleanup_guard_attached == 1
            ) {
                bad_tag_present = 0;
                successor_proof = 0;
                tag_deleted_with_session = 1;
            }
            action TagDeleteLandsResponseLost when (
                bad_tag_present == 1 && bad_release_present == 1 &&
                target_known == 1 && target_identity_valid == 1 &&
                successor_proof == 1 && successor_order > BadOrder &&
                successor_build > BadBuild && RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1 &&
                cleanup_lease_owned == 1 && cleanup_fence_owned == 1 &&
                cleanup_guard_attached == 1 && tag_response_uncertain == 0
            ) {
                bad_tag_present = 0;
                successor_proof = 0;
                tag_response_uncertain = 1;
                tag_deleted_with_session = 1;
            }
            action DeleteReleaseAfterTag when (
                bad_release_present == 1 && bad_tag_present == 0 &&
                target_known == 1 && target_identity_valid == 1 &&
                successor_proof == 1 && successor_order > BadOrder &&
                successor_build > BadBuild && RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1 &&
                cleanup_lease_owned == 1 && cleanup_fence_owned == 1 &&
                cleanup_guard_attached == 1
            ) {
                bad_release_present = 0;
                successor_proof = 0;
                release_deleted_with_session = 1;
                cleanup_complete = 1;
            }
            action ReleaseDeleteLandsResponseLost when (
                bad_release_present == 1 && bad_tag_present == 0 &&
                target_known == 1 && target_identity_valid == 1 &&
                successor_proof == 1 && successor_order > BadOrder &&
                successor_build > BadBuild && RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1 &&
                cleanup_lease_owned == 1 && cleanup_fence_owned == 1 &&
                cleanup_guard_attached == 1 && release_response_uncertain == 0
            ) {
                bad_release_present = 0;
                successor_proof = 0;
                release_response_uncertain = 1;
                release_deleted_with_session = 1;
            }
            action ConvergeObservedAbsent when (
                bad_release_present == 0 && bad_tag_present == 0 &&
                cleanup_complete == 0 && successor_order > BadOrder &&
                successor_build > BadBuild && RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1
            ) {
                cleanup_complete = 1;
            }
            action CrashDuringCleanup when (
                cleanup_complete == 0 && crashes <= MaxCrashes - 1
            ) {
                target_known = 0;
                successor_proof = 0;
                cleanup_guard_attached = 0;
                crashes = crashes + 1;
            }
            action RediscoverTargetFromPublishedReceipt when (
                target_known == 0 && bad_release_present == 1 &&
                target_identity_valid == 1
            ) {
                target_known = 1;
            }
            action ProveCleanupPublisherStopped when (
                cleanup_lease_owned == 1 && cleanup_fence_owned == 1 &&
                cleanup_guard_attached == 0 && cleanup_publisher_stopped == 0
            ) {
                cleanup_publisher_stopped = 1;
            }
            // Abstracts the explicit stopped-publisher recovery command. It
            // rotates/finishes the stale session and releases both refs; a new
            // yank invocation must acquire a fresh lease+fence before deleting.
            action RecoverAndReleaseCleanupSession when (
                cleanup_lease_owned == 1 && cleanup_fence_owned == 1 &&
                cleanup_guard_attached == 0 && cleanup_publisher_stopped == 1
            ) {
                cleanup_lease_owned = 0;
                cleanup_fence_owned = 0;
                cleanup_publisher_stopped = 0;
            }
            action LoseCleanupLease when (
                cleanup_lease_owned == 1 && cleanup_session_released == 0 &&
                cleanup_lease_lost == 0
            ) {
                cleanup_lease_owned = 0;
                cleanup_guard_attached = 0;
                cleanup_lease_lost = 1;
                refused = 1;
            }
            action ReleaseCleanupSession when (
                cleanup_complete == 1 && cleanup_lease_owned == 1 &&
                cleanup_fence_owned == 1 && cleanup_guard_attached == 1
            ) {
                cleanup_lease_owned = 0;
                cleanup_fence_owned = 0;
                cleanup_guard_attached = 0;
                cleanup_session_released = 1;
            }

            action DeleteTagBeforeSuccessor when (
                Buggy == 1 && bad_tag_present == 1 &&
                delete_before_successor_bypassed == 0
            ) {
                bad_tag_present = 0;
                delete_before_successor_bypassed = 1;
                cleanup_session_bypassed = 1;
            }
            action DeleteTagWithWeakFloor when (
                Buggy == 1 && bad_tag_present == 1 && weak_floor_bypassed == 0
            ) {
                successor_order = SuccessorOrder;
                successor_build = SuccessorBuild;
                successor_floor = BadBuild;
                successor_signature_valid = 1;
                successor_artifact_valid = 1;
                bad_tag_present = 0;
                weak_floor_bypassed = 1;
                cleanup_session_bypassed = 1;
            }
            action DeleteTagWithWrongIdentity when (
                Buggy == 1 && bad_tag_present == 1 &&
                target_identity_valid == 0 && identity_bypassed == 0
            ) {
                bad_tag_present = 0;
                identity_bypassed = 1;
                cleanup_session_bypassed = 1;
            }
            action DeleteReleaseFirstAfterSuccessor when (
                Buggy == 1 && bad_release_present == 1 &&
                bad_tag_present == 1 && successor_order > BadOrder &&
                successor_build > BadBuild && RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1 &&
                release_first_bypassed == 0
            ) {
                bad_release_present = 0;
                target_known = 0;
                successor_proof = 0;
                release_first_bypassed = 1;
                cleanup_session_bypassed = 1;
            }
            action DeleteTagAfterCleanupLeaseLoss when (
                Buggy == 1 && bad_tag_present == 1 &&
                successor_proof == 1 && cleanup_lease_lost == 1 &&
                cleanup_lease_owned == 0 && cleanup_guard_attached == 0 &&
                cleanup_session_bypassed == 0
            ) {
                bad_tag_present = 0;
                cleanup_session_bypassed = 1;
            }
            action ReleaseCleanupSessionEarly when (
                Buggy == 1 && cleanup_complete == 0 &&
                cleanup_lease_owned == 1 && cleanup_fence_owned == 1 &&
                cleanup_guard_attached == 1 &&
                early_session_release_bypassed == 0
            ) {
                cleanup_lease_owned = 0;
                cleanup_fence_owned = 0;
                cleanup_guard_attached = 0;
                cleanup_session_released = 1;
                early_session_release_bypassed = 1;
            }

            invariant TagDeletionRequiresVerifiedSuccessor:
                if bad_tag_present == 0 {
                    successor_order > BadOrder && successor_build > BadBuild &&
                    RequiredFloor <= successor_floor &&
                    successor_signature_valid == 1 &&
                    successor_artifact_valid == 1
                } else {
                    bad_tag_present == 1
                };
            invariant ReleaseDeletionRequiresVerifiedSuccessor:
                if bad_release_present == 0 {
                    successor_order > BadOrder && successor_build > BadBuild &&
                    RequiredFloor <= successor_floor &&
                    successor_signature_valid == 1 &&
                    successor_artifact_valid == 1
                } else {
                    bad_release_present == 1
                };
            invariant ReleaseDeletionRequiresTagGone:
                if bad_release_present == 0 {
                    bad_tag_present == 0
                } else {
                    bad_release_present == 1
                };
            invariant ReceiptSurvivesUntilTagGone:
                if bad_tag_present == 1 {
                    bad_release_present == 1
                } else {
                    bad_tag_present == 0
                };
            invariant CompleteMeansConverged:
                if cleanup_complete == 1 {
                    bad_release_present == 0 && bad_tag_present == 0
                } else {
                    cleanup_complete == 0
                };
            invariant TagDeletionHeldUniqueCleanupSession:
                if bad_tag_present == 0 {
                    tag_deleted_with_session == 1
                } else {
                    bad_tag_present == 1
                };
            invariant ReleaseDeletionHeldUniqueCleanupSession:
                if bad_release_present == 0 {
                    release_deleted_with_session == 1
                } else {
                    bad_release_present == 1
                };
            invariant CleanupSessionCannotBeBypassed:
                cleanup_session_bypassed == 0;
            invariant CleanupSessionReleasesOnlyAfterConvergence:
                if cleanup_session_released == 1 {
                    cleanup_complete == 1
                } else {
                    cleanup_session_released == 0
                };
            invariant ExactIdentityCannotBeBypassed: identity_bypassed == 0;
            invariant SuccessorMustPrecedeCleanup:
                delete_before_successor_bypassed == 0;
            invariant RequiredFloorCannotBeWeakened: weak_floor_bypassed == 0;
            invariant ReleaseFirstOrderingIsForbidden: release_first_bypassed == 0;
            invariant EarlySessionReleaseIsForbidden:
                early_session_release_bypassed == 0;
            invariant YankStateBounds:
                bad_release_present <= 1 && bad_tag_present <= 1 &&
                target_identity_valid <= 1 && target_known <= 1 &&
                successor_order <= SuccessorOrder &&
                successor_build <= SuccessorBuild &&
                successor_floor <= RequiredFloor &&
                successor_signature_valid <= 1 && successor_artifact_valid <= 1 &&
                successor_proof <= 1 && cleanup_lease_owned <= 1 &&
                cleanup_fence_owned <= 1 && cleanup_guard_attached <= 1 &&
                cleanup_publisher_stopped <= 1 && cleanup_lease_lost <= 1 &&
                cleanup_session_released <= 1 && tag_deleted_with_session <= 1 &&
                release_deleted_with_session <= 1 && cleanup_complete <= 1 && refused <= 1 &&
                crashes <= MaxCrashes && tag_response_uncertain <= 1 &&
                release_response_uncertain <= 1 &&
                delete_before_successor_bypassed <= 1 &&
                weak_floor_bypassed <= 1 && identity_bypassed <= 1 &&
                release_first_bypassed <= 1 && cleanup_session_bypassed <= 1 &&
                early_session_release_bypassed <= 1;
        }
    }
}

/// Single-head release-channel archive lifecycle.
///
/// The bounded channel begins with two historical releases whose manifests still use
/// the client's exact discovery name. Signatures are an explicit channel policy:
/// `ConfigureSignatures` adds the corresponding historical signatures and requires
/// the flipped current head to carry one too. `Flip` publishes the journal's exact
/// `(tag, build)` under the discovery names. Metadata-only renames then move each
/// historical object to its deterministic archive name without changing the live
/// head or deleting an object.
///
/// A crash preserves the remote exact-commit owner and completed rename prefix while
/// dropping only the process-local guard. Normal resume reattaches to that same owner
/// and re-proves that the immutable live head still has the journal's exact tag and
/// build before re-entering archive. A legacy journal may be observed without the new
/// remote lease; if a competing owner then publishes a monotonically newer head (or a
/// different tag at the same build), the stale journal must refuse rather than archive
/// against it. The manifest observed under the journal tag must independently match
/// the journal's exact version/build/commit/DMG bytes and signature policy—the
/// production `validate_live_release_identity` seam. Missing or mismatched current
/// manifests, collisions, and a configured-but-missing current signature all refuse
/// before mutation.
///
/// `Buggy=1` exposes independent controls for premature finalization, stale-build and
/// wrong-tag resume, bypassing the exact observed-build or signature gates, a competing
/// owner entering or advancing during archive, and live-head regression. The
/// invariants prove each class is observable as well as preserving every historical
/// object across crash/resume.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_channel_single_head_model() -> Model {
    crate::ty_model! {
        ReleaseChannelSingleHead {
            const Buggy = 0;
            const OldHeads = 2;
            const JournalBuild = 2;
            const JournalTag = 2;
            const MaxBuild = 3;
            const MaxTag = 3;
            // phase: 0 OldChannel, 1 Flipped/journal-at-archive,
            // 2 Archiving, 3 Stable, 4 Refused.
            var phase = 0;
            var old_exact_manifest = 2;
            var old_archived_manifest = 0;
            var old_exact_signature = 0;
            var old_archived_signature = 0;
            // Identity counters distinguish a same-object metadata rename from
            // delete+recreate, which would preserve counts but replace bytes/IDs.
            var preserved_manifest_ids = 2;
            var replacement_manifest_ids = 0;
            var preserved_signature_ids = 0;
            var replacement_signature_ids = 0;
            var current_exact_manifest = 0;
            var current_exact_signature = 0;
            var live_identity_valid = 1;
            var signatures_configured = 0;
            var historical_signature_seen = 0;
            var signature_revalidation_pending = 0;
            // The live exact-name manifest identifies the current channel head.
            // `previous_head_build` and `max_seen_build` make non-regression
            // explicit; archive snapshots make immutability explicit.
            var head_build = 1;
            var head_tag = 1;
            var journal_tag_build = 0;
            var previous_head_build = 1;
            var max_seen_build = 1;
            var archive_head_build = 0;
            var archive_head_tag = 0;
            // Remote owner: 0 None, 1 This journal's exact commit,
            // 2 Competing publisher. `guard_attached` is process-local.
            var owner = 0;
            var guard_attached = 0;
            var legacy_format = 0;
            var legacy_unleased = 0;
            var collision = 0;
            var resumed = 0;
            var finalized = 0;
            var idempotent_recheck = 0;
            var stale_head_bypassed = 0;
            var build_guard_bypassed = 0;
            var identity_guard_bypassed = 0;
            var signature_bypassed = 0;
            var signature_ratchet_bypassed = 0;
            var competing_owner_bypassed = 0;
            var legacy_resume_bypassed = 0;

            action ConfigureSignatures when (
                phase == 0 && signatures_configured == 0 &&
                historical_signature_seen == 0 &&
                signature_revalidation_pending == 0
            ) {
                signatures_configured = 1;
                historical_signature_seen = 1;
                old_exact_signature = OldHeads;
                preserved_signature_ids = OldHeads;
            }
            action DetectSignaturePolicyAdvanceUnderSession when (
                phase == 0 && historical_signature_seen == 0 &&
                signature_revalidation_pending == 0
            ) {
                historical_signature_seen = 1;
                old_exact_signature = OldHeads;
                preserved_signature_ids = OldHeads;
                signature_revalidation_pending = 1;
                owner = 1;
                guard_attached = 1;
            }
            action RejectSignaturePolicyAdvance when (
                phase == 0 && historical_signature_seen == 1 &&
                signatures_configured == 0 && signature_revalidation_pending == 1
            ) {
                phase = 4;
            }
            action IgnoreSignedHistory when (
                Buggy == 1 && phase == 0 && historical_signature_seen == 0
            ) {
                historical_signature_seen = 1;
                old_exact_signature = OldHeads;
                preserved_signature_ids = OldHeads;
                signature_ratchet_bypassed = 1;
            }

            action Flip when (
                phase == 0 && signature_revalidation_pending == 0 &&
                signatures_configured == historical_signature_seen
            ) {
                phase = 1;
                current_exact_manifest = 1;
                current_exact_signature = signatures_configured;
                previous_head_build = head_build;
                head_build = JournalBuild;
                head_tag = JournalTag;
                journal_tag_build = JournalBuild;
                max_seen_build = JournalBuild;
                owner = 1;
                guard_attached = 1;
            }
            action FlipBeforeSignatureRevalidation when (
                Buggy == 1 && phase == 0 &&
                signature_revalidation_pending == 1 &&
                historical_signature_seen == 1 && signatures_configured == 0
            ) {
                phase = 1;
                current_exact_manifest = 1;
                previous_head_build = head_build;
                head_build = JournalBuild;
                head_tag = JournalTag;
                journal_tag_build = JournalBuild;
                max_seen_build = JournalBuild;
                owner = 1;
                guard_attached = 1;
                signature_ratchet_bypassed = 1;
            }
            // Pre-v3 journals are classified before any lease/fence acquisition.
            // This environment edge represents an unfinished historical cut whose
            // live tag already exists but whose recovery protocol is insufficient.
            action LoadUnfinishedLegacyJournal when (
                phase == 0 && owner == 0 && signatures_configured == 0 &&
                historical_signature_seen == 0 &&
                signature_revalidation_pending == 0
            ) {
                phase = 1;
                current_exact_manifest = 1;
                previous_head_build = head_build;
                head_build = JournalBuild;
                head_tag = JournalTag;
                journal_tag_build = JournalBuild;
                max_seen_build = JournalBuild;
                legacy_format = 1;
                legacy_unleased = 1;
                resumed = 1;
            }
            action ExposeCollision when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                collision == 0
            ) {
                collision = 1;
            }
            action ObserveMissingCurrentSignature when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                signatures_configured == 1 &&
                current_exact_signature == 1
            ) {
                current_exact_signature = 0;
            }
            action ObserveMissingCurrentManifest when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 1
            ) {
                current_exact_manifest = 0;
                journal_tag_build = 0;
            }
            action ObserveWrongCurrentBuild when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 1 &&
                journal_tag_build == JournalBuild
            ) {
                journal_tag_build = JournalBuild - 1;
            }
            action ObserveAdvancedCurrentBuild when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 1 &&
                journal_tag_build == JournalBuild
            ) {
                journal_tag_build = JournalBuild + 1;
            }
            action ObserveLiveIdentityMismatch when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 1 && live_identity_valid == 1
            ) {
                live_identity_valid = 0;
            }
            action BeginArchive when (
                phase == 1 && collision == 0 && owner == 1 &&
                guard_attached == 1 &&
                head_build == JournalBuild && head_tag == JournalTag &&
                current_exact_manifest == 1 && journal_tag_build == JournalBuild &&
                current_exact_signature == signatures_configured &&
                live_identity_valid == 1
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
            }
            action AbortCollision when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                collision == 1
            ) {
                phase = 4;
            }
            action AbortMissingSignature when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                signatures_configured == 1 &&
                current_exact_signature == 0
            ) {
                phase = 4;
            }
            action AbortMissingCurrentManifest when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 0
            ) {
                phase = 4;
            }
            action AbortWrongCurrentBuild when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 1 &&
                journal_tag_build <= JournalBuild - 1
            ) {
                phase = 4;
            }
            action AbortAdvancedCurrentBuild when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 1 &&
                journal_tag_build > JournalBuild
            ) {
                phase = 4;
            }
            action AbortLiveIdentityMismatch when (
                phase == 1 && owner == 1 && guard_attached == 1 &&
                current_exact_manifest == 1 && live_identity_valid == 0
            ) {
                phase = 4;
            }
            // Refusal is observed while the local guard is still on the stack;
            // unwinding drops only that process-local handle. The persistent
            // remote lease remains for explicit recovery/abandon.
            action ExitAfterRefusal when (
                phase == 4 && owner == 1 && guard_attached == 1
            ) {
                guard_attached = 0;
            }
            action RenameHistoricalManifest when (
                phase == 2 && owner == 1 && guard_attached == 1 &&
                head_build == JournalBuild && head_tag == JournalTag &&
                current_exact_manifest == 1 && journal_tag_build == JournalBuild &&
                current_exact_signature == signatures_configured &&
                live_identity_valid == 1 && old_exact_manifest > 0
            ) {
                old_exact_manifest = old_exact_manifest - 1;
                old_archived_manifest = old_archived_manifest + 1;
            }
            action RenameHistoricalSignature when (
                phase == 2 && owner == 1 && guard_attached == 1 &&
                head_build == JournalBuild && head_tag == JournalTag &&
                current_exact_manifest == 1 && journal_tag_build == JournalBuild &&
                current_exact_signature == signatures_configured &&
                live_identity_valid == 1 && old_exact_signature > 0
            ) {
                old_exact_signature = old_exact_signature - 1;
                old_archived_signature = old_archived_signature + 1;
            }
            action DeleteAndRecreateHistoricalManifest when (
                Buggy == 1 && phase == 2 && owner == 1 &&
                guard_attached == 1 && old_exact_manifest > 0 &&
                preserved_manifest_ids > 0
            ) {
                old_exact_manifest = old_exact_manifest - 1;
                old_archived_manifest = old_archived_manifest + 1;
                preserved_manifest_ids = preserved_manifest_ids - 1;
                replacement_manifest_ids = replacement_manifest_ids + 1;
            }
            action DeleteAndRecreateHistoricalSignature when (
                Buggy == 1 && phase == 2 && owner == 1 &&
                guard_attached == 1 && old_exact_signature > 0 &&
                preserved_signature_ids > 0
            ) {
                old_exact_signature = old_exact_signature - 1;
                old_archived_signature = old_archived_signature + 1;
                preserved_signature_ids = preserved_signature_ids - 1;
                replacement_signature_ids = replacement_signature_ids + 1;
            }
            // A crash preserves the remote exact-commit lease and every renamed
            // asset, but drops the process-local guard. Resume re-observes that same
            // owner before regaining mutation authority.
            action CrashDuringArchive when (
                phase == 2 && owner == 1 && guard_attached == 1 &&
                resumed == 0
            ) {
                phase = 1;
                resumed = 1;
                guard_attached = 0;
            }
            action ReattachJournalOwner when (
                phase == 1 && owner == 1 && guard_attached == 0 &&
                legacy_format == 0
            ) {
                guard_attached = 1;
            }
            // Old journals predate the remote lease step. This environment edge
            // makes their missing owner explicit instead of pretending a crash of
            // the new protocol releases its persistent lease.
            action ObserveLegacyJournalWithoutLease when (
                Buggy == 1 && phase == 1 && owner == 0 && guard_attached == 0 &&
                resumed == 1 && legacy_format == 1 && legacy_unleased == 1 &&
                legacy_resume_bypassed == 0
            ) {
                legacy_resume_bypassed = 1;
            }
            action RefuseLegacyJournal when (
                phase == 1 && owner == 0 && guard_attached == 0 &&
                resumed == 1 && legacy_format == 1 && legacy_unleased == 1
            ) {
                phase = 4;
            }
            action AcquireJournalOwner when (
                Buggy == 1 && phase == 1 && owner == 0 &&
                guard_attached == 0
            ) {
                owner = 1;
                guard_attached = 1;
                legacy_resume_bypassed = 1;
            }
            action AcquireCompetingOwner when (
                phase == 1 && owner == 0 && guard_attached == 0 &&
                head_build <= MaxBuild - 1 &&
                head_tag <= MaxTag - 1
            ) {
                owner = 2;
            }
            // A different cut may legitimately win after a crash. Builds never
            // regress; the stale journal subsequently has no mutation authority.
            action PublishNewerHead when (
                phase == 1 && owner == 2 && head_build <= MaxBuild - 1 &&
                head_tag <= MaxTag - 1
            ) {
                previous_head_build = head_build;
                head_build = head_build + 1;
                head_tag = head_tag + 1;
                max_seen_build = head_build + 1;
                current_exact_manifest = 1;
                current_exact_signature = signatures_configured;
                owner = 0;
                guard_attached = 0;
            }
            // Exact tag and build are both required. This environment transition
            // isolates the tag half while preserving build monotonicity.
            action ReplaceTagAtSameBuild when (
                phase == 1 && owner == 2 && head_tag <= MaxTag - 1
            ) {
                previous_head_build = head_build;
                head_tag = head_tag + 1;
                max_seen_build = head_build;
                current_exact_manifest = 1;
                current_exact_signature = signatures_configured;
                owner = 0;
                guard_attached = 0;
            }
            action AbortNewerHead when (
                phase == 1 && owner == 0 && guard_attached == 0 &&
                legacy_format == 1 && legacy_unleased == 1 && resumed == 1 &&
                head_build > JournalBuild
            ) {
                phase = 4;
            }
            action AbortWrongTag when (
                phase == 1 && owner == 0 && guard_attached == 0 &&
                legacy_format == 1 && legacy_unleased == 1 && resumed == 1 &&
                head_build == JournalBuild &&
                head_tag > JournalTag
            ) {
                phase = 4;
            }
            action FinalizeArchived when (
                phase == 2 && owner == 1 && guard_attached == 1 &&
                head_build == JournalBuild && head_tag == JournalTag &&
                current_exact_manifest == 1 && journal_tag_build == JournalBuild &&
                current_exact_signature == signatures_configured &&
                live_identity_valid == 1 &&
                old_exact_manifest == 0 && old_exact_signature == 0
            ) {
                phase = 3;
                finalized = 1;
            }
            action FinalizeWithoutArchive when (
                Buggy == 1 && phase == 2 && owner == 1 &&
                guard_attached == 1 &&
                old_exact_manifest > 0
            ) {
                phase = 3;
                finalized = 1;
                owner = 0;
                guard_attached = 0;
            }
            action BeginArchiveStaleHead when (
                Buggy == 1 && phase == 1 && owner == 0 &&
                guard_attached == 0 && legacy_format == 1 &&
                legacy_unleased == 1 && resumed == 1 &&
                head_build > JournalBuild
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
                stale_head_bypassed = 1;
                legacy_resume_bypassed = 1;
            }
            action BeginArchiveWrongTag when (
                Buggy == 1 && phase == 1 && owner == 0 &&
                guard_attached == 0 && legacy_format == 1 &&
                legacy_unleased == 1 && resumed == 1 &&
                head_build == JournalBuild && head_tag > JournalTag
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
                stale_head_bypassed = 1;
                legacy_resume_bypassed = 1;
            }
            action BeginArchiveMissingSignature when (
                Buggy == 1 && phase == 1 && owner == 1 &&
                guard_attached == 1 &&
                head_build == JournalBuild && head_tag == JournalTag &&
                signatures_configured == 1 && current_exact_signature == 0
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
                signature_bypassed = 1;
            }
            action BeginArchiveWrongObservedBuild when (
                Buggy == 1 && phase == 1 && owner == 1 &&
                guard_attached == 1 && head_build == JournalBuild &&
                head_tag == JournalTag &&
                journal_tag_build <= JournalBuild - 1
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
                build_guard_bypassed = 1;
            }
            action BeginArchiveAdvancedObservedBuild when (
                Buggy == 1 && phase == 1 && owner == 1 &&
                guard_attached == 1 && head_build == JournalBuild &&
                head_tag == JournalTag && journal_tag_build > JournalBuild
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
                build_guard_bypassed = 1;
            }
            action BeginArchiveInvalidLiveIdentity when (
                Buggy == 1 && phase == 1 && owner == 1 &&
                guard_attached == 1 && head_build == JournalBuild &&
                head_tag == JournalTag && current_exact_manifest == 1 &&
                journal_tag_build == JournalBuild && live_identity_valid == 0
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
                identity_guard_bypassed = 1;
            }
            action BeginArchiveAsCompetingOwner when (
                Buggy == 1 && phase == 1 && owner == 2 &&
                guard_attached == 0 &&
                head_build == JournalBuild && head_tag == JournalTag
            ) {
                phase = 2;
                archive_head_build = head_build;
                archive_head_tag = head_tag;
                competing_owner_bypassed = 1;
            }
            action CompetingOwnerAdvancesDuringArchive when (
                Buggy == 1 && phase == 2 && owner == 1 &&
                guard_attached == 1 &&
                head_build <= MaxBuild - 1 && head_tag <= MaxTag - 1
            ) {
                previous_head_build = head_build;
                head_build = head_build + 1;
                head_tag = head_tag + 1;
                max_seen_build = head_build + 1;
                owner = 2;
                guard_attached = 0;
                competing_owner_bypassed = 1;
            }
            action RegressCurrentHead when (
                Buggy == 1 && phase == 1 && owner == 2 &&
                guard_attached == 0 && head_build > 1
            ) {
                previous_head_build = head_build;
                head_build = head_build - 1;
                owner = 0;
            }
            // Re-running convergence after success yields an empty plan and leaves
            // both the exact/archive partition and the current head unchanged.
            action RecheckStable when (
                phase == 3 && idempotent_recheck == 0
            ) {
                idempotent_recheck = 1;
            }

            invariant HistoricalManifestNeverDeleted:
                old_exact_manifest + old_archived_manifest == OldHeads;
            invariant HistoricalSignatureNeverDeleted:
                if historical_signature_seen == 1 {
                    old_exact_signature + old_archived_signature == OldHeads
                } else {
                    old_exact_signature + old_archived_signature == 0
                };
            invariant SignedHistoryRatchetsCurrentPolicy:
                if phase > 0 && phase <= 3 {
                    signatures_configured == historical_signature_seen &&
                    signature_revalidation_pending == 0
                } else if signature_revalidation_pending == 1 {
                    historical_signature_seen == 1 && signatures_configured == 0 &&
                    owner == 1 &&
                    if phase == 0 {
                        guard_attached == 1
                    } else {
                        phase == 4 && guard_attached <= 1
                    }
                } else {
                    signatures_configured == historical_signature_seen
                };
            invariant HistoricalManifestIdentityPreserved:
                preserved_manifest_ids == OldHeads &&
                replacement_manifest_ids == 0;
            invariant HistoricalSignatureIdentityPreserved:
                preserved_signature_ids == old_exact_signature +
                    old_archived_signature &&
                replacement_signature_ids == 0;
            invariant CurrentExactHeadSurvivesArchive:
                if phase == 0 {
                    current_exact_manifest == 0 && current_exact_signature == 0
                } else if phase == 2 {
                    current_exact_manifest == 1 &&
                    current_exact_signature == signatures_configured
                } else if phase == 3 {
                    current_exact_manifest == 1 &&
                    current_exact_signature == signatures_configured
                } else {
                    current_exact_manifest <= 1 &&
                    current_exact_signature <= signatures_configured
                };
            invariant CurrentHeadNeverRegresses:
                previous_head_build <= head_build &&
                head_build == max_seen_build;
            invariant ArchiveUsesExactJournalHead:
                if phase == 2 {
                    head_build == JournalBuild && head_tag == JournalTag
                } else {
                    phase <= 4
                };
            invariant ArchiveObservedExactJournalBuild:
                if phase == 2 {
                    current_exact_manifest == 1 &&
                    journal_tag_build == JournalBuild
                } else {
                    phase <= 4
                };
            invariant ArchiveUsesValidatedLiveIdentity:
                if phase == 2 {
                    live_identity_valid == 1
                } else {
                    phase <= 4
                };
            invariant ArchiveHeadIsImmutable:
                if phase == 2 {
                    head_build == archive_head_build &&
                    head_tag == archive_head_tag
                } else {
                    phase <= 4
                };
            invariant ArchiveOwnsSharedLease:
                if phase == 2 {
                    owner == 1 && guard_attached == 1
                } else {
                    owner <= 2 && guard_attached <= 1
                };
            invariant NominalCrashPreservesRemoteLease:
                if phase == 1 && resumed == 1 && legacy_unleased == 0 {
                    owner == 1
                } else {
                    owner <= 2
                };
            invariant ConfiguredSignatureRequiredForArchive:
                if phase == 2 {
                    current_exact_signature == signatures_configured
                } else {
                    current_exact_signature <= signatures_configured
                };
            invariant StableHasSingleExactHead:
                if phase == 3 {
                    finalized == 1 && current_exact_manifest == 1 &&
                    current_exact_signature == signatures_configured &&
                    old_exact_manifest == 0 && old_exact_signature == 0 &&
                    head_build == JournalBuild && head_tag == JournalTag &&
                    journal_tag_build == JournalBuild &&
                    owner == 1 && guard_attached == 1
                } else {
                    finalized == 0
                };
            invariant ArchiveExitRetainsSharedLease:
                if phase == 3 {
                    owner == 1 && guard_attached == 1
                } else if phase == 4 {
                    if legacy_format == 1 { owner == 0 } else { owner == 1 }
                } else {
                    owner <= 2 && guard_attached <= 1
                };
            invariant StablePreservesArchivedHistory:
                if phase == 3 {
                    old_archived_manifest == OldHeads &&
                    old_archived_signature == if historical_signature_seen == 1 {
                        OldHeads
                    } else {
                        0
                    }
                } else {
                    phase <= 4
                };
            invariant CollisionNeverFinalizes:
                if finalized == 1 { collision == 0 } else { collision <= 1 };
            invariant StaleHeadCannotBeBypassed: stale_head_bypassed == 0;
            invariant ObservedBuildGuardCannotBeBypassed:
                build_guard_bypassed == 0;
            invariant LiveIdentityGuardCannotBeBypassed:
                identity_guard_bypassed == 0;
            invariant SignaturePolicyCannotBeBypassed: signature_bypassed == 0;
            invariant SignatureRatchetCannotBeBypassed:
                signature_ratchet_bypassed == 0;
            invariant CompetingOwnerCannotBypassLease:
                competing_owner_bypassed == 0;
            invariant LegacyJournalCannotResumeMutation:
                legacy_resume_bypassed == 0;
            invariant ArchiveStateBounds:
                phase <= 4 && old_exact_manifest <= OldHeads &&
                old_archived_manifest <= OldHeads &&
                old_exact_signature <= OldHeads &&
                old_archived_signature <= OldHeads &&
                preserved_manifest_ids <= OldHeads &&
                replacement_manifest_ids <= OldHeads &&
                preserved_signature_ids <= OldHeads &&
                replacement_signature_ids <= OldHeads &&
                current_exact_manifest <= 1 && current_exact_signature <= 1 &&
                live_identity_valid <= 1 &&
                signatures_configured <= 1 && historical_signature_seen <= 1 &&
                signature_revalidation_pending <= 1 &&
                head_build <= MaxBuild &&
                head_tag <= MaxTag && journal_tag_build <= MaxBuild &&
                previous_head_build <= MaxBuild &&
                max_seen_build <= MaxBuild && archive_head_build <= MaxBuild &&
                archive_head_tag <= MaxTag && owner <= 2 && guard_attached <= 1 &&
                legacy_format <= 1 && legacy_unleased <= 1 && collision <= 1 &&
                resumed <= 1 &&
                finalized <= 1 && idempotent_recheck <= 1 &&
                stale_head_bypassed <= 1 && build_guard_bypassed <= 1 &&
                identity_guard_bypassed <= 1 &&
                signature_bypassed <= 1 && signature_ratchet_bypassed <= 1 &&
                competing_owner_bypassed <= 1 && legacy_resume_bypassed <= 1;
        }
    }
}
