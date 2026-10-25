// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Binding publication, optional loss, admission, then an edit or explicit restore.
/// The witness survives the modeled loss. This does not prove filesystem flushes,
/// cross-file transactions, valid-JSON integrity, or survival after all evidence is lost.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_binding_retention_model() -> Model {
    aterm_spec::ty_model! {
        AccountBindingRetention {
            const Buggy = 0;
            var phase = 0; var attempted = 0; var witness = 0; var present = 0;
            var accepted = 0; var edited = 0; var restored = 0;
            action Save when (phase == 0) {
                phase = 1; attempted = 1; present = 1;
                witness = if Buggy == 1 {0} else {1};
            }
            action InterruptedSave when (phase == 0) {
                phase = 1; attempted = 1;
                witness = if Buggy == 1 {0} else {1};
            }
            action Legacy when (phase == 0) {
                phase = 1; present = 1;
            }
            action Fresh when (phase == 0) {
                phase = 1;
            }
            action LoseBindings when (phase == 1 && present == 1) {
                phase = 2; present = 0;
            }
            action Read when (phase <= 2) {
                phase = 3;
                accepted = if present == 1 { if Buggy == 6 {0} else {1} }
                    else if witness == 0 || Buggy == 2 { if Buggy == 4 {0} else {1} }
                    else {0};
            }
            action Edit when (phase == 3) {
                phase = 4;
                edited = if accepted == 1 || Buggy == 3 {1} else {0};
                attempted = if accepted == 1 {1} else {attempted};
                witness = if accepted == 1 {1} else {witness};
                present = if accepted == 1 {1} else {present};
            }
            action Restore when (phase == 3) {
                phase = 4; restored = 1; attempted = 1; witness = 1;
                present = if Buggy == 5 {0} else {1};
            }
            invariant AttemptRetained: attempted == 0 || witness == 1;
            invariant LostBindingsRefused: phase <= 2 || present == 1 || witness == 0 || accepted == 0;
            invariant RefusalNotEdited: edited == 0 || accepted == 1;
            invariant FreshAllowed: phase <= 2 || present == 1 || witness == 1 || accepted == 1;
            invariant RestorePublished: restored == 0 || present == 1;
            invariant KnownReadable: phase <= 2 || phase > 3 || present == 0 || accepted == 1;
        }
    }
}
