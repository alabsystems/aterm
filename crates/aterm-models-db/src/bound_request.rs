// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Binding preservation over an already durable dispatch implementation.
/// Hash collision resistance and authenticated inputs are assumptions; neither
/// this model nor its conformance test establishes provider billing semantics.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn bound_request_model() -> Model {
    aterm_spec::ty_model! {
        DbBoundRequest {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var entered = 0; var exact = 1;
            action Fresh when (scenario == 0) {
                scenario = 1; accepted = if Buggy == 11 {0} else {1}; entered = 1;
                exact = if Buggy == 10 {0} else {1};
            }
            action Retry when (scenario == 0) {
                scenario = 2; accepted = if Buggy == 12 {0} else {1}; entered = if Buggy == 9 {1} else {0};
            }
            action Principal when (scenario == 0) {
                scenario = 3; accepted = if Buggy == 1 {1} else {0};
            }
            action Account when (scenario == 0) {
                scenario = 4; accepted = if Buggy == 2 {1} else {0};
            }
            action Revision when (scenario == 0) {
                scenario = 5; accepted = if Buggy == 3 {1} else {0};
            }
            action Credential when (scenario == 0) {
                scenario = 6; accepted = if Buggy == 4 {1} else {0};
            }
            action Policy when (scenario == 0) {
                scenario = 7; accepted = if Buggy == 5 {1} else {0};
            }
            action Adapter when (scenario == 0) {
                scenario = 8; accepted = if Buggy == 6 {1} else {0};
            }
            action Payload when (scenario == 0) {
                scenario = 9; accepted = if Buggy == 7 {1} else {0};
            }
            action Cost when (scenario == 0) {
                scenario = 10; accepted = if Buggy == 8 {1} else {0};
            }
            invariant FreshProgress: 1 > scenario || scenario > 1 || (accepted == 1 && entered == 1);
            invariant ExactInputs: exact == 1;
            invariant RetryDoesNotEnter: 2 > scenario || scenario > 2 || entered == 0;
            invariant ExactRetryReadable: 2 > scenario || scenario > 2 || accepted == 1;
            invariant ChangedBindingRefused: scenario <= 2 || accepted == 0;
        }
    }
}
