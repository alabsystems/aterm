// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A candidate must contain the retained causal frontier in the same network.
/// This model covers the selection predicate, not its durable publication order.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retained_frontier_model() -> Model {
    aterm_spec::ty_model! {
        VaultRetainedFrontier {
            const Buggy = 0;
            var same = 1; var complete = 1; var phase = 0; var admitted = 0;
            action OtherNetwork when (phase == 0 && same == 1) { same = 0; }
            action LoseAncestor when (phase == 0 && complete == 1) { complete = 0; }
            action Admit when (phase == 0) {
                phase = 1;
                admitted = if Buggy == 3 {0}
                    else if (same == 1 || Buggy == 1) && (complete == 1 || Buggy == 2) {1}
                    else {0};
            }
            invariant SameNetwork: admitted == 0 || same == 1;
            invariant RetainsAncestors: admitted == 0 || complete == 1;
            invariant CompleteAvailable: phase == 0 || same == 0 || complete == 0 || admitted == 1;
            bound ReadBox: same <= 1 && complete <= 1 && phase <= 1 && admitted <= 1;
        }
    }
}

/// Two causally ordered versions, independently durable data and receipt, and loss of the
/// receipt file. A shared reader never acknowledges data or repairs the journal.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retained_commit_model() -> Model {
    aterm_spec::ty_model! {
        VaultRetainedCommit {
            const Buggy = 0;
            var raw = 0; var floor = 0; var known = 0;
            var damaged = 0; var read = 0; var seen = 2;
            action Publish when (raw == 0) { raw = 1; read = 0; seen = 2; }
            action Acknowledge when (raw == 1 && floor == 0 && damaged == 0) {
                floor = 1; known = 1; read = 0; seen = 2;
            }
            action Rollback when (raw == 1) {
                raw = 0; read = 0; seen = 2;
                floor = if Buggy == 1 {0} else {floor};
            }
            action Damage when (damaged == 0) { damaged = 1; read = 0; seen = 2; }
            action Read when (read == 0) {
                read = 1;
                seen = if Buggy == 5 {2}
                    else if Buggy == 3 {if floor > raw {2} else {floor}}
                    else if damaged == 1 {2}
                    else if Buggy == 2 || Buggy == 4 {raw}
                    else if floor > raw {2}
                    else {floor};
            }
            invariant NoForgottenAcknowledgement: known <= floor;
            invariant OnlyAcknowledgedView: seen == 2 || seen == floor;
            invariant DamagedReceiptWithholds: seen == 2 || damaged == 0;
            invariant CompleteReadAvailable: read == 0 || damaged == 1 || floor > raw || seen == floor;
            bound CommitBox: raw <= 1 && floor <= 1 && known <= 1 && damaged <= 1 && read <= 1 && seen <= 2;
        }
    }
}

/// A decoded receipt snapshot is reusable only for the exact, usable writer checkpoint.
/// Opening and validating the physical journal remain environment obligations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retained_replay_model() -> Model {
    aterm_spec::ty_model! {
        VaultRetainedReplay {
            const Buggy = 0;
            var directory = 1; var prefix = 1; var usable = 1; var cached = 1;
            var phase = 0; var reused = 0;
            action OtherDirectory when (phase == 0 && directory == 1) { directory = 0; }
            action Advance when (phase == 0 && prefix == 1) { prefix = 0; }
            action LoseCustody when (phase == 0 && usable == 1) { usable = 0; }
            action Evict when (phase == 0 && cached == 1) { cached = 0; }
            action Replay when (phase == 0) {
                phase = 1;
                reused = if Buggy == 5 {0}
                    else if (directory == 1 || Buggy == 1)
                        && (prefix == 1 || Buggy == 2)
                        && (usable == 1 || Buggy == 3)
                        && (cached == 1 || Buggy == 4) {1}
                    else {0};
            }
            invariant SameDirectory: reused == 0 || directory == 1;
            invariant ExactPrefix: reused == 0 || prefix == 1;
            invariant CurrentCustody: reused == 0 || usable == 1;
            invariant CachedHistory: reused == 0 || cached == 1;
            invariant MatchingReuse: phase == 0 || directory == 0 || prefix == 0
                || usable == 0 || cached == 0 || reused == 1;
            bound ReplayBox: directory <= 1 && prefix <= 1 && usable <= 1
                && cached <= 1 && phase <= 1 && reused <= 1;
        }
    }
}
