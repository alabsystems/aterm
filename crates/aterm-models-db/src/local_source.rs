// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local negative knowledge needs an understood catalog and retained source custody.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn local_source_model() -> Model {
    aterm_spec::ty_model! {
        DbLocalSource {
            const Buggy = 0;
            var scenario = 0; var done = 0; var called = 0;
            var accepted = 0; var absent = 0; var held = 1;
            action Case when (done == 0 && 42 > scenario) { scenario = scenario + 1; }
            action Read when (done == 0) {
                done = 1;
                called = if Buggy == 6 {0} else { if scenario <= 4 || scenario == 20 || scenario == 21 || scenario == 22
                    || scenario == 23 || scenario == 28 || scenario == 34 || scenario == 35 || scenario == 36 || scenario == 41 || scenario == 42
                    || (Buggy == 1 && (scenario == 24 || scenario == 25 || scenario == 30 || scenario == 39))
                    || (Buggy == 2 && (scenario == 8 || scenario == 9))
                    || (Buggy == 3 && (scenario == 16 || scenario == 17 || scenario == 18 || scenario == 32))
                    || (Buggy == 5 && (scenario == 7 || scenario == 31)) {1} else {0} };
                accepted = if Buggy == 6 || scenario == 22 || scenario == 23 {0} else {
                    if scenario <= 4 || scenario == 20 || scenario == 21 || scenario == 28
                        || scenario == 34 || scenario == 35 || scenario == 36 || scenario == 41 || scenario == 42
                        || (Buggy == 1 && (scenario == 24 || scenario == 25 || scenario == 30 || scenario == 39))
                        || (Buggy == 2 && (scenario == 8 || scenario == 9))
                        || (Buggy == 3 && (scenario == 16 || scenario == 17 || scenario == 18 || scenario == 32))
                        || (Buggy == 5 && (scenario == 7 || scenario == 31)) {1} else {0}
                };
                absent = if Buggy == 6 {0} else {
                    if scenario <= 2 || scenario == 20 || scenario == 21 || scenario == 22 || scenario == 23
                        || scenario == 34 || scenario == 35 || scenario == 36 || scenario == 41 || scenario == 42
                        || (Buggy == 1 && (scenario == 24 || scenario == 30 || scenario == 39))
                        || (Buggy == 2 && (scenario == 8 || scenario == 9))
                        || (Buggy == 3 && (scenario == 16 || scenario == 17 || scenario == 18 || scenario == 32))
                        || (Buggy == 5 && (scenario == 7 || scenario == 31)) {1} else {0}
                };
                held = if Buggy == 4 && (scenario <= 2 || scenario == 20 || scenario == 21
                    || scenario == 22 || scenario == 23 || scenario == 34 || scenario == 35
                    || scenario == 36 || scenario == 41 || scenario == 42) {0} else {1};
            }
            invariant AdmittedInputs: called == 0 || scenario <= 4 || scenario == 20
                || scenario == 21 || scenario == 22 || scenario == 23 || scenario == 28
                || scenario == 34 || scenario == 35 || scenario == 36 || scenario == 41 || scenario == 42;
            invariant CatalogCustody: called == 0 || held == 1;
            invariant AvailableSources: done == 0 || (scenario > 4 && (20 > scenario || scenario > 21)
                && (28 > scenario || scenario > 28) && (34 > scenario || scenario > 36) && 41 > scenario) || accepted == 1;
            bound Kind: 0 <= absent && absent <= 1;
            bound Cases: 0 <= scenario && scenario <= 42;
        }
    }
}
