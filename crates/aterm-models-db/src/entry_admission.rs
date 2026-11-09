// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Ten local admission cases. `blocked` observes a separate process's exclusive
/// lock attempt after the callback durably records its marker; `released`
/// observes another attempt after admission returns. The caller's existing
/// writer in the Busy case remains held. This model claims no provider grant,
/// delivery freshness, callback transaction recovery or semantic reactivation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn entry_admission_model() -> Model {
    aterm_spec::ty_model! {
        DbEntryAdmission {
            const Buggy = 0;
            var scenario = 0; var done = 0; var result = 0;
            var entered = 0; var blocked = 0; var released = 0; var changed = 0;
            action Case when (done == 0 && 9 > scenario) { scenario = scenario + 1; }
            action Admit when (done == 0) {
                done = 1;
                result = if Buggy == 1 {1} else {
                    if 2 > scenario {1} else {
                        if 4 > scenario {4} else {
                            if scenario == 7 {2} else {if scenario == 8 {5} else {3}}
                        }
                    }
                };
                entered = if 2 > scenario || Buggy == 2 {1} else {0};
                blocked = if 2 > scenario && Buggy == 3 {0} else {if 2 > scenario {1} else {0}};
                released = if scenario == 7 || Buggy == 4 {0} else {1};
                changed = if Buggy == 5 {1} else {0};
            }
            invariant Outcome: done == 0 || result ==
                (if 2 > scenario {1} else {if 4 > scenario {4} else {
                    if scenario == 7 {2} else {if scenario == 8 {5} else {3}}
                }});
            invariant CallbackOnlyCurrent: done == 0 || entered == (if 2 > scenario {1} else {0});
            invariant HeldThroughDurability: entered == 0 || blocked == 1;
            invariant ReleasedOnReturn: done == 0 || released == (if scenario == 7 {0} else {1});
            invariant VaultReadOnly: changed == 0;
        }
    }
}
