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
///
/// `SingleFlight` bounds a flag: the real service holds its work as ONE
/// `Option<UpdaterWorkTicket>` (projected `active.is_some()`), so a second worker
/// is unrepresentable rather than refused. The single-flight law itself — a
/// request while work is in flight joins the live ticket and moves nothing — is
/// bound at Tier-1 on `request_check`'s real `CheckStart::Joined`.
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
/// reproducing an unjournaled request or a duplicate retry after crash/resume. It
/// also TRUSTS A POST'S OWN ANSWER in the three places the shipping code refuses
/// to: the draft converges, its assets are journaled against it, and an asset
/// converges, each on the strength of a landed POST rather than the re-listed
/// object. `step_draft` binds (and journals) the release ID the POST answered
/// FIRST, but reports the draft converged — returns `Ok`, so the step is
/// journaled done and the upload may begin — only after `release_object_by_id`
/// re-reads that ID and `validate_release_object_capability` accepts it; and
/// `upload_release_asset_by_id` returns only after
/// `release_asset_identity_for_release_id_optional` sees the asset — "returned
/// success but no asset is visible" is its own refusal. The mutants are that
/// re-read skipped.
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
                attached == 1 && create_converged == 0 &&
                (create_visible == 1 || (Buggy == 1 && create_posts > 0))
            ) {
                create_converged = 1;
            }
            action PersistUploadIntent when (
                attached == 1 &&
                (create_converged == 1 || (Buggy == 1 && create_posts > 0)) &&
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
                attached == 1 && upload_converged == 0 &&
                (upload_visible == 1 || (Buggy == 1 && upload_posts > 0))
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

/// Crash/restart and snapshot-CAS protocol for the machine-roster body/signature pair.
///
/// The pair has two independently renamed files, so the fixed redo directory is the
/// durable commit point.  `body`/`signature` use the same small identity domain:
/// `0` is the exact predecessor (including "absent" for first setup), `1` is the
/// exact transaction target, and `2` is an unrelated/newer value.  A writer checks
/// its snapshot while holding the shared lock, commits the complete redo record before
/// either promotion, and recovery moves every predecessor/target mixture forward.
/// Read-only acquisition reports a redo record without replaying it.
///
/// `Buggy=1` exposes the regressions this protocol exists to prevent: retiring a
/// body-only pair, overwriting unrelated bytes during replay, mutating from the
/// check-only lane, promoting the body with no redo record (`publish_roster` before
/// 8dbc4e967 — "promote the document, and on an *error* between the renames roll
/// the document back"; a process death runs no error path and leaves the new body
/// beside the old signature), and a per-half CAS that promotes the body on its own
/// premise before finding the signature stale. Tier-1
/// (`crates/atpkg-keys/tests/roster_redo_model.rs`) binds the real `lock_roster` /
/// `lock_roster_read_only` / `commit_roster_pair` / `publish_roster_locked`
/// filesystem operations to these decisions, and replays the pre-redo torn pair.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn roster_pair_redo_model() -> Model {
    crate::ty_model! {
        RosterPairRedo {
            const Buggy = 0;
            const MaxIdentity = 2;
            const MaxWrites = 2;
            var body = 0;
            var signature = 0;
            var redo = 0;
            var writer_lock = 0;
            var snapshot_checked = 0;
            // result: 0 in progress, 1 exact success, 2 refused.
            var result = 0;
            var writer_writes = 0;
            var readonly_checked = 0;
            var readonly_refused = 0;
            var readonly_writes = 0;
            var foreign_seen = 0;
            var foreign_overwritten = 0;
            var crashes = 0;

            // Environment changes before lock acquisition are precisely what the
            // byte-for-byte snapshot comparison must reject.
            action AdvanceBodyBeforeCas when (
                result == 0 && writer_lock == 0 && redo == 0 && body == 0
            ) {
                body = 2;
                foreign_seen = 1;
            }
            action AdvanceSignatureBeforeCas when (
                result == 0 && writer_lock == 0 && redo == 0 && signature == 0
            ) {
                signature = 2;
                foreign_seen = 1;
            }
            action AcquireWriter when (
                result == 0 && writer_lock == 0 && redo == 0
            ) {
                writer_lock = 1;
            }
            action AcceptSnapshot when (
                result == 0 && writer_lock == 1 && redo == 0 &&
                body == 0 && signature == 0 && snapshot_checked == 0
            ) {
                snapshot_checked = 1;
            }
            action RejectStaleSnapshot when (
                result == 0 && writer_lock == 1 && redo == 0 &&
                (body == 2 || signature == 2)
            ) {
                result = 2;
            }

            // The public pair-publish call is one logical transition.  The crash-cut
            // actions below expose each durable intermediate state of that call.
            action PublishExact when (
                result == 0 && writer_lock == 1 && snapshot_checked == 1 &&
                redo == 0 && body == 0 && signature == 0
            ) {
                body = 1;
                signature = 1;
                result = 1;
                writer_writes = 2;
            }
            action CrashAfterRedo when (
                result == 0 && writer_lock == 1 && snapshot_checked == 1 &&
                redo == 0 && body == 0 && signature == 0 && crashes == 0
            ) {
                redo = 1;
                writer_lock = 0;
                crashes = 1;
            }
            action CrashAfterBody when (
                result == 0 && writer_lock == 1 && snapshot_checked == 1 &&
                redo == 0 && body == 0 && signature == 0 && crashes == 0
            ) {
                body = 1;
                redo = 1;
                writer_lock = 0;
                crashes = 1;
                writer_writes = 1;
            }
            action CrashAfterPair when (
                result == 0 && writer_lock == 1 && snapshot_checked == 1 &&
                redo == 0 && body == 0 && signature == 0 && crashes == 0
            ) {
                body = 1;
                signature = 1;
                redo = 1;
                writer_lock = 0;
                crashes = 1;
                writer_writes = 2;
            }
            action ReplaceBodyWhileDown when (
                result == 0 && writer_lock == 0 && redo == 1 && body <= 1
            ) {
                body = 2;
                foreign_seen = 1;
            }
            action ReplaceSignatureWhileDown when (
                result == 0 && writer_lock == 0 && redo == 1 && signature <= 1
            ) {
                signature = 2;
                foreign_seen = 1;
            }
            action RecoverKnown when (
                result == 0 && writer_lock == 0 && redo == 1 &&
                body <= 1 && signature <= 1
            ) {
                body = 1;
                signature = 1;
                redo = 0;
                writer_lock = 1;
                result = 1;
                writer_writes = 2;
            }
            action RejectForeignRecovery when (
                result == 0 && writer_lock == 0 && redo == 1 &&
                (body > 1 || signature > 1)
            ) {
                result = 2;
            }

            action ReadOnlyObserveClean when (
                result == 0 && writer_lock == 0 && redo == 0 &&
                readonly_checked == 0
            ) {
                readonly_checked = 1;
            }
            action ReadOnlyRejectRedo when (
                result == 0 && writer_lock == 0 && redo == 1 &&
                readonly_checked == 0
            ) {
                readonly_checked = 1;
                readonly_refused = 1;
            }
            action ReleaseSuccess when (result == 1 && writer_lock == 1) {
                writer_lock = 0;
            }
            action ReleaseRefusal when (result == 2 && writer_lock == 1) {
                writer_lock = 0;
            }

            action BuggyRetirePartial when (
                Buggy > 0 && result == 0 && writer_lock == 0 && redo == 1 &&
                body == 1 && signature == 0
            ) {
                redo = 0;
                writer_lock = 1;
                result = 1;
            }
            action BuggyOverwriteForeign when (
                Buggy > 0 && result == 0 && writer_lock == 0 && redo == 1 &&
                (body == 2 || signature == 2)
            ) {
                body = 1;
                signature = 1;
                redo = 0;
                writer_lock = 1;
                result = 1;
                writer_writes = 2;
                foreign_overwritten = 1;
            }
            action BuggyReadOnlyReplay when (
                Buggy > 0 && result == 0 && writer_lock == 0 && redo == 1 &&
                readonly_checked == 0
            ) {
                body = 1;
                signature = 1;
                redo = 0;
                result = 1;
                readonly_checked = 1;
                readonly_writes = 2;
            }
            // The pre-redo publisher: the body is renamed into place with no
            // committed redo record, and the process dies before the signature.
            action BuggyPromoteBodyWithoutRedo when (
                Buggy > 0 && result == 0 && writer_lock == 1 && snapshot_checked == 1 &&
                redo == 0 && body == 0 && signature == 0 && crashes == 0
            ) {
                body = 1;
                writer_lock = 0;
                crashes = 1;
                writer_writes = 1;
            }
            // A CAS checked per half at each rename: the body's premise holds, so
            // the body is promoted, and only then is the signature found stale.
            action BuggyPromoteBeforeSignatureCas when (
                Buggy > 0 && result == 0 && writer_lock == 1 && redo == 0 &&
                snapshot_checked == 0 && body == 0 && signature == 2
            ) {
                body = 1;
                writer_writes = 1;
                result = 2;
            }
            action Done when (result > 0 && writer_lock == 0) {
                result = result;
            }

            invariant SuccessfulPairIsExact:
                if result == 1 {
                    body == 1 && signature == 1 && redo == 0
                } else {
                    result <= 2
                };
            invariant TargetHalfHasRedoAuthority:
                if result == 0 && (body == 1 || signature == 1) {
                    redo == 1
                } else {
                    redo <= 1
                };
            invariant StaleSnapshotWritesNothing:
                if result == 2 && redo == 0 {
                    writer_writes == 0
                } else {
                    writer_writes <= MaxWrites
                };
            invariant ForeignBytesAreNeverOverwritten: foreign_overwritten == 0;
            invariant ReadOnlyNeverWrites: readonly_writes == 0;
            invariant StateBounded:
                body <= MaxIdentity && signature <= MaxIdentity && redo <= 1 &&
                writer_lock <= 1 && snapshot_checked <= 1 && result <= 2 &&
                writer_writes <= MaxWrites && readonly_checked <= 1 &&
                readonly_refused <= 1 && readonly_writes <= MaxWrites &&
                foreign_seen <= 1 && foreign_overwritten <= 1 && crashes <= 1;
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
/// lease remains held after the head PATCH until what a stranger sees has been proved
/// (`publish`'s last act — the client's own election, the evergreen pointer, the
/// anonymous downloads); only the final journaled unlock releases it. This models the
/// whole scan → freeze/crash/resume → lease → revalidate → publish → prove head →
/// unlock lifecycle rather than testing the arithmetic decisions in isolation. (Until
/// 2026-09-26 the post-publish suffix was the private origin's archive and verify;
/// the one publication proves its own head.)
///
/// `Buggy=1` enables independent non-vacuity controls:
/// `ResolveOperatorOnly` drops the observed channel input (the retired
/// operator-only policy), `PublishUnchecked` skips late revalidation,
/// `BypassLeaseAdvance` lets the channel change after a covered verdict despite
/// lease ownership, and `UnlockBeforeHeadProof` drops the owner before the head is
/// proved. Six more are the slips each remaining law exists to refuse, one apiece:
/// `ResolveUncheckedCarryForward` validates only the operator's request against the
/// claim, so a channel floor above the build is carried forward (`effective_min_build`
/// validates the MAXIMUM); `ResumeFromOperatorRequest` rebuilds the resumed floor from
/// the resume command's request instead of `journal.min_build`;
/// `ConfirmCoveredWithoutLease` is the floor check run without the owner check
/// `publish_checked` pairs it with; `CompleteWithoutUnlock` journals completion over a
/// refused CAS delete; `RejectAdvancedReleasingLease` drops the remote lease on the late
/// guard's error path; and `AbandonIgnoringFailedCas` marks an abandon done whose CAS
/// delete never landed. Tier-1 binds the resolver, journal, `PublishChecked`,
/// exact-owner acquire/resume, and CAS unlock production seams.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_channel_floor_model() -> Model {
    crate::ty_model! {
        ReleaseChannelFloor {
            const Buggy = 0;
            const MaxFloor = 4;
            // phase: 0 Inputs, 1 Frozen, 2 Revalidated under lease,
            // 3 Published (the head PATCH), 4 Aborted, 5 ResumePending,
            // 6 Head proved, 7 Completed/Unlocked.
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
            var head_proved = 0;
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
            action ProveHead when (phase == 3 && lease_owned == 1) {
                phase = 6;
                head_proved = 1;
            }
            action Unlock when (
                phase == 6 && lease_owned == 1 && head_proved == 1
            ) {
                phase = 7;
                lease_owned = 0;
            }
            action UnlockBeforeHeadProof when (
                Buggy == 1 && phase == 3 && lease_owned == 1
            ) {
                phase = 7;
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
            action ResolveUncheckedCarryForward when (
                Buggy == 1 && phase == 0 && operator_floor <= claimed_build &&
                observed_floor > claimed_build
            ) {
                phase = 1;
                frozen_floor = observed_floor;
                journal_floor = observed_floor;
            }
            action ResumeFromOperatorRequest when (Buggy == 1 && phase == 5) {
                phase = 1;
                frozen_floor = operator_floor;
                resumed = 1;
            }
            action ConfirmCoveredWithoutLease when (
                Buggy == 1 && phase == 1 && lease_owned == 0 &&
                latest_floor <= frozen_floor
            ) {
                phase = 2;
                late_checked = 1;
            }
            action CompleteWithoutUnlock when (
                Buggy == 1 && phase == 6 && lease_owned == 1 && head_proved == 1
            ) {
                phase = 7;
            }
            action RejectAdvancedReleasingLease when (
                Buggy == 1 && phase == 1 && lease_owned == 1 &&
                latest_floor > frozen_floor
            ) {
                phase = 4;
                late_checked = 1;
                advanced_rejected = 1;
                lease_owned = 0;
            }
            action AbandonIgnoringFailedCas when (
                Buggy == 1 && phase == 4 && lease_owned == 1 &&
                advanced_rejected == 1 && abandon_done == 0
            ) {
                abandon_done = 1;
            }

            invariant FrozenCoversInitialInputs:
                if phase > 0 && phase <= 3 {
                    operator_floor <= frozen_floor &&
                    observed_floor <= frozen_floor
                } else if phase > 5 {
                    operator_floor <= frozen_floor &&
                    observed_floor <= frozen_floor
                } else {
                    phase <= 7
                };
            invariant FrozenFloorFitsClaim:
                if phase > 0 && phase <= 3 {
                    frozen_floor <= claimed_build
                } else if phase > 5 {
                    frozen_floor <= claimed_build
                } else {
                    phase <= 7
                };
            invariant RuntimeMatchesFrozenJournal:
                if phase > 0 && phase <= 3 {
                    frozen_floor == journal_floor
                } else if phase > 5 {
                    frozen_floor == journal_floor
                } else {
                    phase <= 7
                };
            invariant JournalSurvivesCrash:
                if phase == 5 {
                    operator_floor <= journal_floor &&
                    observed_floor <= journal_floor &&
                    journal_floor <= claimed_build && frozen_floor == 0
                } else {
                    phase <= 7
                };
            invariant PublishedNeverLowersLatest:
                if phase == 3 {
                    latest_floor <= frozen_floor
                } else if phase > 5 {
                    latest_floor <= frozen_floor
                } else {
                    phase <= 7
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
                } else if phase == 6 {
                    lease_owned == 1
                } else {
                    lease_owned <= 1
                };
            invariant CompletedReleasesLease:
                if phase == 7 { lease_owned == 0 } else { lease_owned <= 1 };
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
            invariant CompletionRequiresProvedHead:
                if phase == 7 {
                    head_proved == 1
                } else {
                    phase <= 7
                };
            invariant LeaseCannotBeBypassed: lease_bypassed == 0;
            invariant UnlockCannotBeBypassed: unlock_bypassed == 0;
            invariant FloorStateBounds:
                phase <= 7 && operator_floor <= MaxFloor &&
                observed_floor <= MaxFloor && claimed_build <= MaxFloor &&
                frozen_floor <= MaxFloor && journal_floor <= MaxFloor &&
                latest_floor <= MaxFloor && late_checked <= 1 && resumed <= 1 &&
                lease_owned <= 1 && lease_bypassed <= 1 && head_proved <= 1 &&
                unlock_bypassed <= 1 &&
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
/// or bad-identity journal, can skip preparation after resume, and can complete the
/// cut with its final verify/unlock step unjournaled — the cut reported DONE (and
/// its journal retired) over a last step that failed, so nothing is left to resume
/// the unlock from.
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
            action CompleteBeforeUnlockJournaled when (
                Buggy == 1 && phase == 1 && attached == 1 && resume_cursor == 3 &&
                done_lock == 1 && done_prepare == 1 && done_visible == 1 &&
                done_unlock == 0
            ) {
                phase = 2;
                resume_cursor = 4;
                attached = 0;
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
/// stopped-proof reuse, lease-loss mutation, and ambiguity bypass controls, plus
/// the opposite failure: `RefuseWellFormedFence` refuses a coherent, unambiguous
/// fence — the parser slip that counts the annotated ref's own `^{}` peel row as
/// an extra row (`publisher_fence` requires exactly those two), which wedges every
/// resume behind a refusal no fault explains. Tier-1
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
            action RefuseWellFormedFence when (
                Buggy == 1 && refused == 0 && remote_token > 0 &&
                ambiguous_remote == 0 && incoherent_remote == 0
            ) {
                refused = 1;
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

/// Historical release recovery is not historical publication.
///
/// A stranded pre-activation owner may converge in exactly two ways: delete an
/// exact observed draft (or abandon when no POST was issued), or finish
/// bookkeeping for an exact release that was already published. Unknown/issued
/// intent plus an absent listing retains the owner because a delayed draft may
/// still appear. A signed historical release is verified with the retired public
/// key; the explicit unsigned bootstrap remains unsigned. Neither branch rebuilds,
/// uploads, tags, or flips the retired version.
///
/// A LOST JOURNAL IS ANSWERED IN TWO WAYS (728af7315), both modeled: a visible
/// draft whose target is the recovery claim's commit (`draft_bound`, the shipping
/// `claim_bound`) is provably this claim's, so the remote's binding stands for
/// issued intent (`LearnIssuedIntentFromClaimBinding`, knowledge 2); and with
/// nothing visible, the operator's `--no-draft-was-posted` stands for a no-POST
/// journal (`AbandonOnOperatorNoPostAnswer`). A journal that proves a POST was
/// issued is never overridable. Every delete also needs the draft bound to the
/// claim: `delete_owned_draft_release` runs `validate_release_object_capability`
/// against the claim commit before it deletes, whatever the journal said.
///
/// `Buggy=1` exposes seven independent prohibited controls: publishing during
/// recovery, accepting the current key for a signed retired-epoch release,
/// unlocking on unknown visibility without the operator's separate answer (the
/// answer folded into `--old-publisher-stopped`), the operator's answer overriding
/// a journal that proves a POST, deleting an UNBOUND draft on a lost journal (the
/// lost-journal row with its `claim_bound` conjunct dropped), deleting an issued
/// but unbound draft (the capability check skipped), and releasing the owner
/// before recovery has finished — `release_completed_publisher_session` run ahead
/// of `delete_owned_release_tag`, so a failed tag delete returns with the lease
/// already free for a successor to find our half-cleaned tag.
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
            // 0 unknown/lost journal, 1 no POST (a current journal proves it),
            // 2 durable create intent issued (a current journal, or the remote's
            // claim binding of a visible draft).
            var create_knowledge = 0;
            var draft_observed = 0;
            // The observed draft targets the recovery claim's commit.
            var draft_bound = 0;
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
                draft_bound = 1;
            }
            // Someone else's object under this tag: visible, but not this claim's.
            action ObserveUnboundDraft when (
                phase == 0 && owner_held == 1 && draft_observed == 0
            ) {
                draft_observed = 1;
                draft_bound = 0;
            }
            action LearnIssuedIntentFromClaimBinding when (
                phase == 0 && owner_held == 1 && create_knowledge == 0 &&
                draft_observed == 1 && draft_bound == 1
            ) {
                create_knowledge = 2;
            }
            action DeleteExactDraft when (
                phase == 0 && owner_held == 1 && draft_observed == 1 &&
                draft_bound == 1 && draft_deleted == 0 && create_knowledge == 2
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
            action AbandonOnOperatorNoPostAnswer when (
                phase == 0 && owner_held == 1 && create_knowledge == 0 &&
                draft_observed == 0
            ) {
                phase = 2;
                owner_held = 0;
                create_knowledge = 1;
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
                draft_bound == 0 && draft_deleted == 0
            ) {
                draft_deleted = 1;
            }
            action DeleteIssuedDraftWithoutCapabilityCheck when (
                Buggy == 1 && phase == 0 && owner_held == 1 &&
                create_knowledge == 2 && draft_observed == 1 &&
                draft_bound == 0 && draft_deleted == 0
            ) {
                draft_deleted = 1;
            }

            action ReleaseOwnerBeforeTagCleanup when (
                Buggy == 1 && phase == 0 && owner_held == 1 &&
                create_knowledge == 2 && draft_deleted == 1
            ) {
                owner_held = 0;
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
            invariant DeletedDraftTargetsTheClaim:
                if draft_deleted == 1 {
                    draft_observed == 1 && draft_bound == 1
                } else {
                    draft_deleted == 0
                };
            invariant CompletionReleasesOwner:
                if phase > 1 { owner_held == 0 } else { owner_held == 1 };
            invariant HistoricalRecoveryBounds:
                phase <= 3 && owner_held <= 1 && signature_required <= 1 &&
                selected_key <= CurrentKey && publication_mutation <= 1 &&
                wrong_key_bypassed <= 1 && create_knowledge <= 2 &&
                draft_observed <= 1 && draft_bound <= 1 && draft_deleted <= 1 &&
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
/// Since the cut publishes once (2026-09-26), "the release disappears" is a DEMOTION
/// of the bad channel release to a prerelease (`verify::run_yank`): to every client
/// and every scan it has left the published set exactly as a deleted release has, and
/// it keeps its signed source attestation and stays reversible. The tag is the origin
/// tag the cut pushed.
///
/// `Buggy=1` exposes delete-before-successor, weak-floor cleanup, wrong-identity
/// cleanup, cleanup after lease/fence loss, premature session release, the
/// release-first crash cut that strands a tag after destroying the only remotely
/// discoverable receipt, and a convergence probe that reads only the tag: cleanup
/// declared complete once the tag is gone while the bad release is still listed.
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
            action ConvergeOnTagAbsenceOnly when (
                Buggy == 1 && bad_tag_present == 0 && bad_release_present == 1 &&
                cleanup_complete == 0 && successor_order > BadOrder &&
                successor_build > BadBuild && RequiredFloor <= successor_floor &&
                successor_signature_valid == 1 && successor_artifact_valid == 1
            ) {
                cleanup_complete = 1;
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

/// The release CLAIM, where it LANDS, and the reader that classifies the NEXT cut
/// (2026-09-23, owner ruling R2) — the writer/reader contract of `ledger::claim` +
/// `changelog::claim_changelogs` (writers) and `publish::real_cut_version` over
/// `verify::derive_cut_mode` (reader), in crates/aterm-release.
///
/// A real cut builds the published commit P. The claim writes ONE release commit
/// R = P + one ledger line + P's changelog rolled — P's code and nothing else — and
/// main takes it as a fast-forward when its tip is P, else as a merge onto the tip
/// whose tree is the TIP's plus the same line and the shipped notes. The push is the
/// compare-and-swap on main's tip: a lost race re-reads the winner's tip and ledger
/// and claims again — unless main now carries this version's section and the cut is
/// not a recut, which is the "cut elsewhere" abort. The NEXT cut reads MAIN's
/// changelog section and the published state: section and unpublished is a recut of
/// a claim whose cut died, section and published refuses, anything else is fresh.
///
/// Environment: peers push code (`PeerPush`), other versions' claims land ledger
/// lines (`RivalClaim`), and another machine may claim THIS version while our claim
/// is in flight (`ElsewhereClaim`, the one foreign section); each moves main's tip
/// (`seq`). Build numbers are ordinals (the seed line is 1).
///
/// `Buggy = 1` turns on the defects this contract rules out, each caught by its own
/// invariant: the release built from main's tip (the pre-R2 cut); the landing tree
/// built from R (peers' code and other claims' ledger lines dropped from main); a
/// retry that keeps its stale build number; and the reader taking the recut signal
/// from the published commit's changelog — which never carries the section — so a
/// claimed-unpublished version is classified fresh, then aborted as "cut elsewhere"
/// on its own claim, and a published one is claimed again.
///
/// Tier-1: crates/aterm-release/tests/it/claim_landing_model.rs drives the real claim,
/// claim_changelogs and real_cut_version against real git and checks every
/// transition here, replaying the source-changelog reader and an R-tree landing as
/// its negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_claim_landing_model() -> Model {
    crate::ty_model! {
        ReleaseClaimLanding {
            const Buggy = 0;
            const MaxPeers = 2;
            const MaxRivals = 1;
            const MaxSeq = 6;
            // main, as the environment moves it
            var peers = 0;
            var rivals = 0;
            var foreign = 0;
            var seq = 0;
            var published = 0;
            // main's tree, projected
            var main_code = 0;
            var main_lines = 1;
            var tail = 1;
            var section = 0;
            // the cut: 0 idle, 1 classified, 2 claiming, 3 landed, 4 died unpublished,
            // 5 refused (already published, or cut elsewhere)
            var phase = 0;
            var mode = 0;
            var allow = 0;
            var read_seq = 0;
            var n = 0;
            var release_code = 0;
            var landings = 0;
            var prior = 0;
            var last = 0;
            var elsewhere = 0;
            var after_publish = 0;

            action PeerPush when (MaxPeers > peers && MaxSeq > seq) {
                peers = peers + 1;
                main_code = main_code + 1;
                seq = seq + 1;
            }
            action RivalClaim when (MaxRivals > rivals && MaxSeq > seq) {
                rivals = rivals + 1;
                main_lines = main_lines + 1;
                tail = tail + 1;
                seq = seq + 1;
            }
            action ElsewhereClaim when (
                foreign == 0 && section == 0 && (phase == 1 || phase == 2) && MaxSeq > seq
            ) {
                foreign = 1;
                section = 1;
                main_lines = main_lines + 1;
                tail = tail + 1;
                seq = seq + 1;
            }

            // THE READER. `section` is MAIN's changelog. The Buggy reader takes it from
            // the published commit's changelog, which never carries it: every cut is
            // fresh to it, and a fresh cut claims without allowing the section.
            action ClassifyFresh when (
                (phase == 0 || phase == 4) &&
                ((section == 0 && published == 0) || Buggy == 1)
            ) {
                phase = 1;
                mode = 1;
                allow = 0;
            }
            action ClassifyRecut when (
                (phase == 0 || phase == 4) && section == 1 && published == 0
            ) {
                phase = 1;
                mode = 2;
                allow = 1;
            }
            action ClassifyRefuse when (
                (phase == 0 || phase == 4) && section == 1 && published == 1
            ) {
                phase = 5;
                mode = 0;
                allow = 0;
            }

            // THE WRITER. The release commit carries P's code; the Buggy one is built
            // from the tip it read.
            action ClaimRead when (phase == 1) {
                phase = 2;
                read_seq = seq;
                n = tail + 1;
                release_code = if Buggy == 1 { main_code } else { 0 };
            }
            action ClaimElsewhere when (
                phase == 2 && seq > read_seq && section == 1 && allow == 0
            ) {
                phase = 5;
                elsewhere = 1;
            }
            action ClaimRetry when (
                phase == 2 && seq > read_seq && (section == 0 || allow == 1)
            ) {
                read_seq = seq;
                n = if Buggy == 1 { n } else { tail + 1 };
            }
            action ClaimLand when (phase == 2 && seq == read_seq && MaxSeq > seq) {
                phase = 3;
                prior = tail;
                tail = n;
                last = n;
                landings = landings + 1;
                main_lines = if Buggy == 1 { 2 } else { main_lines + 1 };
                main_code = if Buggy == 1 { release_code } else { main_code };
                section = 1;
                seq = seq + 1;
                after_publish = after_publish + published;
            }
            action Die when (phase == 3) {
                phase = 4;
                mode = 0;
                allow = 0;
            }
            action Publish when (phase == 3) {
                phase = 0;
                published = 1;
                mode = 0;
                allow = 0;
            }

            invariant ReleaseCarriesOnlyThePublishedCode: release_code == 0;
            invariant MainKeepsEveryPeerCommit: main_code == peers;
            invariant MainKeepsEveryLedgerLine:
                main_lines == 1 + rivals + foreign + landings;
            invariant BuildsStrictlyIncrease: landings == 0 || last > prior;
            invariant ClaimedUnpublishedIsNeverFresh:
                section == 0 || foreign == 1 || mode == 2 || phase == 0 || phase > 2;
            invariant OwnSectionIsNeverCutElsewhere: elsewhere == 0 || foreign == 1;
            invariant NoClaimAfterPublish: after_publish == 0;
            invariant ClaimStateBounds:
                peers <= MaxPeers && rivals <= MaxRivals && foreign <= 1 &&
                seq <= MaxSeq && published <= 1 && main_code <= MaxPeers &&
                main_lines <= MaxSeq + 1 && tail <= MaxSeq + 1 && section <= 1 &&
                phase <= 5 && mode <= 2 && allow <= 1 && read_seq <= MaxSeq &&
                n <= MaxSeq + 2 && release_code <= MaxPeers && landings <= MaxSeq &&
                prior <= MaxSeq + 1 && last <= MaxSeq + 2 && elsewhere <= 1 &&
                after_publish <= MaxSeq;
        }
    }
}
