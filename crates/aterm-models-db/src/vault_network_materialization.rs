// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Network-body provenance and full local readiness after the existing signed
/// envelope verifier. A recorded event is historical; request start order and
/// wall-clock order do not select the last successful local commit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn vault_network_materialization_admission_model() -> Model {
    aterm_spec::ty_model! {
        DbVaultNetworkMaterializationAdmission {
            const Buggy = 0;
            var scenario = 0; var recorded = 0; var retained = 0; var original = 0;
            action ConfiguredBody when (scenario == 0) { scenario = 1; recorded = if Buggy == 1 {0} else {1}; retained = if Buggy == 1 {1} else {2}; original = if Buggy == 27 {0} else {1}; }
            action PeerBody when (scenario == 0) { scenario = 2; recorded = if Buggy == 2 {0} else {1}; retained = if Buggy == 2 {1} else {2}; original = 1; }
            action FreshSameVersion when (scenario == 0) { scenario = 3; recorded = if Buggy == 3 {0} else {1}; retained = if Buggy == 3 {1} else {2}; original = 1; }
            action EarlierStartLaterCommit when (scenario == 0) { scenario = 4; recorded = if Buggy == 4 {0} else {1}; retained = if Buggy == 4 {1} else {2}; original = 1; }
            action RawBody when (scenario == 0) { scenario = 5; recorded = if Buggy == 5 {1} else {0}; retained = if Buggy == 26 {0} else {1}; }
            action NoNewer when (scenario == 0) { scenario = 6; recorded = if Buggy == 6 {1} else {0}; retained = 1; }
            action Missing when (scenario == 0) { scenario = 7; recorded = if Buggy == 7 {1} else {0}; retained = 1; }
            action Path when (scenario == 0) { scenario = 8; recorded = if Buggy == 8 {1} else {0}; retained = 1; }
            action CachedSubstitute when (scenario == 0) { scenario = 9; recorded = if Buggy == 9 {1} else {0}; retained = 1; }
            action ChangedConfiguration when (scenario == 0) { scenario = 10; recorded = if Buggy == 10 {1} else {0}; retained = 1; }
            action ChangedRecipient when (scenario == 0) { scenario = 11; recorded = if Buggy == 11 {1} else {0}; retained = 1; }
            action UnboundSource when (scenario == 0) { scenario = 12; recorded = if Buggy == 12 {1} else {0}; retained = 1; }
            action WrongSequence when (scenario == 0) { scenario = 13; recorded = if Buggy == 13 {1} else {0}; retained = 1; }
            action WrongEnvelopeDigest when (scenario == 0) { scenario = 14; recorded = if Buggy == 14 {1} else {0}; retained = 1; }
            action FailedApply when (scenario == 0) { scenario = 15; recorded = if Buggy == 15 {1} else {0}; retained = 1; }
            action PendingApply when (scenario == 0) { scenario = 16; recorded = if Buggy == 16 {1} else {0}; retained = 1; }
            action NoOwnership when (scenario == 0) { scenario = 17; recorded = if Buggy == 17 {1} else {0}; retained = 1; }
            action Revoked when (scenario == 0) { scenario = 18; recorded = if Buggy == 18 {1} else {0}; retained = 1; }
            action CacheMissing when (scenario == 0) { scenario = 19; recorded = if Buggy == 19 {1} else {0}; retained = 1; }
            action FileMissing when (scenario == 0) { scenario = 20; recorded = if Buggy == 20 {1} else {0}; retained = 1; }
            action FileChanged when (scenario == 0) { scenario = 21; recorded = if Buggy == 21 {1} else {0}; retained = 1; }
            action MissingIdentity when (scenario == 0) { scenario = 22; recorded = if Buggy == 22 {1} else {0}; retained = 1; }
            action PlainSource when (scenario == 0) { scenario = 23; recorded = if Buggy == 23 {1} else {0}; retained = 1; }
            action UnknownMembership when (scenario == 0) { scenario = 24; recorded = if Buggy == 24 {1} else {0}; retained = 1; }
            action ChangedRequestedContext when (scenario == 0) { scenario = 25; recorded = if Buggy == 25 {1} else {0}; retained = 1; }
            invariant QualifiedProgress: scenario == 0 || scenario > 4 || (recorded == 1 && retained == 2);
            invariant ExactAdmission: 5 > scenario || recorded == 0;
            invariant OriginalEvent: recorded == 0 || original == 1;
            invariant RejectionKeepsHistory: 5 > scenario || retained == 1;
            bound ScenarioRange: scenario <= 25;
        }
    }
}

/// Exact stored receipt context and existing local readiness are independent of
/// the original network admission. Reading or decoding an old receipt cannot
/// supply a new network sample or a broader service claim.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn vault_network_materialization_projection_model() -> Model {
    aterm_spec::ty_model! {
        DbVaultNetworkMaterializationProjection {
            const Buggy = 0;
            var scenario = 0; var shown = 0; var original = 0;
            action Matching when (scenario == 0) { scenario = 1; shown = if Buggy == 1 {0} else {1}; original = if Buggy == 19 {0} else {1}; }
            action NoReceipt when (scenario == 0) { scenario = 2; shown = if Buggy == 2 {1} else {0}; }
            action ChangedSource when (scenario == 0) { scenario = 3; shown = if Buggy == 3 {1} else {0}; }
            action ChangedName when (scenario == 0) { scenario = 4; shown = if Buggy == 4 {1} else {0}; }
            action ChangedPin when (scenario == 0) { scenario = 5; shown = if Buggy == 5 {1} else {0}; }
            action ChangedFollowed when (scenario == 0) { scenario = 6; shown = if Buggy == 6 {1} else {0}; }
            action ChangedHost when (scenario == 0) { scenario = 7; shown = if Buggy == 7 {1} else {0}; }
            action ChangedRecipient when (scenario == 0) { scenario = 8; shown = if Buggy == 8 {1} else {0}; }
            action ChangedSequence when (scenario == 0) { scenario = 9; shown = if Buggy == 9 {1} else {0}; }
            action ChangedEnvelope when (scenario == 0) { scenario = 10; shown = if Buggy == 10 {1} else {0}; }
            action ChangedRecords when (scenario == 0) { scenario = 11; shown = if Buggy == 11 {1} else {0}; }
            action ChangedSourceIdentity when (scenario == 0) { scenario = 12; shown = if Buggy == 12 {1} else {0}; }
            action StateReload when (scenario == 0) { scenario = 13; shown = if Buggy == 13 {0} else {1}; original = 1; }
            action ReadAgain when (scenario == 0) { scenario = 14; shown = if Buggy == 14 {0} else {1}; original = if Buggy == 20 {0} else {1}; }
            action MissingIdentity when (scenario == 0) { scenario = 15; shown = if Buggy == 15 {1} else {0}; }
            action MalformedReceipt when (scenario == 0) { scenario = 16; shown = if Buggy == 16 {1} else {0}; }
            action LegacyState when (scenario == 0) { scenario = 17; shown = if Buggy == 17 {1} else {0}; }
            action Unready when (scenario == 0) { scenario = 18; shown = if Buggy == 18 {1} else {0}; }
            invariant QualifiedProgress: scenario == 0 || (scenario > 1 && 13 > scenario) || scenario > 14 || shown == 1;
            invariant ExactContext: scenario == 0 || scenario == 1 || scenario == 13 || scenario == 14 || shown == 0;
            invariant OriginalObservation: shown == 0 || original == 1;
            bound ScenarioRange: scenario <= 18;
        }
    }
}
