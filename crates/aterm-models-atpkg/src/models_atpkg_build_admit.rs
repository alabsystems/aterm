// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lock order of a plain build's memory claim (`atpkg::build_admit`, BUILD-ADMIT).

use aterm_spec::derive::*;

/// Two builders of one checkout share its build directory, whose lock cargo takes once a build
/// starts, and the capacity ledger, which admits one of their heavy claims at a time, first come
/// first served (each needs more than the other leaves; a claim that needs more than can ever be
/// available runs alone, which admits it the same way). Builder A is a verify stage: it claims,
/// then starts its build, which takes the lock. Builder B is a plain build under
/// `build_admit::supervise`, which claims before it spawns `targo`, so its lock is taken under its
/// claim too. `Buggy = 1` is the rejected wrapper's order: B's build takes the lock first and
/// claims from inside it, so A, holding its claim, waits for the lock while B, holding the lock,
/// waits for A's claim, and neither moves. Tier 1 (atpkg's
/// `a_builds_claim_is_held_before_its_targo_starts_as_the_model_orders`) binds the order: a
/// stand-in `targo` that `build_admit::run_if_claimed_at` ran, as a call runs one (classify,
/// admit, then supervise's spawn), finds its parent's claim held at its start, once; the lab's
/// claimed builds start only through `aterm-lab`'s `util::spawn_claimed`, which takes the claim
/// (`a_claimed_lab_build_starts_under_its_claim`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_build_admit_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgBuildAdmit {
            const Buggy = 0;
            // A: 0 idle, 1 waits for its claim, 2 claimed, 3 builds (claim and lock), 4 done.
            var a = 0;
            // B: as A, and 5 holds the lock with no claim (the wrapper's order).
            var b = 0;
            // The build directory's lock: 0 free, 1 A's, 2 B's.
            var lock = 0;
            // The heavy claim the ledger holds: 0 none, 1 A's, 2 B's.
            var held = 0;
            // Each waiter's place in the ledger's queue (0: not waiting), and the next place.
            var qa = 0;
            var qb = 0;
            var tick = 0;

            action AEnter when (a == 0) {
                a = 1;
                qa = tick + 1;
                tick = tick + 1;
            }
            action AAdmit when (a == 1 && held == 0 && (qb == 0 || qb > qa)) {
                held = 1;
                qa = 0;
                a = 2;
            }
            action ALock when (a == 2 && lock == 0) {
                lock = 1;
                a = 3;
            }
            action ADone when (a == 3) {
                lock = 0;
                held = 0;
                a = 4;
            }

            action BEnter when ((b == 0 && Buggy == 0) || b == 5) {
                b = 1;
                qb = tick + 1;
                tick = tick + 1;
            }
            action BLockFirst when (b == 0 && Buggy == 1 && lock == 0) {
                lock = 2;
                b = 5;
            }
            action BAdmit when (b == 1 && held == 0 && (qa == 0 || qa > qb)) {
                held = 2;
                qb = 0;
                b = if lock == 2 { 3 } else { 2 };
            }
            action BLock when (b == 2 && lock == 0) {
                lock = 2;
                b = 3;
            }
            action BDone when (b == 3) {
                lock = 0;
                held = 0;
                b = 4;
            }

            // A builder holds the build directory's lock only under its own claim.
            invariant LockUnderClaim: lock == 0 || lock == held;
            // Never does each builder wait on what the other holds: A for B's lock, B's claim or
            // B's earlier place in the queue, while B waits on A's the same way.
            invariant NoWaitCycle:
                (if (a == 2 && lock == 2) || (a == 1 && (held == 2 || (qb > 0 && qa > qb))) {
                    1
                } else {
                    0
                }) + (if (b == 2 && lock == 1) || (b == 1 && (held == 1 || (qa > 0 && qb > qa))) {
                    1
                } else {
                    0
                }) <= 1;
        }
    }
}
