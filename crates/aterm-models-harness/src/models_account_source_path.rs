// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Source context selects a file independently of the host process's working directory.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_path_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourcePath {
            const Buggy = 0;
            var case = 0; var selected = 0; var kept = 1;
            action Explicit when (case == 0) {
                case = 1; selected = if Buggy == 1 {2} else {1};
            }
            action Configured when (case == 0) {
                case = 2; selected = if Buggy == 2 {1} else {2};
            }
            action Absolute when (case == 0) {
                case = 3; selected = if Buggy == 3 {0} else {3};
            }
            action Home when (case == 0) {
                case = 4; selected = if Buggy == 4 {1} else {4};
            }
            action NoBase when (case == 0) {
                case = 5; selected = if Buggy == 5 {1} else {0};
                kept = if Buggy == 7 {0} else {1};
            }
            action NoHome when (case == 0) {
                case = 6; selected = if Buggy == 6 {1} else {0};
            }
            action NoConfigFile when (case == 0) {
                case = 7; selected = if Buggy == 8 {1} else {0};
            }
            invariant ContextSelects: case == 0 || case > 4 || selected == case;
            invariant UnknownRefused: 5 > case || selected == 0;
            invariant InputsRetained: kept == 1;
        }
    }
}
