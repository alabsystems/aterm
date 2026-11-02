// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A deep rename's durable keep and its one pass-local exemption.

use aterm_spec::derive::*;

/// A move can escape a just-published ancestor keep. Restoration may fail at every name;
/// the top's prearmed keep must then outlive the current pass. Explicit keep acknowledgement
/// revokes new mutation admissions, even though the provisional declaration was already
/// retained. An admission is the permission check, not an atomic filesystem operation:
/// an already-admitted syscall may race a later keep acknowledgement.
/// `Buggy=1` moves before durable fencing; `Buggy=2` acknowledges an explicit keep without
/// revoking the token; `Buggy=3` drops the fence after failed restoration; `Buggy=4` lets a
/// later pass ignore the fence that stayed. Real filesystem controls bind these states in
/// `atpkg`'s retention and deep-removal tests. The boolean escaped state abstracts possible
/// outstanding pieces; filesystem controls for nested restoration and an ACL-repair retry
/// check the queue/active-piece bound separately.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_deep_removal_fence_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgDeepRemovalFence {
            const Buggy = 0;
            var phase = 0;
            var fence = 0;
            var owned = 0;
            var escaped = 0;
            var explicit = 0;
            var removed = 0;
            var unsafe_remove = 0;

            action Arm when (phase == 0) {
                fence = 1;
                owned = 1;
                phase = 1;
            }
            action Move when (phase == 1 && owned == 1) {
                escaped = 1;
                phase = 2;
            }
            action MoveBeforeFence when (Buggy == 1 && phase == 0) {
                escaped = 1;
                phase = 2;
            }
            action ExplicitKeep when (phase > 0 && phase <= 2 && explicit == 0) {
                explicit = 1;
                fence = 1;
                owned = if Buggy == 2 { owned } else { 0 };
            }
            action AdmitRemoval when (phase == 2 && owned == 1) {
                removed = 1;
                unsafe_remove = explicit;
                escaped = 0;
                phase = 4;
            }
            action Restore when (phase == 2 && owned == 1 && explicit == 0) {
                escaped = 0;
                phase = 4;
            }
            action RestoreFailedOrCrash when (phase == 2) {
                owned = 0;
                fence = if Buggy == 3 { 0 } else { fence };
                phase = 3;
            }
            action LaterPass when (phase == 3 && (fence == 0 || Buggy == 4)) {
                removed = 1;
                unsafe_remove = 1;
                phase = 4;
            }
            action Disarm when (phase == 4 && owned == 1 && explicit == 0 && escaped == 0) {
                fence = 0;
                owned = 0;
                phase = 5;
            }

            invariant EveryEscapeAlreadyFenced: escaped == 0 || fence == 1;
            invariant ExplicitKeepRevokesExemption: explicit == 0 || owned == 0;
            invariant NoNewAdmissionAfterRevocation: unsafe_remove == 0;
            bound Bounds: phase <= 5 && fence <= 1 && owned <= 1 && escaped <= 1
                && explicit <= 1 && removed <= 1 && unsafe_remove <= 1;
        }
    }
}
