// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Opening a validated checkpoint does not rewrite it, but still finishes its directory
/// barrier. A missing checkpoint is initialized only for a fresh, empty journal. Physical
/// hashing, writer locks and astream decoding are prerequisites outside this bounded model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn journal_open_model() -> Model {
    aterm_spec::ty_model! {
        JournalOpen {
            const Buggy = 0;
            var present = 1; var valid = 1; var fresh = 0; var barrier = 1; var early = 0;
            var phase = 0; var admitted = 0; var rewritten = 0;
            action Missing when (phase == 0 && present == 1 && valid == 1) { present = 0; }
            action Corrupt when (phase == 0 && present == 1 && valid == 1) { valid = 0; }
            action Unenroll when (phase == 0 && fresh == 0) { fresh = 1; }
            action FailBarrier when (phase == 0 && barrier == 1) { barrier = 0; }
            action FailEarly when (phase == 0 && barrier == 1) { barrier = 0; early = 1; }
            action Open when (phase == 0) {
                phase = 1;
                rewritten = if Buggy == 6 {0} else {if valid == 1 && (present == 1 || fresh == 1)
                    && (early == 0 || Buggy == 7)
                    && (present == 0 || Buggy == 4) {1} else {0}};
                admitted = if Buggy == 5 {0} else {if (valid == 1 || Buggy == 2)
                    && (present == 1 || fresh == 1 || Buggy == 3)
                    && (barrier == 1 || Buggy == 1) {1} else {0}};
            }
            invariant ValidPrefix: admitted == 0 || valid == 1;
            invariant EnrolledOrFresh: admitted == 0 || present == 1 || fresh == 1;
            invariant DurableBeforeAdmission: admitted == 0 || barrier == 1;
            invariant ExistingNotRewritten: present == 0 || rewritten == 0;
            invariant FreshCheckpoint: admitted == 0 || present == 1 || fresh == 0 || rewritten == 1;
            invariant NoPublicationAfterEarlyFailure: early == 0 || rewritten == 0;
            invariant Available: phase == 0 || valid == 0 || (present == 0 && fresh == 0)
                || barrier == 0 || admitted == 1;
            bound OpenBox: present <= 1 && valid <= 1 && fresh <= 1 && barrier <= 1 && early <= 1
                && phase <= 1 && admitted <= 1 && rewritten <= 1;
        }
    }
}
