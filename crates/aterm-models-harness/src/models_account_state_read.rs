// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One read, transaction and report against absent, valid or unreadable account state.
/// The native conformance test uses real files and the production CLI. This case model
/// does not cover concurrent file replacement, whole-store loss or valid-JSON corruption.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_state_read_model() -> Model {
    aterm_spec::ty_model! {
        AccountStateRead {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var edited = 0;
            var rewritten = 0; var advertised = 0;
            action Absent when (scenario == 0) {
                scenario = 1; accepted = if Buggy == 7 {0} else {1};
                edited = 1; rewritten = 1; advertised = 1;
            }
            action Valid when (scenario == 0) {
                scenario = 2; accepted = 1; edited = 1; rewritten = 1; advertised = 1;
            }
            action Torn when (scenario == 0) {
                scenario = 3; accepted = if Buggy == 1 {1} else {0};
                edited = if Buggy == 6 {1} else {0};
                rewritten = if Buggy == 8 {1} else {0};
                advertised = if Buggy == 5 {1} else {0};
            }
            action NonUtf8 when (scenario == 0) {
                scenario = 4; accepted = if Buggy == 2 {1} else {0};
            }
            action ForeignSchema when (scenario == 0) {
                scenario = 5; accepted = if Buggy == 3 {1} else {0};
            }
            action Unreadable when (scenario == 0) {
                scenario = 6; accepted = if Buggy == 4 {1} else {0};
            }
            invariant UnknownStateIsRefused: 3 > scenario || accepted == 0;
            invariant NoRejectedEdits: 3 > scenario || edited == 0;
            invariant NoRejectedRewrite: 3 > scenario || rewritten == 0;
            invariant NoUnknownAvailability: 3 > scenario || advertised == 0;
            invariant KnownStateAccepted: scenario == 0 || scenario > 2 || accepted == 1;
        }
    }
}
