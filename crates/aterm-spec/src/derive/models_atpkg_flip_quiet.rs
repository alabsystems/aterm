// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Flip when quiet (gap #12(b)): an unattended update stages a new Trust toolchain and
//! flips it only when nothing is using the one it replaces — the decision
//! `atpkg::quiet::decide`, looked at once per pass or park re-check.

use super::*;

/// A staged Trust toolchain waiting for its flip, one look per period. `pending` is whether
/// a staged build waits; `busy` what the toolchain it replaces is doing — 0 quiet, 1 in
/// use (a run's lease on it, or a process running from it), 2 cannot be told (the process
/// table or the leases unreadable); `forced` whether the live build is revoked (yanked,
/// below the floor, tombstoned); `age` how many periods the flip has waited; `looked`
/// whether this period's look ran. The look FLIPS when the build is revoked, the toolchain
/// is quiet or the wait reached `Ceiling`, and otherwise HOLDS; a period passes only after
/// its look (`Tick`).
///
/// Three claims, each with its outcome variable: a flip never lands under a build — in use
/// OR not known — before the ceiling, unforced (`NoFlipUnderABuild`, over `early`); a
/// revoked build's flip is never held (`RevokedNeverWaits`, over `stalled`); and the wait
/// is bounded (`WaitIsBounded`).
///
/// `Buggy=1` is the three ways the decision gets this wrong, one per claim: an unreadable
/// table read as quiet (the flip lands under a build nobody could see), a hold that ignores
/// revocation, and a hold that ignores the ceiling. Tier-1 (`atpkg::quiet`'s tests) drives
/// the real `decide` and the real `Busy` classification over every reachable look, with
/// the unknown-is-quiet decision as the caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_flip_quiet_model() -> Model {
    crate::ty_model! {
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

            // The world: a build starts or ends, the table stops answering, a build is
            // recalled.
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

            // The look: flip when revoked, quiet, or at the ceiling…
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
            // …else hold.
            action Hold when (
                pending == 1
                    && looked == 0
                    && ((forced == 0 && busy > 0 && age <= Ceiling - 1) || (Buggy == 1 && busy > 0))
            ) {
                stalled = if forced == 1 { 1 } else { stalled };
                looked = 1;
            }
            // A period passes after its look.
            action Tick when (pending == 1 && looked == 1 && age <= Ceiling) {
                age = age + 1;
                looked = 0;
            }
            // A newer toolchain is staged once the last one flipped.
            action Publish when (pending == 0) {
                pending = 1;
                looked = 0;
            }

            // No flip lands under a build (in use or not known) before the ceiling unforced.
            invariant NoFlipUnderABuild: early == 0;
            // A revoked build's flip is never held.
            invariant RevokedNeverWaits: stalled == 0;
            // The wait is bounded by the ceiling.
            invariant WaitIsBounded: age <= Ceiling;
        }
    }
}
