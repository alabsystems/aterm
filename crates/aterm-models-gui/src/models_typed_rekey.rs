// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typed re-key.

use aterm_spec::derive::Model;

/// One degraded (or healthy) tab, one typed re-key, one settlement.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn typed_rekey_model() -> Model {
    aterm_spec::ty_model! {
        TypedRekey {
            const Buggy = 0;
            var phase = 0;
            var working = 0;
            var sh = 1;
            var eng = 0;
            var prior = 0;
            var issued = 0;
            var file = 0;
            var settled = 0;
            var fkey = 0;
            var upg = 0;

            action Lose when (phase == 0) { phase = 1; }
            action Keep when (phase == 0) { phase = 1; working = 1; eng = 1; }
            action Issue when (phase == 1 && issued == 0 && (eng == 0 || Buggy == 1)) {
                issued = 1;
                file = 1;
                prior = eng;
                eng = 2;
                fkey = 2;
            }
            action Upgrade when (phase == 1 && issued == 0 && eng == 1) {
                issued = 1;
                file = 1;
                upg = 1;
                prior = eng;
                fkey = if Buggy == 1 { 0 } else { eng };
            }
            action Read when (file == 1) {
                file = 0;
                sh = fkey;
            }
            action Settle when (issued == 1 && settled == 0) {
                settled = 1;
                file = if Buggy == 1 { file } else { 0 };
                eng = if Buggy == 0 && file == 1 { prior } else { eng };
            }

            invariant WorkingKeyNeverMoved: working == 0 || eng == sh;
            invariant UnreadKeyTakenBack: settled == 0 || sh == 2 || eng <= 1;
            invariant NoLiveKeyInAFile: settled == 0 || file == 0 || fkey > eng || eng > fkey;
        }
    }
}
