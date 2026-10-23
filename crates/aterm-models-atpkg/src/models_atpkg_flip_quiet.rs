// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Flip-when-quiet model for staged Trust toolchain updates (`atpkg::quiet::decide`).

use aterm_spec::derive::*;

/// Staged Trust toolchain waiting for its flip (`NoFlipUnderABuild`, `RevokedNeverWaits`,
/// `WaitIsBounded`). Flips when revoked, quiet, or at `Ceiling`, and holds otherwise; `Buggy = 1`
/// breaks each invariant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_flip_quiet_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgFlipQuiet {
            const Buggy = 0;
            const Ceiling = 3;
            var pending = 1;
            var busy = 0;
            var forced = 0;
            var age = 0;
            var looked = 0;
            var early = 0;
            var stalled = 0;

            action Use when (busy == 0) {
                busy = 1;
            }
            action Unsure when (busy == 0) {
                busy = 2;
            }
            action Quiet when (busy > 0) {
                busy = 0;
            }
            action Revoke when (forced == 0) {
                forced = 1;
            }

            action Flip when (
                pending == 1
                    && looked == 0
                    && (forced == 1 || busy == 0 || Ceiling <= age || (Buggy == 1 && busy == 2))
            ) {
                early = if forced == 0 && busy > 0 && age <= Ceiling - 1 { 1 } else { early };
                pending = 0;
                age = 0;
                looked = 1;
            }
            action Hold when (
                pending == 1
                    && looked == 0
                    && ((forced == 0 && busy > 0 && age <= Ceiling - 1) || (Buggy == 1 && busy > 0))
            ) {
                stalled = if forced == 1 { 1 } else { stalled };
                looked = 1;
            }
            action Tick when (pending == 1 && looked == 1 && age <= Ceiling) {
                age = age + 1;
                looked = 0;
            }
            action Publish when (pending == 0) {
                pending = 1;
                looked = 0;
            }

            invariant NoFlipUnderABuild: early == 0;
            invariant RevokedNeverWaits: stalled == 0;
            invariant WaitIsBounded: age <= Ceiling;
        }
    }
}
