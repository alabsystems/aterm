// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The native_update_* scan / admission / worker family — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// Order-independent update-channel authority selection.
///
/// GitHub does not document List Releases row order. The bounded catalog therefore
/// presents canonical `v0.8`, `v0.9`, and `v0.10` candidates in every possible action
/// order plus an optional strictly-lower numeric multi-part legacy tag. Metadata
/// arbitration must enumerate the complete catalog and retain the numeric-vector
/// maximum before downloading anything. Lower legacy tags are migration-compatible;
/// a same/newer noncanonical maximum refuses. The exact canonical selected manifest is
/// then fetched once (plus one detached signature iff the channel is pinned); an
/// unreadable older asset is metadata only and can neither add a fetch nor poison the
/// authority.
/// Failure, rejection, missing/ambiguous signature, or a missing/ambiguous/
/// noncanonical DMG identity at the authoritative release is terminal for that
/// check—there is no fallback to an older release.
///
/// Nonnumeric/duplicate-order metadata refuses before transport. `Buggy=1` exposes
/// independent row-order overwrites for legacy/v0.8/v0.9, selection before
/// enumeration, acceptance of a noncanonical maximum, an older 503 fetch after the
/// authority, and fallback after authoritative fetch, parse, or signature failure.
/// These controls keep permutation invariance, numeric `9 < 10`, migration
/// compatibility, the one-fetch budget, and fail-closed authority non-vacuous.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_channel_scan_model() -> Model {
    crate::ty_model! {
        NativeUpdateChannelScan {
            const Buggy = 0;
            const MaxMinor = 10;
            // phase: 0 Catalog, 1 AuthoritySelected, 2 AuthorityVerified,
            // 3 Refused, 4 Accepted.
            var phase = 0;
            var seen_minor_8 = 0;
            var seen_minor_9 = 0;
            var seen_minor_10 = 0;
            var lower_legacy_seen = 0;
            var noncanonical_maximum = 0;
            var max_minor = 0;
            var selected_minor = 0;
            var metadata_complete = 0;
            var metadata_error = 0;
            var signatures_configured = 0;
            // `signature_policy_ready=1` means either unsigned policy or exactly
            // one authoritative signature under a pinned policy.
            var signature_policy_ready = 1;
            var signature_ambiguous = 0;
            var manifest_fetch_count = 0;
            var signature_fetch_count = 0;
            var fetched_minor = 0;
            var older_unreadable = 0;
            var older_manifest_fetch_count = 0;
            var authoritative_fetch_failed = 0;
            var authoritative_manifest_rejected = 0;
            var deferred = 0;
            var accepted = 0;
            var early_selection_bypassed = 0;
            var noncanonical_bypassed = 0;
            var fallback_bypassed = 0;

            action ConfigureSignatures when (
                phase == 0 && signatures_configured == 0
            ) {
                signatures_configured = 1;
            }
            // A numeric multi-part migration tag (abstracted as v0.5.x) is
            // orderable and harmless while it remains strictly below v0.8+.
            action ObserveLowerLegacy when (
                phase == 0 && lower_legacy_seen == 0
            ) {
                lower_legacy_seen = 1;
                max_minor = if max_minor > 5 {
                    max_minor
                } else {
                    5
                };
            }
            action ObserveLowerLegacyByRowOrder when (
                Buggy == 1 && phase == 0 && lower_legacy_seen == 0
            ) {
                lower_legacy_seen = 1;
                max_minor = 5;
            }
            action ObserveMinor8 when (phase == 0 && seen_minor_8 == 0) {
                seen_minor_8 = 1;
                max_minor = if max_minor > 8 {
                    max_minor
                } else {
                    8
                };
            }
            action ObserveMinor8ByRowOrder when (
                Buggy == 1 && phase == 0 && seen_minor_8 == 0
            ) {
                seen_minor_8 = 1;
                max_minor = 8;
            }
            action ObserveMinor9 when (phase == 0 && seen_minor_9 == 0) {
                seen_minor_9 = 1;
                max_minor = if max_minor > 9 {
                    max_minor
                } else {
                    9
                };
            }
            action ObserveMinor9ByRowOrder when (
                Buggy == 1 && phase == 0 && seen_minor_9 == 0
            ) {
                seen_minor_9 = 1;
                max_minor = 9;
            }
            action ObserveMinor10 when (phase == 0 && seen_minor_10 == 0) {
                seen_minor_10 = 1;
                max_minor = 10;
            }
            action ObserveMalformedCandidate when (
                phase == 0 && metadata_error == 0
            ) {
                metadata_error = 1;
            }
            action ObserveDuplicateCanonicalCandidate when (
                phase == 0 && metadata_error == 0
            ) {
                metadata_error = 1;
            }
            // v0.10.1 (same-prefix newer) wins numeric vector order over v0.10,
            // but cannot be a canonical channel authority.
            action ObserveNewerNoncanonical when (
                phase == 0 && noncanonical_maximum == 0
            ) {
                noncanonical_maximum = 1;
            }
            action CompleteMetadataArbitration when (
                phase == 0 && metadata_error == 0 && seen_minor_8 == 1 &&
                seen_minor_9 == 1 && seen_minor_10 == 1 &&
                noncanonical_maximum == 0
            ) {
                phase = 1;
                selected_minor = max_minor;
                metadata_complete = 1;
            }
            action RefuseMetadata when (phase == 0 && metadata_error == 1) {
                phase = 3;
                metadata_complete = 1;
                deferred = 1;
            }
            action RefuseNoncanonicalAuthority when (
                phase == 0 && metadata_error == 0 && seen_minor_8 == 1 &&
                seen_minor_9 == 1 && seen_minor_10 == 1 &&
                noncanonical_maximum == 1
            ) {
                phase = 3;
                metadata_complete = 1;
                deferred = 1;
            }
            action SelectBeforeCatalogComplete when (
                Buggy == 1 && phase == 0 && metadata_error == 0 &&
                max_minor > 0 &&
                seen_minor_8 + seen_minor_9 + seen_minor_10 <= 2
            ) {
                phase = 1;
                selected_minor = max_minor;
                early_selection_bypassed = 1;
            }
            action AcceptNoncanonicalAuthority when (
                Buggy == 1 && phase == 0 && metadata_error == 0 &&
                seen_minor_8 == 1 && seen_minor_9 == 1 &&
                seen_minor_10 == 1 && noncanonical_maximum == 1
            ) {
                phase = 1;
                selected_minor = max_minor;
                metadata_complete = 1;
                noncanonical_bypassed = 1;
            }
            action ObserveMissingAuthoritativeSignature when (
                phase == 1 && signatures_configured == 1 &&
                signature_policy_ready == 1
            ) {
                signature_policy_ready = 0;
            }
            action ObserveAmbiguousAuthoritativeSignature when (
                phase == 1 && signatures_configured == 1 &&
                signature_policy_ready == 1
            ) {
                signature_policy_ready = 0;
                signature_ambiguous = 1;
            }
            action RefuseSignaturePolicy when (
                phase == 1 && signatures_configured == 1 &&
                signature_policy_ready == 0
            ) {
                phase = 3;
                deferred = 1;
            }
            // The old 503 is a property of catalog metadata, not a transport call.
            action ExposeOlderUnreadable when (
                phase == 1 && older_unreadable == 0
            ) {
                older_unreadable = 1;
            }
            action FetchAuthoritativeVerified when (
                phase == 1 && metadata_complete == 1 &&
                selected_minor == max_minor && signature_policy_ready == 1 &&
                manifest_fetch_count == 0
            ) {
                phase = 2;
                manifest_fetch_count = 1;
                signature_fetch_count = signatures_configured;
                fetched_minor = selected_minor;
            }
            action FetchAuthoritativeUnreadable when (
                phase == 1 && metadata_complete == 1 &&
                selected_minor == max_minor && signature_policy_ready == 1 &&
                manifest_fetch_count == 0
            ) {
                phase = 3;
                manifest_fetch_count = 1;
                fetched_minor = selected_minor;
                authoritative_fetch_failed = 1;
                deferred = 1;
            }
            action FetchAuthoritativeSignatureUnreadable when (
                phase == 1 && metadata_complete == 1 &&
                selected_minor == max_minor && signatures_configured == 1 &&
                signature_policy_ready == 1 && manifest_fetch_count == 0
            ) {
                phase = 3;
                manifest_fetch_count = 1;
                signature_fetch_count = 1;
                fetched_minor = selected_minor;
                authoritative_fetch_failed = 1;
                deferred = 1;
            }
            // Includes parse/version/signature rejection and a signed manifest
            // whose canonical DMG name does not resolve to exactly one asset.
            action RejectAuthoritativeManifest when (
                phase == 1 && metadata_complete == 1 &&
                selected_minor == max_minor && signature_policy_ready == 1 &&
                manifest_fetch_count == 0
            ) {
                phase = 3;
                manifest_fetch_count = 1;
                signature_fetch_count = signatures_configured;
                fetched_minor = selected_minor;
                authoritative_manifest_rejected = 1;
                deferred = 1;
            }
            action FinalizeAccepted when (phase == 2) {
                phase = 4;
                accepted = 1;
            }
            action FetchOlderUnreadable when (
                Buggy == 1 && phase == 2 && older_unreadable == 1 &&
                manifest_fetch_count == 1
            ) {
                phase = 3;
                manifest_fetch_count = 2;
                older_manifest_fetch_count = 1;
                authoritative_fetch_failed = 1;
                deferred = 1;
            }
            action FallbackAfterFetchFailure when (
                Buggy == 1 && phase == 3 && authoritative_fetch_failed == 1
            ) {
                phase = 4;
                manifest_fetch_count = 2;
                fetched_minor = 9;
                deferred = 0;
                accepted = 1;
                fallback_bypassed = 1;
            }
            action FallbackAfterManifestReject when (
                Buggy == 1 && phase == 3 &&
                authoritative_manifest_rejected == 1
            ) {
                phase = 4;
                manifest_fetch_count = 2;
                fetched_minor = 9;
                deferred = 0;
                accepted = 1;
                fallback_bypassed = 1;
            }
            action FallbackAfterSignatureRefusal when (
                Buggy == 1 && phase == 3 && signatures_configured == 1 &&
                signature_policy_ready == 0 && manifest_fetch_count == 0
            ) {
                phase = 4;
                manifest_fetch_count = 1;
                fetched_minor = 9;
                deferred = 0;
                accepted = 1;
                fallback_bypassed = 1;
            }

            invariant CatalogMaximumIsNumericAndOrderIndependent:
                if seen_minor_10 == 1 {
                    max_minor == 10
                } else if seen_minor_9 == 1 {
                    max_minor == 9
                } else if seen_minor_8 == 1 {
                    max_minor == 8
                } else if lower_legacy_seen == 1 {
                    max_minor == 5
                } else {
                    max_minor == 0
                };
            invariant SelectionWaitsForCompleteCatalog:
                if selected_minor > 0 {
                    metadata_complete == 1 && metadata_error == 0 &&
                    seen_minor_8 + seen_minor_9 + seen_minor_10 == 3
                } else {
                    selected_minor == 0
                };
            invariant SelectedAuthorityIsNumericMaximum:
                if selected_minor > 0 {
                    selected_minor == max_minor && selected_minor == 10
                } else {
                    selected_minor == 0
                };
            invariant MetadataFailureFetchesNothing:
                if metadata_error == 1 {
                    selected_minor == 0 && manifest_fetch_count == 0 &&
                    signature_fetch_count == 0 && accepted == 0
                } else {
                    metadata_error == 0
                };
            invariant NoncanonicalMaximumCannotSelect:
                if noncanonical_maximum == 1 {
                    selected_minor == 0 && manifest_fetch_count == 0 &&
                    accepted == 0
                } else {
                    noncanonical_maximum == 0
                };
            invariant ManifestFetchBudgetIsOne:
                manifest_fetch_count <= 1;
            invariant FetchTargetsSelectedAuthority:
                if manifest_fetch_count == 1 {
                    fetched_minor == selected_minor
                } else if manifest_fetch_count == 0 {
                    fetched_minor == 0
                } else {
                    fetched_minor <= MaxMinor
                };
            invariant VerifiedFetchHonorsSignaturePolicy:
                if phase == 2 {
                    signature_policy_ready == 1 &&
                    signature_fetch_count == signatures_configured
                } else {
                    signature_fetch_count <= signatures_configured
                };
            invariant OlderUnreadableIsNeverFetched:
                older_manifest_fetch_count == 0;
            invariant AuthorityFailureNeverFallsBack:
                fallback_bypassed == 0;
            invariant EnumerationCannotBeBypassed:
                early_selection_bypassed == 0;
            invariant NoncanonicalAuthorityCannotBeBypassed:
                noncanonical_bypassed == 0;
            invariant AcceptedUsesAuthoritativeRelease:
                if phase == 4 {
                    accepted == 1 && selected_minor == 10 &&
                    fetched_minor == 10 && manifest_fetch_count == 1 &&
                    authoritative_fetch_failed == 0 &&
                    authoritative_manifest_rejected == 0 &&
                    signature_policy_ready == 1
                } else {
                    accepted == 0
                };
            invariant RefusalIsTerminalForThisCheck:
                if phase == 3 {
                    accepted == 0 && deferred == 1
                } else {
                    deferred == 0
                };
            invariant ScanBounds:
                phase <= 4 && seen_minor_8 <= 1 && seen_minor_9 <= 1 &&
                seen_minor_10 <= 1 && lower_legacy_seen <= 1 &&
                noncanonical_maximum <= 1 && max_minor <= MaxMinor &&
                selected_minor <= MaxMinor && metadata_complete <= 1 &&
                metadata_error <= 1 && signatures_configured <= 1 &&
                signature_policy_ready <= 1 && signature_ambiguous <= 1 &&
                manifest_fetch_count <= 2 && signature_fetch_count <= 1 &&
                fetched_minor <= MaxMinor && older_unreadable <= 1 &&
                older_manifest_fetch_count <= 1 &&
                authoritative_fetch_failed <= 1 &&
                authoritative_manifest_rejected <= 1 && deferred <= 1 &&
                accepted <= 1 && early_selection_bypassed <= 1 &&
                noncanonical_bypassed <= 1 && fallback_bypassed <= 1;
        }
    }
}

/// Admission and handoff policy for applying an already verified native update.
///
/// A foreground terminal job is not dirty native UI state: the seamless handoff
/// adopts its PTY into the replacement process.  It must therefore be admitted
/// when the handoff lane is available.  If that lane cannot engage, a foreground
/// job forbids the destructive cold-reexec fallback and leaves the staged update
/// retryable.  Dirty/pending native documents remain a real blocker.  The mutant
/// recreates the v0.53 regression by treating any foreground terminal job as a
/// close-preflight blocker even when seamless adoption is available.
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
                phase = if Buggy == 1 && foreground_job == 1 { 3 } else { 1 };
                decision = if Buggy == 1 && foreground_job == 1 { 3 } else { 1 };
                attempt_count = if attempt_count <= MaxAttempts - 1 {
                    attempt_count + 1
                } else {
                    attempt_count
                };
                retry_eligible = if Buggy == 1 && foreground_job == 1 { 1 } else { 0 };
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
            invariant ApplyAtMostOnce: reexec_count <= 1;
            invariant AttemptsBounded: attempt_count <= MaxAttempts;
        }
    }
}

/// Ordering contract for persistent automatic-update intent.
///
/// A durable-stage wake may race an already-active manual check. The wake arms
/// intent immediately; completion then imports the durable stage and makes that
/// retained intent eligible. A blocked or failed attempt returns to the same
/// retryable ready state. The mutant recreates the lost-wake regression by
/// dropping intent solely because the manual check is active.
///
/// Terminal idleness is a bounded PREFERENCE, not a precondition: activity
/// defers an attempt only while the grace window is open (`GraceWindowCloses`),
/// because the previous unbounded rule let a machine that is never quiet keep a
/// verified build staged forever. The mutant still attempts with neither quiet
/// nor a closed window, which
/// `AutomaticAttemptRequiresQuietOrClosedGraceWindow` rejects.
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
                intent = if Buggy == 1 { 0 } else { 1 };
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
                intent = if Buggy == 1 { 0 } else { 1 };
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
            // The bounded idle-preference window closes on a machine that keeps
            // producing activity. Quiet is PREFERRED, never required forever:
            // waiting on an unreachable idle sample is indistinguishable from
            // refusing to update, which is the regression this models.
            action GraceWindowCloses when (
                phase == 2 && staged == 1 && intent == 1 &&
                quiet == 0 && grace_expired == 0
            ) {
                grace_expired = 1;
            }
            action Attempt when (
                phase == 2 && staged == 1 && intent == 1 &&
                (Buggy == 1 || quiet == 1 || grace_expired == 1)
            ) {
                phase = 3;
                attempts = if attempts <= MaxAttempts - 1 {
                    attempts + 1
                } else {
                    attempts
                };
                last_unsuccessful = 0;
                parked = 1;
            }
            action AttemptDidNotReplace when (
                phase == 3 && staged == 1 && intent == 1 && accepted == 0
            ) {
                phase = 2;
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
            action AttemptAccepted when (
                phase == 3 && staged == 1 && intent == 1 && accepted == 0
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
            invariant AttemptRequiresImportedStage:
                if phase == 3 {
                    staged == 1 && attempts > 0 && accepted == 0
                } else {
                    if phase == 4 {
                        staged == 1 && attempts > 0 && accepted == 1
                    } else {
                        accepted == 0
                    }
                };
            invariant AutomaticAttemptRequiresQuietOrClosedGraceWindow:
                if parked == 1 {
                    quiet == 1 || grace_expired == 1
                } else {
                    quiet <= 1
                };
            invariant DeferralsBounded: deferrals <= MaxDeferrals;
            invariant AcceptedAtMostOnce: accepted <= 1;
            invariant AttemptsBounded: attempts <= MaxAttempts;
        }
    }
}

/// Liveness contract for automatic-update quiet admission after hidden PTY output.
///
/// A background tab can consume output and handle its wake without ever presenting;
/// its first-edge presentation-latency sample therefore remains armed. Admission is
/// governed by the independently aging latest-output clock, not that presentation
/// acknowledgement. Activity retries are also strictly future. The mutant recreates
/// the regression by gating on the permanent presentation sample and deriving its
/// retry from the already-expired output deadline.
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
                activity_quiet = if Buggy == 1 && presentation_stamp == 1 { 0 } else { 1 };
                phase = if Buggy == 1 && presentation_stamp == 1 { 2 } else { 3 };
                retry_deadline = if Buggy == 1 && presentation_stamp == 1 {
                    latest_output_tick + QuietTicks
                } else {
                    0
                };
            }
            action Attempt when (
                phase == 3 && activity_quiet == 1 && attempted == 0
            ) {
                phase = 4;
                attempted = 1;
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
                    phase == 4 && activity_quiet == 1 && wake_handled == 1
                } else {
                    phase <= 3
                };
            invariant HiddenSampleRemainsUnacknowledged:
                if phase > 0 { presentation_stamp == 1 } else { presentation_stamp == 0 };
            invariant Bounds:
                phase <= 4 && now_tick <= QuietTicks + 1 &&
                latest_output_tick <= 1 && presentation_stamp <= 1 &&
                wake_handled <= 1 && activity_quiet <= 1 &&
                retry_deadline <= QuietTicks + 1 && attempted <= 1;
        }
    }
}

/// Identity discipline for retrying a failed updater process replacement.
///
/// Every authorized attempt receives a fresh nonce. A failure may re-arm only
/// the exact currently active nonce; replaying the first failure after a retry
/// is live must be inert. The mutant accepts that stale abort and cancels the
/// newer authority.
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
            action ReplayOldAbort when (
                phase == 1 && live_authority == 1 && old_attempt_nonce > 0 &&
                active_attempt_nonce > old_attempt_nonce
            ) {
                phase = if Buggy == 1 { 0 } else { phase };
                active_attempt_nonce = if Buggy == 1 { 0 } else { active_attempt_nonce };
                live_authority = if Buggy == 1 { 0 } else { live_authority };
                wrong_abort = if Buggy == 1 { 1 } else { wrong_abort };
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
            invariant OneLiveAttemptAuthority: live_authority <= 1;
            invariant NonceBounded: attempt_nonce <= MaxNonce;
            invariant AbortsBounded: abort_count <= MaxAborts;
        }
    }
}

/// Property-level accounting model for the bounded native-update facts worker.
///
/// A nonblocking send that sees a full FIFO retains one coalesced purpose; every
/// worker dequeue emits a drain edge that gives that latch another chance. A
/// dispatch-time disconnect gets one restart attempt and then becomes an explicit
/// unavailable result, never an invisible dropped request or an event-loop hot
/// loop. This scopes disconnection to `try_send`/retry observation; arbitrary
/// worker death after an accepted message is a separate worker-supervision
/// concern. `filler` abstracts the shipping `try_send(Full)` boundary; it does
/// not count the independently active worker item, so this model makes no exact
/// FIFO-occupancy or processing-duration claim. The mutant drops either the
/// full-queue latch or its drain edge.
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
            // Observable side effects of an event-loop park with no retained
            // intent. Both must remain zero: on macOS a proxy clone installs
            // and wakes a CFRunLoop source, while an unavailable warning feeds
            // more log I/O back into the same idle turn.
            var idle_proxy_wakes = 0;
            var idle_warnings = 0;

            action ParkIdle when (
                live_intent == 0 && pending == 0 && queued_intent == 0 &&
                completion_ready == 0 && idle_proxy_wakes == 0 && idle_warnings == 0
            ) {
                idle_proxy_wakes = if Buggy == 1 { 1 } else { 0 };
                idle_warnings = if Buggy == 1 { 1 } else { 0 };
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
                pending = if Buggy == 1 { 0 } else { 1 };
            }
            action RequestApplyFull when (
                connected == 1 && filler == 1 && queued_intent == 0 &&
                live_intent == 0 && delivered == 0 && failed_explicitly == 0
            ) {
                live_intent = 1;
                purpose = 2;
                pending = if Buggy == 1 { 0 } else { 1 };
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
                drain_edge = if Buggy == 1 { 0 } else { 1 };
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
            invariant AbstractFifoBoundaryIsBinary: filler + queued_intent <= 1;
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

/// Reconciliation of the updater's durable status ledger with the RUNNING caller
/// and the canonical `Ready` marker.
///
/// A status ledger is historical evidence about the build that wrote it, never
/// authority to relabel the process that is currently executing. The returned
/// `current_build` therefore always comes from the caller. Likewise, a persisted
/// "staged" sentence is not a stage: when the canonical Ready marker is absent,
/// reconciliation must remove that claim. A mismatched writer build with no Ready
/// marker is normalized even when its prose is otherwise innocuous, preventing an
/// overlapping old/new process pair from publishing one another's stale outcome.
///
/// `PickStatusInputs` is the bounded environment projection for
/// `reconcile_status_outcome(running_build, checked_from_build, ready_present,
/// persisted)`. `Buggy=1` reproduces the two regression classes: trusting the
/// ledger's build instead of the caller and preserving an absent-Ready staged
/// claim/mismatched outcome.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_status_reconciliation_model() -> Model {
    let stale_without_ready = || {
        and_(
            eq(var("ready_present"), int(0)),
            or_(
                neq(var("ledger_build"), var("running_build")),
                eq(var("persisted_staged_claim"), int(1)),
            ),
        )
    };
    let settled = || eq(var("phase"), int(2));

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
            Action {
                name: "ReconcileStatus",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "reported_build",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            var("ledger_build"),
                            var("running_build"),
                        ),
                    },
                    Update {
                        var: "reported_staged_claim",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            var("persisted_staged_claim"),
                            if_(
                                eq(var("ready_present"), int(0)),
                                int(0),
                                var("persisted_staged_claim"),
                            ),
                        ),
                    },
                    Update {
                        var: "neutralized",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            int(0),
                            if_(stale_without_ready(), int(1), int(0)),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "CallerBuildIsAuthoritative",
                expr: if_(
                    settled(),
                    eq(var("reported_build"), var("running_build")),
                    eq(var("reported_build"), int(0)),
                ),
            },
            Invariant {
                name: "AbsentReadyCannotAdvertiseStage",
                expr: if_(
                    and_(settled(), eq(var("ready_present"), int(0))),
                    eq(var("reported_staged_claim"), int(0)),
                    le(var("reported_staged_claim"), int(1)),
                ),
            },
            Invariant {
                name: "MismatchedAbsentReadyIsNeutralized",
                expr: if_(
                    and_(settled(), stale_without_ready()),
                    eq(var("neutralized"), int(1)),
                    le(var("neutralized"), int(1)),
                ),
            },
            Invariant {
                name: "ReadyPreservesPersistedOutcome",
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
