// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Provider observations checked at the first writer or after sign-in custody.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_provider_capture_model() -> Model {
    aterm_spec::ty_model! {
        AccountProviderCapture {
            const Buggy = 0;
            var origin = 0; var change = 0; var config = 0; var stage = 0;
            var admitted = 0; var done = 0;
            action Origin when (done == 0 && origin == 0) { origin = 1; }
            action Change when (done == 0 && 5 > change) { change = change + 1; }
            action Config when (done == 0 && config == 0) { config = 1; }
            action Stage when (done == 0 && stage == 0) { stage = 1; }
            action Admit when (done == 0) {
                admitted = if Buggy == 1 || (Buggy == 4 && stage == 1) {1}
                    else if change > 1 {0}
                    else if config == 1 {if Buggy == 2 {1} else {0}}
                    else if Buggy == 3 && change == 1 {0} else {1};
                done = 1;
            }
            invariant ChangedSourceRefused: done == 0 || 2 > change || admitted == 0;
            invariant ChangedConfigRefused: done == 0 || config == 0 || admitted == 0;
            invariant EquivalentSourceAvailable: done == 0 || config == 1
                || change > 1 || admitted == 1;
            bound Inputs: origin <= 1 && change <= 5 && config <= 1 && stage <= 1;
        }
    }
}
