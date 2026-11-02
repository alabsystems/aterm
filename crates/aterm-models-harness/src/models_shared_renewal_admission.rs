// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A checked replacement preparation waits for the machine signer before
/// changing attempt history. Repeated blocked passes retain the old history.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_renewal_admission_model() -> Model {
    aterm_spec::ty_model! {
        SharedRenewalAdmission {
            const Buggy = 0;
            var locked = 1; var history = 1; var calls = 0;
            action Blocked when (locked == 1) {
                history = if Buggy == 1 {2} else {history};
                calls = if Buggy == 2 {1} else {calls};
            }
            action Unlock when (locked == 1) {
                locked = 0; history = if Buggy == 3 {2} else {history};
            }
            action Start when (locked == 0 && (calls == 0 || Buggy == 5)) {
                history = if Buggy == 4 {history} else {2}; calls = calls + 1;
            }
            action Used when (locked == 0 && calls == 1) {
                calls = if Buggy == 6 {2} else {calls};
            }
            invariant LockedHasNoProvider: locked == 0 || calls == 0;
            invariant UnadmittedKeepsHistory: calls > 0 || history == 1;
            invariant AdmittedUsesReplacement: calls == 0 || history == 2;
            invariant OneProviderPerPreparation: calls <= 1;
        }
    }
}
