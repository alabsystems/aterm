// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two fixed-share writers retain a legacy cross-writer dependency before a
/// membership change. Each enabled append is compared with the native store.
/// Counts are acknowledged liabilities, including when a missing frontier
/// denies a current balance. This model assumes non-cloned writer custody.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resource_retirement_model() -> Model {
    aterm_spec::ty_model! {
        DbResourceRetirement {
            const Buggy = 0;
            var stage = 0; var mode = 0; var kind = 0;
            var accepted = 0; var observed = 0; var a = 0; var b = 0; var foreign = 0;
            action LegacyPeer when (stage == 0) {stage = 1; mode = 1; a = 2; b = 2;}
            action LogPeer when (stage == 0) {stage = 1; mode = 2; a = 2; b = 2;}
            action PeerCutoff when (stage == 0) {stage = 1; mode = 3; a = 2; b = 2;}
            action LogCutoff when (stage == 0) {stage = 1; mode = 4; a = 2; b = 2;}
            action LocalCutoff when (stage == 0) {stage = 1; mode = 5; a = 2; b = 2;}
            action MissingFrontier when (stage == 0) {stage = 1; mode = 6; a = 2; b = 2;}
            action Reserve when (stage == 1) {
                stage = 2; kind = 1;
                accepted = if mode <= 4 {if Buggy == 3 {0} else {1}} else {if Buggy == 4 {1} else {0}};
                a = if mode <= 4 {if Buggy == 2 {4} else {3}} else {2};
                b = if Buggy == 1 {0} else {b};
                foreign = if Buggy == 6 {1} else {0};
            }
            action Dispatch when (stage == 1) {
                stage = 2; kind = 2;
                accepted = if mode <= 4 {if Buggy == 3 {0} else {1}} else {if Buggy == 4 {1} else {0}};
                observed = if mode <= 4 {if Buggy == 5 {0} else {1}} else {0};
                a = if mode <= 4 {3} else {2};
                b = if Buggy == 1 {0} else {b};
                foreign = if Buggy == 6 {1} else {0};
            }
            action Record when (stage == 1) {
                stage = 2; kind = 3;
                accepted = if mode <= 4 {if Buggy == 3 {0} else {1}} else {if Buggy == 4 {1} else {0}};
                observed = if mode <= 4 {if Buggy == 5 {0} else {1}} else {0};
                b = if Buggy == 1 {0} else {b};
                foreign = if Buggy == 6 {1} else {0};
            }
            invariant RetainedLiability: stage == 0 || (b == 2 && a > 0);
            invariant FixedShare: a <= 3;
            invariant EligibleProgress: 2 > stage || mode > 4 || accepted == 1;
            invariant RestrictedRefused: 2 > stage || mode <= 4 || accepted == 0;
            invariant DurableObservation: 2 > stage || kind == 1 || mode > 4 || observed == 1;
            invariant LocalDependency: foreign == 0;
        }
    }
}
