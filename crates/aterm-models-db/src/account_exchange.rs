// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Metadata admission by the real inventory constructor and completed-round
/// getter. Authentication is the transport's input boundary; this model checks
/// that missing or mismatched authenticated metadata cannot publish a witness.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_exchange_admission_model() -> Model {
    aterm_spec::ty_model! {
        DbAccountExchangeAdmission {
            const Buggy = 0;
            var scenario = 0; var constructed = 0; var issued = 0; var original = 0;
            action Valid when (scenario == 0) { scenario = 1; constructed = 1; issued = if Buggy == 1 {0} else {1}; original = if Buggy == 35 {0} else {1}; }
            action DifferentHash when (scenario == 0) { scenario = 2; constructed = if Buggy == 2 {1} else {0}; }
            action DifferentCount when (scenario == 0) { scenario = 3; constructed = if Buggy == 3 {1} else {0}; }
            action MissingLocal when (scenario == 0) { scenario = 4; constructed = if Buggy == 4 {1} else {0}; }
            action MissingPeer when (scenario == 0) { scenario = 5; constructed = if Buggy == 5 {1} else {0}; }
            action WrongScope when (scenario == 0) { scenario = 6; constructed = if Buggy == 6 {1} else {0}; }
            action LocalChanged when (scenario == 0) { scenario = 7; constructed = if Buggy == 7 {1} else {0}; }
            action PeerChanged when (scenario == 0) { scenario = 8; constructed = if Buggy == 8 {1} else {0}; }
            action ControlChanged when (scenario == 0) { scenario = 9; constructed = if Buggy == 9 {1} else {0}; }
            action FinalInventoryChanged when (scenario == 0) { scenario = 10; constructed = if Buggy == 10 {1} else {0}; }
            action FinalScopeChanged when (scenario == 0) { scenario = 11; constructed = if Buggy == 11 {1} else {0}; }
            action ZeroStamp when (scenario == 0) { scenario = 12; constructed = if Buggy == 12 {1} else {0}; }
            action BackwardsSample when (scenario == 0) { scenario = 13; constructed = if Buggy == 13 {1} else {0}; }
            action Deferred when (scenario == 0) { scenario = 14; constructed = if Buggy == 14 {1} else {0}; }
            action NoActor when (scenario == 0) { scenario = 15; constructed = 1; original = 1; issued = if Buggy == 15 {1} else {0}; }
            action ForeignActor when (scenario == 0) { scenario = 16; constructed = 1; original = 1; issued = if Buggy == 16 {1} else {0}; }
            action NotStarted when (scenario == 0) { scenario = 17; constructed = 1; original = 1; issued = if Buggy == 17 {1} else {0}; }
            action Cancelled when (scenario == 0) { scenario = 18; constructed = 1; original = 1; issued = if Buggy == 18 {1} else {0}; }
            action ControlError when (scenario == 0) { scenario = 19; constructed = 1; original = 1; issued = if Buggy == 19 {1} else {0}; }
            action MembershipError when (scenario == 0) { scenario = 20; constructed = 1; original = 1; issued = if Buggy == 20 {1} else {0}; }
            action MissingApplication when (scenario == 0) { scenario = 21; constructed = 1; original = 1; issued = if Buggy == 21 {1} else {0}; }
            action ApplicationError when (scenario == 0) { scenario = 22; constructed = 1; original = 1; issued = if Buggy == 22 {1} else {0}; }
            action Interrupted when (scenario == 0) { scenario = 23; constructed = 1; original = 1; issued = if Buggy == 23 {1} else {0}; }
            action InventoryDeferral when (scenario == 0) { scenario = 24; constructed = 1; original = 1; issued = if Buggy == 24 {1} else {0}; }
            action BatchDeferral when (scenario == 0) { scenario = 25; constructed = 1; original = 1; issued = if Buggy == 25 {1} else {0}; }
            action HandoffError when (scenario == 0) { scenario = 26; constructed = 1; original = 1; issued = if Buggy == 26 {1} else {0}; }
            action MissingWitness when (scenario == 0) { scenario = 27; constructed = 1; original = 1; issued = if Buggy == 27 {1} else {0}; }
            action CompletedScopeChanged when (scenario == 0) { scenario = 28; constructed = 1; original = 1; issued = if Buggy == 28 {1} else {0}; }
            action CompletedInventoryChanged when (scenario == 0) { scenario = 29; constructed = 1; original = 1; issued = if Buggy == 29 {1} else {0}; }
            action CompletedDeferred when (scenario == 0) { scenario = 30; constructed = 1; original = 1; issued = if Buggy == 30 {1} else {0}; }
            action OfferedSubset when (scenario == 0) { scenario = 31; constructed = 1; issued = if Buggy == 31 {0} else {1}; original = 1; }
            action LaterWallRollback when (scenario == 0) { scenario = 32; constructed = 1; issued = if Buggy == 32 {0} else {1}; original = 1; }
            action CompletedPeerInventoryChanged when (scenario == 0) { scenario = 33; constructed = 1; original = 1; issued = if Buggy == 33 {1} else {0}; }
            action CompletedPeerInventoryMissing when (scenario == 0) { scenario = 34; constructed = 1; original = 1; issued = if Buggy == 34 {1} else {0}; }
            invariant Progress: scenario == 0 || (scenario > 1 && 31 > scenario) || scenario > 32
                || (constructed == 1 && issued == 1);
            invariant ExactAdmission: scenario == 0 || scenario == 1 || scenario == 31 || scenario == 32
                || (issued == 0 && (scenario > 14 || constructed == 0));
            invariant OriginalContext: constructed == 0 || original == 1;
            bound ScenarioRange: scenario <= 34;
        }
    }
}

/// Original sample ordering and immutable historical metadata after producer
/// admission. Payload tokens stand for the complete retained observation; this
/// model establishes no current service state or global inventory equality.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_exchange_retention_model() -> Model {
    aterm_spec::ty_model! {
        DbAccountExchangeRetention {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var payload = 0;
            var stamp = 0; var sample = 0;
            action First when (scenario == 0) {
                scenario = 1;
                accepted = if Buggy == 1 {0} else {1};
                payload = if Buggy == 1 {0} else {1};
                stamp = if Buggy == 1 {0} else if Buggy == 8 {3} else {1};
                sample = if Buggy == 1 {0} else {2};
            }
            action Newer when (scenario == 0) {
                scenario = 2;
                accepted = if Buggy == 2 {0} else {1};
                payload = if Buggy == 2 {1} else {2};
                stamp = if Buggy == 2 {1} else {2};
                sample = if Buggy == 2 {2} else {3};
            }
            action SameSample when (scenario == 0) {
                scenario = 3;
                accepted = if Buggy == 3 {1} else {0};
                payload = if Buggy == 3 {2} else {1};
                stamp = if Buggy == 3 {2} else {1}; sample = 2;
            }
            action OlderSample when (scenario == 0) {
                scenario = 4;
                accepted = if Buggy == 4 {1} else {0};
                payload = if Buggy == 4 {2} else {1};
                stamp = if Buggy == 4 {2} else {1};
                sample = if Buggy == 4 {1} else {2};
            }
            action Read when (scenario == 0) {
                scenario = 5; payload = 1;
                stamp = if Buggy == 5 {3} else {1}; sample = 2;
            }
            action HistoryRead when (scenario == 0) {
                scenario = 6;
                payload = if Buggy == 7 {2} else {1}; stamp = 1;
                sample = if Buggy == 6 {2} else {0};
            }
            action FirstAfterHistory when (scenario == 0) {
                scenario = 7;
                accepted = if Buggy == 9 {0} else {1};
                payload = if Buggy == 9 {1} else {2};
                stamp = if Buggy == 9 {1} else {2};
                sample = if Buggy == 9 {0} else {1};
            }
            invariant Progress: scenario == 0 || (scenario > 2 && 7 > scenario)
                || (accepted == 1 && payload == if scenario == 1 {1} else {2});
            invariant StrictSampleOrder: 3 > scenario || scenario > 4
                || (accepted == 0 && payload == 1 && stamp == 1 && sample == 2);
            invariant OriginalEvidence: (payload == 0 && stamp == 0)
                || (payload == 1 && stamp == 1) || (payload == 2 && stamp == 2);
            invariant HistoricalReads: 5 > scenario || scenario > 6
                || (payload == 1 && stamp == 1);
            invariant ReplayHasNoSample: 6 > scenario || scenario > 6 || sample == 0;
            bound ScenarioRange: scenario <= 7;
            bound SampleRange: sample <= 3;
        }
    }
}
