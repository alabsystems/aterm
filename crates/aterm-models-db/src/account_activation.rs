// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Every delivery order of a root, stop, resumption, sibling and explicit merge.
/// The graph is bounded; membership and provider effects remain separate checks.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_activation_model() -> Model {
    aterm_spec::ty_model! {
        DbAccountActivation {
            const Buggy = 0;
            var root = 0; var stop = 0; var resume = 0; var fork = 0;
            var merged = 0; var replayed = 0; var seen_stop = 0; var active = 0;
            action Root when (root == 0) {
                root = 1;
                active = if Buggy == 2 {1} else if Buggy == 7 {0} else if 1 == 1 && ((merged == 1 && stop == 1 && resume == 1 && fork == 1) || (merged == 0 && ((resume == 1 && stop == 1 && fork == 0) || (resume == 0 && stop == 0)))) {1} else {0};
            }
            action Stop when (stop == 0) {
                stop = 1; seen_stop = if Buggy == 8 {0} else {1};
                active = if Buggy == 3 {1} else if root == 1 && ((merged == 1 && 1 == 1 && resume == 1 && fork == 1) || (merged == 0 && ((resume == 1 && 1 == 1 && fork == 0) || (resume == 0 && 1 == 0)))) {1} else {0};
            }
            action Resume when (resume == 0) {
                resume = 1;
                active = if Buggy == 4 {1} else if root == 1 && ((merged == 1 && stop == 1 && 1 == 1 && fork == 1) || (merged == 0 && ((1 == 1 && stop == 1 && fork == 0) || (1 == 0 && stop == 0)))) {1} else {0};
            }
            action Fork when (fork == 0) {
                fork = 1;
                active = if Buggy == 5 {1} else if root == 1 && ((merged == 1 && stop == 1 && resume == 1 && 1 == 1) || (merged == 0 && ((resume == 1 && stop == 1 && 1 == 0) || (resume == 0 && stop == 0)))) {1} else {0};
            }
            action Merge when (merged == 0) {
                merged = 1;
                active = if Buggy == 6 {1} else if root == 1 && ((1 == 1 && stop == 1 && resume == 1 && fork == 1) || (1 == 0 && ((resume == 1 && stop == 1 && fork == 0) || (resume == 0 && stop == 0)))) {1} else {0};
            }
            action Replay when (root == 1 && replayed == 0) {
                replayed = 1; stop = if Buggy == 1 {0} else {stop};
                active = if root == 1 && ((merged == 1 && stop == 1 && resume == 1 && fork == 1) || (merged == 0 && ((resume == 1 && stop == 1 && fork == 0) || (resume == 0 && stop == 0)))) {1} else {0};
            }
            invariant RetainStop: seen_stop == stop;
            invariant StopDominates: stop == 0 || resume == 1 || active == 0;
            invariant ForkWithholds: fork == 0 || (stop == 0 && resume == 0) || merged == 1 || active == 0;
            invariant CompleteProjection: active == if root == 1 && ((merged == 1 && stop == 1 && resume == 1 && fork == 1) || (merged == 0 && ((resume == 1 && stop == 1 && fork == 0) || (resume == 0 && stop == 0)))) {1} else {0};
        }
    }
}
