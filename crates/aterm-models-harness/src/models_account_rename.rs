// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A local account relabel preserves its payload and references, or refuses without edits.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_rename_model() -> Model {
    aterm_spec::ty_model! {
        AccountRename {
            const Buggy = 0;
            var phase = 0; var case = 1; var accepted = 0;
            var preserved = 1; var payload = 1; var detached = 1; var unrelated = 1;
            action AnotherCase when (phase == 0 && case <= 21) { case = case + 1; }
            action Move when (phase == 0) {
                phase = 1;
                accepted = if case == 1 {if Buggy == 1 {0} else {1}}
                    else if case == 2 {if Buggy == 2 {1} else {0}}
                    else if case == 3 {if Buggy == 3 {1} else {0}}
                    else if case == 4 {if Buggy == 4 {1} else {0}}
                    else if case == 5 {if Buggy == 5 {1} else {0}}
                    else if case == 6 {if Buggy == 6 {1} else {0}}
                    else if case == 7 {if Buggy == 7 {1} else {0}}
                    else if case == 8 {if Buggy == 8 {1} else {0}}
                    else if case == 9 {if Buggy == 9 {1} else {0}}
                    else if case == 10 {if Buggy == 10 {1} else {0}}
                    else if case == 11 {if Buggy == 11 {1} else {0}}
                    else if case == 12 {if Buggy == 12 {0} else {1}}
                    else if case == 13 {if Buggy == 13 {0} else {1}}
                    else if case == 14 {if Buggy == 14 {0} else {1}}
                    else if case == 15 {if Buggy == 15 {0} else {1}}
                    else if case == 16 {if Buggy == 16 {1} else {0}}
                    else if case == 17 {if Buggy == 17 {1} else {0}}
                    else if case == 18 {if Buggy == 18 {0} else {1}}
                    else if case == 19 {if Buggy == 19 {0} else {1}}
                    else if case == 20 {if Buggy == 20 {0} else {1}}
                    else if case == 21 {if Buggy == 21 {1} else {0}}
                    else if case == 22 {if Buggy == 22 {1} else {0}} else {0};
                preserved = if Buggy == 23 {0} else {1};
                payload = if Buggy == 24 {0} else {1};
                detached = if Buggy == 25 {0} else {1};
                unrelated = if Buggy == 26 {0} else {1};
            }
            invariant NoCollision: accepted == 0 || case == 1 || case == 12 || case == 13
                || case == 14 || case == 15 || case == 18 || case == 19 || case == 20;
            invariant ValidMoveAdmitted: phase == 0 || accepted == 1 || (case > 1 && 12 > case)
                || case == 16 || case == 17 || case == 21 || case == 22;
            invariant RefusalAndSelfMovePreserve: preserved == 1;
            invariant PayloadPreserved: payload == 1;
            invariant NoStaleSourceReference: detached == 1;
            invariant UnrelatedStatePreserved: unrelated == 1;
        }
    }
}
