// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two status writers with identical native state and distinct publication identities.
/// The model assumes fresh identities and truthful filesystem acknowledgements.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_write_identity_model() -> Model {
    aterm_spec::ty_model! {
        NativeWriteIdentity {
            const Buggy = 0;
            var disk = 0; var meta = 0; var begun = 0; var rival = 0;
            var retries = 0; var ack = 0; var fresh = 0;
            var clobbered = 0; var blocked = 0; var premature = 0; var reused = 0;
            action AttemptBefore when (begun == 0) { begun = 1; }
            action AttemptAfter when (begun == 0) { begun = 1; disk = 1; meta = 1; }
            action Rival when (begun == 1 && rival == 0 && fresh == 0) {
                rival = 1;
                disk = if disk == 0 {2} else {disk};
                meta = if disk == 0 {2} else {meta};
            }
            action Retry when (begun == 1 && retries <= 1 && fresh == 0 && ack == 0) {
                retries = retries + 1;
                clobbered = if disk == 2 && Buggy == 1 {1} else {clobbered};
                blocked = if disk <= 1 && Buggy == 3 {1} else {blocked};
                disk = if disk <= 1 && (Buggy <= 2 || Buggy == 4) {1} else {disk};
                meta = if (disk <= 1 && (Buggy <= 2 || Buggy == 4)) || (disk == 2 && Buggy == 1) {1} else {meta};
                ack = if (disk <= 1 && (Buggy <= 2 || Buggy == 4)) || (disk == 2 && Buggy == 1) {1} else {ack};
            }
            action LateRetry when (begun == 1 && retries <= 1 && fresh == 0 && ack == 0) {
                retries = retries + 1;
                disk = if disk <= 1 {1} else {disk};
                meta = if disk <= 1 {1} else {meta};
                ack = if disk <= 1 && Buggy == 4 {1} else {ack};
                premature = if disk <= 1 && Buggy == 4 && ack == 0 {1} else {premature};
            }
            action Fresh when (ack == 1 && fresh == 0) {
                fresh = 1;
                disk = if Buggy == 2 {1} else {3};
                meta = 3;
                reused = if Buggy == 2 {1} else {reused};
            }
            invariant PreserveRivalMetadata: clobbered == 0;
            invariant ValidRetryProgress: blocked == 0;
            invariant NoFailedAcknowledgement: premature == 0;
            invariant FreshWriteIdentity: reused == 0;
            bound Finite: 0 <= disk && disk <= 3 && 0 <= meta && meta <= 3
                && 0 <= begun && begun <= 1 && 0 <= rival && rival <= 1
                && 0 <= retries && retries <= 2 && 0 <= ack && ack <= 1
                && 0 <= fresh && fresh <= 1 && 0 <= clobbered && clobbered <= 1
                && 0 <= blocked && blocked <= 1 && 0 <= premature && premature <= 1
                && 0 <= reused && reused <= 1;
        }
    }
}
