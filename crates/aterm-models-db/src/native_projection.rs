// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A current reader may persist a bounded local projection while native data
/// and source custody stay held. Unavailable or changed inputs do not enter it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_projection_model() -> Model {
    aterm_spec::ty_model! {
        DbNativeProjection {
            const Buggy = 0;
            var scenario = 0; var done = 0; var called = 0; var accepted = 0; var held = 1;
            action Case when (done == 0 && 38 > scenario) { scenario = scenario + 1; }
            action Read when (done == 0) {
                done = 1;
                called = if Buggy == 3 || ((Buggy == 4 || Buggy == 5) && (scenario == 15 || scenario == 16)) {0} else {
                    if scenario == 0 || scenario == 1 || scenario == 8 || scenario == 9
                        || (11 <= scenario && scenario <= 17) || scenario == 32 || scenario == 33
                        || (Buggy == 1 && (scenario == 2 || (26 <= scenario && scenario <= 31)))
                        || (Buggy == 4 && (scenario == 36 || scenario == 37))
                        || (Buggy == 5 && (scenario == 34 || scenario == 35)) {1} else {0}
                };
                accepted = if Buggy == 3 || ((Buggy == 4 || Buggy == 5) && (scenario == 15 || scenario == 16)) {0} else {
                    if scenario == 0 || scenario == 1 || (11 <= scenario && scenario <= 17)
                        || (Buggy == 1 && (scenario == 2 || (26 <= scenario && scenario <= 31)))
                        || (Buggy == 4 && (scenario == 36 || scenario == 37))
                        || (Buggy == 5 && (scenario == 34 || scenario == 35)) {1} else {0}
                };
                held = if Buggy == 2 && (scenario == 0 || scenario == 1 || scenario == 8 || scenario == 9
                    || (11 <= scenario && scenario <= 17) || scenario == 32 || scenario == 33) {0} else {1};
            }
            invariant AdmittedInputs: scenario == 0 || scenario == 1 || scenario == 8 || scenario == 9
                || (11 <= scenario && scenario <= 17) || scenario == 32 || scenario == 33 || called == 0;
            invariant DataCustody: called == 0 || held == 1;
            invariant ReaderAndWriterAvailable: done == 0 || (scenario > 1 && (11 > scenario || scenario > 17)) || accepted == 1;
            bound Cases: 0 <= scenario && scenario <= 38;
        }
    }
}
