// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two provider stores contend for the same machine-local sign-in lock. Guards are held
/// until release; call-path tests separately check their lifetime through authentication
/// and publication. This model does not coordinate other machines or unrelated CLIs.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_signin_lock_model() -> Model {
    aterm_spec::ty_model! {
        AccountSigninLock {
            const Buggy = 0;
            var owner = 0; var a = 0; var b = 0; var tried_a = 0; var tried_b = 0;
            action TakeA when (tried_a == 0) {
                tried_a = 1;
                a = if owner == 0 || Buggy == 1 {1} else {0};
                owner = if owner == 0 || Buggy == 1 {1} else {owner};
            }
            action TakeB when (tried_b == 0) {
                tried_b = 1;
                b = if owner == 0 || Buggy == 1 {1} else {0};
                owner = if owner == 0 || Buggy == 1 {2} else {owner};
            }
            action ReleaseA when (a == 1) {
                a = 0; owner = if Buggy == 2 {owner} else {0};
            }
            action ReleaseB when (b == 1) {
                b = 0; owner = if Buggy == 2 {owner} else {0};
            }
            action RetryA when (a == 0 && tried_a == 1 && owner == 0) { tried_a = 0; }
            action RetryB when (b == 0 && tried_b == 1 && owner == 0) { tried_b = 0; }
            invariant Exclusive: a + b <= 1;
            invariant OwnerA: a == 0 || owner == 1;
            invariant OwnerB: b == 0 || owner == 2;
            invariant NoOrphan: owner == 0 || a + b > 0;
        }
    }
}
