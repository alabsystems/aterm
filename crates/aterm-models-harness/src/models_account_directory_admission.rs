// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// New account rows cannot take another account's current, historical or default directory.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_directory_admission_model() -> Model {
    aterm_spec::ty_model! {
        AccountDirectoryAdmission {
            const Buggy = 0;
            var case = 1; var phase = 0; var accepted = 0; var unchanged = 1;
            action AnotherCase when (phase == 0 && 16 > case) {case = case + 1;}
            action Check when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 1 && case == 2 {1}
                    else if Buggy == 2 && case == 3 {1}
                    else if Buggy == 3 && case == 5 {1}
                    else if Buggy == 4 && case == 7 {1}
                    else if Buggy == 5 && case == 12 {1}
                    else if Buggy == 6 && case == 9 {1}
                    else if Buggy == 7 && case == 1 {0}
                    else if case == 1 || case == 14 || case == 15 || case == 16 {1} else {0};
                unchanged = if Buggy == 8 {0} else {1};
            }
            invariant NoDirectoryTransfer: accepted == 0 || case == 1 || case > 13;
            invariant SeparateDirectoriesAvailable: phase == 0 || accepted == 1 || (case > 1 && 14 > case);
            invariant AdmissionOnlyReads: unchanged == 1;
        }
    }
}
