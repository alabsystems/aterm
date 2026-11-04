// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Advisory recovery preserves consumed custody and retries only its request.
/// Progress assumes a later successful publication and independently issued grant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_recovery_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedRecovery {
            const Buggy = 0;
            var case = 0; var requests = 0; var accepted = 0; var unchanged = 1;
            action Unknown when (case == 0) { case = 1; requests = if Buggy == 1 {0} else {1}; accepted = 1; unchanged = if Buggy == 3 {0} else {1}; }
            action Pending when (case == 0) { case = 2; requests = if Buggy == 1 {0} else {1}; accepted = 1; unchanged = if Buggy == 3 {0} else {1}; }
            action Published when (case == 0) { case = 3; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action Held when (case == 0) { case = 4; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action NotStarted when (case == 0) { case = 5; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action ChangedBinding when (case == 0) { case = 6; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action ChangedDecision when (case == 0) { case = 7; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action Stopped when (case == 0) { case = 8; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action Unavailable when (case == 0) { case = 9; requests = if Buggy == 1 {0} else {1}; accepted = if Buggy == 5 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action Retry when (case == 9) { case = 10; requests = if Buggy == 4 {1} else {2}; accepted = 1; unchanged = if Buggy == 3 {0} else {1}; }
            action ChangedRoster when (case == 0) { case = 11; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action WrongIdentity when (case == 0) { case = 12; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action WrongConfiguration when (case == 0) { case = 13; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action WrongGeneration when (case == 0) { case = 14; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action BusySigner when (case == 0) { case = 15; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action AfterSigner when (case == 15) { case = 16; requests = if Buggy == 1 {0} else {1}; accepted = 1; unchanged = if Buggy == 3 {0} else {1}; }
            action Checkpoint when (case == 0) { case = 17; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action MissingCredential when (case == 0) { case = 18; requests = if Buggy == 1 {0} else {1}; accepted = 1; unchanged = if Buggy == 3 {0} else {1}; }
            action NewerSignout when (case == 0) { case = 19; requests = if Buggy == 1 {0} else {1}; accepted = 1; unchanged = if Buggy == 3 {0} else {1}; }
            action QuotaCheckpoint when (case == 0) { case = 20; requests = if Buggy == 2 {1} else {0}; accepted = if Buggy == 2 {1} else {0}; unchanged = if Buggy == 3 {0} else {1}; }
            action ReferencedCheckpoint when (case == 0) { case = 21; requests = if Buggy == 1 {0} else {1}; accepted = 1; unchanged = if Buggy == 3 {0} else {1}; }
            invariant EligibleRequest: case == 0 || (case > 2 && 9 > case) || (case > 9 && 16 > case) || case == 17 || case == 20 || requests == 1;
            invariant RefuseOtherAttempt: case == 0 || case == 1 || case == 2 || case == 9 || case == 10 || case == 16 || (case > 17 && 20 > case) || case > 20 || (requests == 0 && accepted == 0);
            invariant PreserveCustody: unchanged == 1;
            invariant RetryRequest: 10 > case || case > 10 || (requests == 2 && accepted == 1);
            invariant FailedPublication: 9 > case || case > 9 || accepted == 0;
        }
    }
}

/// Real discovery and one process-local scheduler survive uncertainty and reissue.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_progress_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedProgress {
            const Buggy = 0;
            var phase = 0; var calls = 0; var requests = 0; var generation = 1; var healthy = 0;
            action Fail when (phase == 0) { phase = 1; calls = 1; requests = if Buggy == 2 {0} else {1}; generation = 2; }
            action Retry when (phase == 1) { phase = 2; calls = if Buggy == 1 {2} else {1}; requests = 2; }
            action Reissue when (phase == 2) { phase = 3; calls = if Buggy == 1 {1} else {2}; generation = if Buggy == 3 {2} else {3}; healthy = if Buggy == 4 {0} else {1}; }
            action Healthy when (phase == 3) { phase = 4; calls = if Buggy == 1 {3} else {2}; }
            invariant ProviderEntries: phase == 0 || calls == if 3 > phase {1} else {2};
            invariant RecoveryRequests: requests == if 3 > phase {phase} else {2};
            invariant CurrentGeneration: phase == 0 || generation == if 3 > phase {2} else {3};
            invariant NewGrantProgress: healthy == if phase > 2 {1} else {0};
        }
    }
}

/// First custody of a listed row resumes only a proved unstarted attempt.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_adoption_progress_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedAdoptionProgress {
            const Buggy = 0;
            var phase = 0; var calls = 0; var outcome = 0; var same = 1;
            action Busy when (phase == 0) { phase = 1; calls = if Buggy == 1 {1} else {0}; }
            action Unstarted when (phase == 1) { phase = 2; calls = 1; outcome = 1; }
            action Cooldown when (phase == 2) { phase = 3; calls = if Buggy == 4 {2} else {1}; }
            action Resume when (phase == 3) { phase = 4; calls = 2; outcome = if Buggy == 2 {1} else {2}; same = if Buggy == 3 {0} else {1}; }
            action Healthy when (phase == 4) { phase = 5; calls = if Buggy == 1 {3} else {2}; }
            invariant ProviderEntries: calls == if 2 > phase {0} else if 4 > phase {1} else {2};
            invariant OutcomeProgress: outcome == if 2 > phase {0} else if 4 > phase {1} else {2};
            invariant OriginalAttempt: same == 1;
        }
    }
}

/// A rejected first login keeps its consumed identity and asks for new authority.
/// Local or root changes suppress even advisory requests; replies may be retried.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_adoption_recovery_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedAdoptionRecovery {
            const Buggy = 0;
            var case = 0; var calls = 0; var requests = 0; var accepted = 0;
            var published = 0; var retained = 1;
            action WrongPrincipal when (case == 0) { case = 1; calls = 1; requests = if Buggy == 1 {0} else {1}; accepted = 1; published = if Buggy == 3 {1} else {0}; retained = if Buggy == 4 {0} else {1}; }
            action Retry when (case == 1) { case = 2; calls = if Buggy == 2 {2} else {1}; requests = 2; accepted = 2; }
            action Reissue when (case == 2) { case = 3; calls = 2; published = if Buggy == 5 {0} else {1}; }
            action WrongEmail when (case == 0) { case = 4; calls = 1; requests = if Buggy == 1 {0} else {1}; accepted = 1; }
            action Stopped when (case == 0) { case = 5; calls = 1; requests = if Buggy == 6 {1} else {0}; }
            action Revoked when (case == 0) { case = 6; calls = 1; requests = if Buggy == 6 {1} else {0}; }
            action ChangedRoster when (case == 0) { case = 7; calls = 1; requests = if Buggy == 6 {1} else {0}; }
            action ChangedBinding when (case == 0) { case = 8; calls = 1; requests = if Buggy == 6 {1} else {0}; }
            action MissingProof when (case == 0) { case = 9; calls = 1; requests = 1; accepted = 1; }
            action Unavailable when (case == 0) { case = 10; calls = 1; requests = 1; accepted = if Buggy == 7 {1} else {0}; }
            action RetryUnavailable when (case == 10) { case = 11; requests = if Buggy == 8 {1} else {2}; accepted = 1; }
            action ObservationError when (case == 0) { case = 12; calls = 1; requests = if Buggy == 19 {1} else {0}; accepted = if Buggy == 19 {1} else {0}; }
            action Healthy when (case == 0) { case = 13; calls = 1; published = 1; }
            action UnknownReply when (case == 0) { case = 14; calls = 1; requests = if Buggy == 9 {0} else {1}; accepted = if Buggy == 9 {0} else {1}; }
            action UnknownCompleted when (case == 0) { case = 15; calls = 1; published = if Buggy == 10 {0} else {1}; }
            action PriorCheckpoint when (case == 0) { case = 16; calls = 1; requests = 1; accepted = 1; published = if Buggy == 11 {0} else {1}; }
            action ClaimedPending when (case == 0) { case = 17; calls = 1; requests = 1; accepted = 1; published = if Buggy == 12 {1} else {0}; }
            action ClaimedUnstarted when (case == 0) { case = 18; calls = if Buggy == 13 {2} else {1}; }
            action CodexCheckpoint when (case == 0) { case = 19; published = if Buggy == 14 {1} else {0}; retained = if Buggy == 14 {0} else {1}; }
            action KimiCheckpoint when (case == 0) { case = 20; published = if Buggy == 14 {1} else {0}; retained = if Buggy == 14 {0} else {1}; }
            action ClaimedBeforeBegin when (case == 0) { case = 21; retained = if Buggy == 15 {0} else {1}; }
            action RetainedPrincipal when (case == 0) { case = 22; calls = 1; requests = if Buggy == 16 {0} else {1}; accepted = if Buggy == 16 {0} else {1}; }
            action OtherPrincipal when (case == 0) { case = 23; calls = 1; requests = if Buggy == 16 {0} else {1}; accepted = if Buggy == 16 {0} else {1}; }
            action ZeroPrincipal when (case == 0) { case = 24; calls = 1; requests = if Buggy == 16 {0} else {1}; accepted = if Buggy == 16 {0} else {1}; }
            action StagingAlias when (case == 0) { case = 25; published = if Buggy == 17 {1} else {0}; retained = if Buggy == 17 {0} else {1}; }
            action PrincipalAlias when (case == 0) { case = 26; published = if Buggy == 17 {1} else {0}; retained = if Buggy == 17 {0} else {1}; }
            action SupersededCheckpoint when (case == 0) { case = 27; calls = 2; requests = 1; accepted = 1; published = if Buggy == 18 {0} else {1}; }
            invariant NoReplay: calls == if case == 0 || case == 19 || case == 20 || case == 21 || case == 25 || case == 26 {0} else if case == 3 || case == 27 {2} else {1};
            invariant RequestProgress: requests == if case == 2 || case == 3 || case == 11 {2} else if case == 1 || case == 4 || case == 9 || case == 10 || case == 14 || case == 16 || case == 17 || (case > 21 && 25 > case) || case == 27 {1} else {0};
            invariant AcceptedRequests: accepted == if case == 2 || case == 3 {2} else if case == 1 || case == 4 || case == 9 || case == 11 || case == 14 || case == 16 || case == 17 || (case > 21 && 25 > case) || case == 27 {1} else {0};
            invariant Publication: published == if case == 3 || case == 13 || case == 15 || case == 16 || case == 27 {1} else {0};
            invariant PreserveAttempt: retained == 1;
        }
    }
}

/// A retained Claude result publishes only under the same current root terms.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_checkpoint_authority_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedCheckpointAuthority {
            const Buggy = 0;
            var case = 0; var published = 0; var custody = 1;
            var kept = 1; var unchanged = 1;
            action Revoke when (case == 0) { case = 1; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Restore when (case == 1) { case = 2; published = if Buggy == 2 {0} else {1}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {1} else {0}; }
            action Repeat when (case == 2) { case = 3; published = if Buggy == 3 {2} else {1}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {1} else {0}; }
            action Decision when (case == 0) { case = 4; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Configuration when (case == 0) { case = 5; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Generation when (case == 0) { case = 6; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Identity when (case == 0) { case = 7; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Source when (case == 0) { case = 8; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Principal when (case == 0) { case = 9; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action ChangedDuringCheck when (case == 0) { case = 10; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Disabled when (case == 0) { case = 11; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Unavailable when (case == 0) { case = 12; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            action Offline when (case == 0) { case = 13; published = if Buggy == 2 {0} else {1}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {1} else {0}; }
            action StagingAlias when (case == 0) { case = 14; published = if Buggy == 1 {1} else {0}; custody = if Buggy == 5 {0} else {1}; kept = if Buggy == 4 {0} else {1}; unchanged = if Buggy == 6 {0} else {1}; }
            invariant AuthorizedPublication: published == if case == 2 || case == 3 || case == 13 {1} else {0};
            invariant AtomicCustody: custody == 1;
            invariant PreserveEvidence: kept == 1;
            invariant RefusalIsReadOnly: unchanged == if case == 2 || case == 3 || case == 13 {0} else {1};
        }
    }
}

/// A lost staging directory never replays an attempt or silently prevents recovery.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_staging_recovery_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedStagingRecovery {
            const Buggy = 0;
            var case = 0; var calls = 1; var requests = 0; var accepted = 0;
            var published = 0; var unchanged = 1; var kept = 1;
            action Missing when (case == 0) { case = 1; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 1 {0} else {1}; accepted = 1; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action Retry when (case == 1) { case = 2; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 1 {0} else {2}; accepted = 2; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action Reissue when (case == 2) { case = 3; calls = if Buggy == 2 {3} else {2}; requests = if Buggy == 1 {0} else {2}; accepted = 2; published = if Buggy == 6 {0} else {1}; unchanged = if Buggy == 5 {1} else {0}; kept = if Buggy == 9 {0} else {1}; }
            action Unstarted when (case == 0) { case = 4; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 1 {0} else {1}; accepted = 1; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action RetryUnstarted when (case == 4) { case = 5; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 1 {0} else {2}; accepted = 2; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action ReissueUnstarted when (case == 5) { case = 6; calls = if Buggy == 2 {3} else {2}; requests = if Buggy == 1 {0} else {2}; accepted = 2; published = if Buggy == 6 {0} else {1}; unchanged = if Buggy == 5 {1} else {0}; kept = if Buggy == 9 {0} else {1}; }
            action Revoked when (case == 0) { case = 7; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 3 {1} else {0}; accepted = 0; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action Disabled when (case == 0) { case = 8; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 3 {1} else {0}; accepted = 0; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action RowChanged when (case == 0) { case = 9; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 3 {1} else {0}; accepted = 0; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action BindingChanged when (case == 0) { case = 10; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 3 {1} else {0}; accepted = 0; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action Claimed when (case == 0) { case = 11; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 3 {1} else {0}; accepted = 0; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action PublicationUnavailable when (case == 0) { case = 12; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 1 {0} else {1}; accepted = if Buggy == 7 {1} else {0}; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action RetryPublication when (case == 12) { case = 13; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 8 {1} else {2}; accepted = 1; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action FileEntry when (case == 0) { case = 14; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 3 {1} else {0}; accepted = 0; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action RevokedDuringCheck when (case == 0) { case = 15; calls = if Buggy == 2 {3} else {1}; requests = if Buggy == 3 {1} else {0}; accepted = 0; published = if Buggy == 4 {1} else {0}; unchanged = if Buggy == 5 {0} else {1}; kept = if Buggy == 9 {0} else {1}; }
            action UnstartedWithoutOpener when (case == 0) { case = 16; calls = 1; requests = if Buggy == 10 {0} else {1}; accepted = if Buggy == 10 {0} else {1}; }
            action MissingParent when (case == 0) { case = 17; calls = 1; requests = if Buggy == 11 {0} else {1}; accepted = if Buggy == 11 {0} else {1}; }
            invariant ProviderEntries: calls == if case == 3 || case == 6 {2} else {1};
            invariant RecoveryProgress: requests == if case == 2 || case == 3 || case == 5 || case == 6 || case == 13 {2} else if case == 1 || case == 4 || case == 12 || case == 16 || case == 17 {1} else {0};
            invariant AcceptedRequests: accepted == if case == 2 || case == 3 || case == 5 || case == 6 {2} else if case == 1 || case == 4 || case == 13 || case == 16 || case == 17 {1} else {0};
            invariant AuthorizedPublication: published == if case == 3 || case == 6 {1} else {0};
            invariant PreserveCustody: unchanged == if case == 3 || case == 6 {0} else {1};
            invariant PreserveEvidence: kept == 1;
        }
    }
}
