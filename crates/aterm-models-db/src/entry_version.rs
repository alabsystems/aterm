// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The version relation retains source context and authenticated log facts,
/// including when an entry's absence was observed. Authentication is separate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn entry_version_model() -> Model {
    aterm_spec::ty_model! {
        DbEntryVersion {
            const Buggy = 0;
            var scenario = 0; var accepted = 0;
            action Same when (scenario == 0) { scenario = 1; accepted = if Buggy == 1 {0} else {1}; }
            action Advance when (scenario == 0) { scenario = 2; accepted = if Buggy == 2 {0} else {1}; }
            action Rollback when (scenario == 0) { scenario = 3; accepted = if Buggy == 3 {1} else {0}; }
            action Fork when (scenario == 0) { scenario = 4; accepted = if Buggy == 4 {1} else {0}; }
            action LogExtend when (scenario == 0) { scenario = 5; accepted = if Buggy == 5 {0} else {1}; }
            action LogDrop when (scenario == 0) { scenario = 6; accepted = if Buggy == 6 {1} else {0}; }
            action LogSwap when (scenario == 0) { scenario = 7; accepted = if Buggy == 7 {1} else {0}; }
            action Context when (scenario == 0) { scenario = 8; accepted = if Buggy == 8 {1} else {0}; }
            invariant Progress: scenario == 0 || scenario == 3 || scenario == 4 || scenario > 5 || accepted == 1;
            invariant Retained: scenario == 0 || scenario == 1 || scenario == 2 || scenario == 5 || accepted == 0;
        }
    }
}
