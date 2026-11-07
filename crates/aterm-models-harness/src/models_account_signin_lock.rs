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

/// Native browser reservations share legacy exclusion, while native code POST
/// and managed refresh use a different gate. This models kernel admission only;
/// it grants no provider identity, source publication, or account authorization.
/// Mutants bypass legacy exclusion, admit POST over refresh, and release the
/// browser reservation during POST, respectively.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_signin_phase_model() -> Model {
    aterm_spec::ty_model! {
        NativeSigninPhases {
            const Buggy = 0;
            var browser_a = 0; var browser_b = 0; var legacy = 0;
            var post = 0; var refresh_a = 0; var refresh_b = 0;
            action BrowserA when (browser_a == 0) { browser_a = if legacy == 0 {1} else {0}; }
            action BrowserB when (browser_b == 0) { browser_b = if legacy == 0 {1} else {0}; }
            action Legacy when (legacy == 0) { legacy = if browser_a + browser_b == 0 || Buggy == 1 {1} else {0}; }
            action Post when (post == 0 && browser_a + browser_b > 0) { post = if refresh_a + refresh_b == 0 || Buggy == 2 {1} else {0}; }
            action RefreshA when (refresh_a == 0) { refresh_a = if post == 0 {1} else {0}; }
            action RefreshB when (refresh_b == 0) { refresh_b = if post == 0 {1} else {0}; }
            action EndBrowserA when (browser_a == 1 && (post == 0 || Buggy == 3)) { browser_a = 0; }
            action EndBrowserB when (browser_b == 1 && (post == 0 || Buggy == 3)) { browser_b = 0; }
            action EndLegacy when (legacy == 1) { legacy = 0; }
            action EndPost when (post == 1) { post = 0; }
            action EndRefreshA when (refresh_a == 1) { refresh_a = 0; }
            action EndRefreshB when (refresh_b == 1) { refresh_b = 0; }
            invariant LegacyExclusive: legacy == 0 || browser_a + browser_b == 0;
            invariant UnknownPostExclusive: post == 0 || refresh_a + refresh_b == 0;
            invariant BrowserOwnsPost: post == 0 || browser_a + browser_b > 0;
        }
    }
}
