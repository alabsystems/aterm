// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Anonymous Claude login observes current staging under its publication writer.
/// Credential 0 is empty, 1 opaque, 2 an ordinary ID and 3 a shared-reserved ID.
/// An earlier observation may differ; this is not a continuous filesystem lease.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn plain_login_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountPlainLoginCommit {
            const Buggy = 0;
            var custody = 0; var credential = 1; var prior = 0; var phase = 0;
            var seen = 0; var published = 0; var bound = 0; var unchanged = 1;
            var preserved = 1; var guarded = 1; var calls = 0;
            action Custody when (phase == 0 && custody == 0) { custody = 1; }
            action Credential when (phase == 0 && credential <= 2) { credential = credential + 1; }
            action Prior when (phase == 0 && prior == 0) { prior = 1; }
            action Report when (phase == 0) { phase = 1; seen = credential; }
            action ChangeCredential when (phase == 1 && credential <= 2) { credential = credential + 1; }
            action LoseCredential when (phase == 1 && credential > 0) { credential = 0; }
            action Commit when (phase == 1) {
                phase = 2;
                published = if Buggy == 10 {
                    if seen == 0 || (custody == 1 && (seen == 1 || seen == 3)) {0} else {1}
                } else if (Buggy == 1 && credential == 0)
                    || (Buggy == 2 && custody == 1 && credential == 1)
                    || (Buggy == 3 && custody == 1 && credential == 3) {1}
                else if credential == 0 || (custody == 1 && (credential == 1 || credential == 3)) {0}
                else {if Buggy == 4 {0} else {1}};
                bound = if credential == 0 || (custody == 1 && (credential == 1 || credential == 3)) {
                    if Buggy == 5 {1} else {0}
                } else {if Buggy == 5 {0} else {1}};
                unchanged = if Buggy == 6 {0}
                    else if credential == 0 || (custody == 1 && (credential == 1 || credential == 3)) {1} else {0};
                preserved = if Buggy == 7 {0} else {1};
                guarded = if Buggy == 8 {0} else {1};
                calls = if credential == 0 {0} else {if Buggy == 9 {2} else {1}};
            }
            invariant Publication: phase <= 1 || published == if credential == 0 || (custody == 1 && (credential == 1 || credential == 3)) {0} else {1};
            invariant Binding: phase <= 1 || bound == published;
            invariant Records: phase <= 1 || unchanged == 1 - published;
            invariant Evidence: preserved == 1;
            invariant LockedObservation: calls == 0 || guarded == 1;
            invariant OneRead: phase <= 1 || calls == if credential == 0 {0} else {1};
        }
    }
}
