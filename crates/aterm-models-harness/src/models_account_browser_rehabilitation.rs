// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Browser completion admits another identity check without authenticating the account.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_browser_rehabilitation_model() -> Model {
    aterm_spec::ty_model! {
        AccountBrowserRehabilitation {
            const Buggy = 0;
            var case = 0; var phase = 0; var ready = 0;
            var authentication = 0; var wrong_retained = 1; var unchanged = 1;
            action Case when (phase == 0 && 36 > case) { case = case + 1; }
            action Read when (phase == 0) {
                phase = 1;
                ready = if Buggy == 10 && case == 34 {1}
                    else if Buggy == 11 && case == 35 {0}
                    else if Buggy == 12 && case == 36 {1}
                    else if Buggy == 1 && case == 1 {0}
                    else if Buggy == 2 && (case == 3 || case == 4) {1}
                    else if Buggy == 3 && (case == 5 || case == 6) {1}
                    else if Buggy == 4 && (case == 8 || case == 9 || case == 12
                        || case == 13 || case == 18 || case == 19 || case == 24
                        || case == 27 || case == 28 || case == 29) {1}
                    else if Buggy == 5 && (case == 14 || case == 15 || case == 16
                        || case == 25 || case == 26) {1}
                    else if Buggy == 6 && (case == 2 || case == 17 || case == 20 || case == 23 || (case > 30 && 34 > case)) {1}
                    else if case == 0 || case == 1 || case == 7 || case == 10
                        || case == 11 || case == 21 || case == 22 || case == 30
                        || case == 35 {1} else {0};
                authentication = if Buggy == 7 {1} else {0};
                wrong_retained = if Buggy == 8 {0} else {1};
                unchanged = if Buggy == 9 {0} else {1};
            }
            invariant CurrentBrowserReceiptAdmitted: phase == 0
                || (case > 1 && 7 > case) || (case > 7 && 10 > case)
                || (case > 11 && 21 > case) || (case > 22 && 30 > case) || case > 30 || ready == 1;
            invariant ReceiptStrictlyNewer: 3 > case || case > 4 || ready == 0;
            invariant LaterWrongAccountInvalidates: 5 > case || case > 6 || ready == 0;
            invariant LatestCompletionMustQualify: case <= 7 || case == 10 || case == 11
                || (case > 13 && 18 > case) || (case > 19 && 24 > case)
                || case == 25 || case == 26 || case > 29 || ready == 0;
            invariant TrustedProfileRequired: 14 > case || (case > 16 && 25 > case)
                || case > 26 || ready == 0;
            invariant UnreadableOrMissingEvidenceRefused: case <= 1 || (case > 2 && 17 > case)
                || (case > 17 && 20 > case) || (case > 20 && 23 > case)
                || (case > 23 && 31 > case) || case > 33 || ready == 0;
            invariant BrowserIsNotAuthentication: authentication == 0;
            invariant WorkflowScopedReceiptRequired: phase == 0 || 34 > case || case > 34 || ready == 0;
            invariant CurrentScopedReceiptAdmitted: phase == 0 || 35 > case || case > 35 || ready == 1;
            invariant IndependentLaterWrongAccountVeto: phase == 0 || 36 > case || case > 36 || ready == 0;
            invariant WrongAccountEvidenceRetained: wrong_retained == 1;
            invariant ReadOnlyAdmission: unchanged == 1;
        }
    }
}
