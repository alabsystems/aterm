// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A login is adopted only on evidence newer than its recorded sign-out, of a request that
/// began after it, and of a login place a credential-free file proves the row's own.
///
/// `word` is whether the row reads signed out on record. `order` places the evidence against
/// that sign-out: 0 not after it, 1 after it and not after now, 2 dated after now. `owner` is
/// what says whose login the place holds: 0 the default login's profile names the row's
/// address, 1 it names another account, 2 it says nothing, 3 the row is on a login directory
/// of its own, which no file without a credential proves. `kind` is served evidence (0) or a
/// sign-in made in a tab (1). `begun` is when the served request began: 0 after the sign-out,
/// 1 before it (authorized by the login that was then lost, and answered after).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_login_adoption_model() -> Model {
    aterm_spec::ty_model! {
        AccountLoginAdoption {
            const Buggy = 0;
            var phase = 0; var word = 1; var order = 0; var owner = 0; var kind = 0;
            var begun = 0; var lifted = 0; var cleared = 0;
            action NotSignedOut when (phase == 0 && word == 1) { word = 0; }
            action LaterEvidence when (phase == 0 && order <= 1) { order = order + 1; }
            action AnotherOwner when (phase == 0 && owner <= 2) { owner = owner + 1; }
            action SignedIn when (phase == 0 && kind == 0 && begun == 0) { kind = 1; }
            action InFlight when (phase == 0 && kind == 0 && begun == 0) { begun = 1; }
            action Adopt when (phase == 0) {
                phase = 1;
                lifted = if word == 1 && order == 1 && owner == 0 && begun == 0 {
                    if Buggy == 1 {0} else {1}
                } else if (Buggy == 2 && word == 1 && order == 0 && owner == 0 && begun == 0)
                    || (Buggy == 3 && word == 1 && order == 1 && owner == 1 && begun == 0)
                    || (Buggy == 4 && word == 1 && order == 1 && owner == 2 && begun == 0)
                    || (Buggy == 5 && word == 1 && order == 2 && owner == 0 && begun == 0)
                    || (Buggy == 7 && word == 0 && order == 1 && owner == 0 && begun == 0)
                    || (Buggy == 8 && word == 1 && order == 1 && owner == 3 && begun == 0)
                    || (Buggy == 9 && word == 1 && order == 1 && owner == 0 && begun == 1) {1} else {0};
                cleared = if word == 1 && order == 1 && owner == 0 && begun == 0 {
                    if Buggy == 6 {0} else {1}
                } else {0};
            }
            invariant NewerEvidenceOfItsLoginLifts:
                phase == 0 || word == 0 || order == 0 || order == 2 || owner > 0 || begun == 1
                    || lifted == 1;
            invariant OlderEvidenceLiftsNothing: phase == 0 || order > 0 || lifted == 0;
            invariant AnothersLoginLiftsNothing: phase == 0 || owner == 0 || owner > 1 || lifted == 0;
            invariant UnknownLoginLiftsNothing: phase == 0 || owner <= 1 || owner == 3 || lifted == 0;
            invariant UnprovedPlaceLiftsNothing: phase == 0 || owner <= 2 || lifted == 0;
            invariant InFlightRequestLiftsNothing: phase == 0 || begun == 0 || lifted == 0;
            invariant FutureEvidenceLiftsNothing: phase == 0 || order <= 1 || lifted == 0;
            invariant NothingSignedOutLiftsNothing: phase == 0 || word == 1 || lifted == 0;
            invariant LiftClearsThePendingSignIn: phase == 0 || lifted == 0 || cleared == 1;
        }
    }
}
