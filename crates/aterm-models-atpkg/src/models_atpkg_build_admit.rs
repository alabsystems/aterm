// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lock order of a plain build's memory claim (`atpkg::build_admit`, BUILD-ADMIT).

use aterm_spec::derive::*;

/// Two builders of one checkout share its build directory, whose lock cargo takes once a build
/// starts, and the capacity ledger, which admits one of their heavy claims at a time, first come
/// first served (each needs more than the other leaves; a claim that needs more than can ever be
/// available runs alone, which admits it the same way). Builder A is a verify stage: it claims,
/// then starts its build, which takes the lock. Builder B is a plain build through the shim's hop
/// (`build_admit::front_exec_bounded`): its front claims, hands the claim to a holder once the
/// holder arms (`keep` 1), and becomes the build (`keep` 2), whose lock is so taken under the claim
/// the holder keeps; where no holder arms it runs under `build_admit::supervise` (`keep` 0), which
/// claims before it spawns `targo`. The holder may die (fail open: the claim goes, `orphaned`), and
/// a build killed by a signal leaves its lock but not its claim while its survivors run (`b` 7).
/// Builder C is an unclaimed build of the same checkout (a rerouted `cargo build`): it takes the
/// lock, and a front its build script starts claims inside it in its configured mode. With no
/// verified covering claim, an ambiguous active lock permits one ordinary admission look: the front
/// runs if it fits, otherwise refuses without waiting, so it cannot close a cycle through
/// C's lock. D is a front a test or a `targo run` program starts, which
/// carries cargo's words but runs under no lock (its cargo holds none while it runs): it waits its
/// turn like any other, and so holds its claim beside no other claimed builder's
/// (`NoUnrequestedOvercommit`). B's front may first be held once for disk (`build_admit::DiskHold`:
/// its volume below its emergency line, the window's watch asked for one pass): the hold comes
/// before B enters the ledger's queue, so while it lasts B holds no place in the queue, no claim
/// and no lock (`HoldHoldsNothing`). It ends when the watch answers, or at its own bound
/// (`HoldBound` steps of its clock, `HoldBounded`) where the watch never answers (an older
/// `aterm`, a hung window): nobody's answer is needed for it to end. No front is held under a
/// possible build lock (C's inner front never is), so no claimed builder waits for a lock whose
/// build waits on a hold (`NoWaitOnAHold`); at or above the line there is no hold.
///
/// `Buggy = 1` is the rejected wrapper's order: B's build takes the lock first and claims from
/// inside it, so A, holding its claim, waits for the lock while B, holding the lock, waits for A's
/// claim, and neither moves. `Buggy = 2` execs before the hand-off: the claim's descriptor closes
/// with the exec, so B builds under the lock with no claim. `Buggy = 3` lets C's inner front wait
/// for its claim: A, holding its claim, waits for C's lock while C waits for its inner front,
/// which waits for A's claim. `Buggy = 4` lets B's holder take a watch that failed for the build's
/// end and let the claim go while the build still runs under the lock. `Buggy = 5` admits D at
/// once, as the rule that read cargo's words did, so its claim is held beside A's or B's.
/// `Buggy = 6` holds B for disk only once it entered (its place in the queue, or its claim,
/// kept through the hold). `Buggy = 7` lets only the watch's answer end the hold: its clock runs
/// past the bound, and a watch that never answers holds B for good. `Buggy = 8` holds C's inner
/// front for disk under the lock C holds, so a claimed builder waiting for that lock waits on the
/// hold.
/// Tier 1 (atpkg's
/// `a_builds_claim_is_held_before_its_targo_starts_as_the_model_orders`) binds the order: a
/// stand-in `targo` run through the real front finds a held claim in its own pid while it holds
/// no descriptor of it (the holder keeps it), and one that `build_admit::run_if_claimed_at` ran
/// finds its parent's claim held at its start; the lab's claimed builds start only through
/// `aterm-lab`'s `util::spawn_claimed`, which takes the claim
/// (`a_claimed_lab_build_starts_under_its_claim`); D's wait and C's inner front's admission are
/// bound through the real front by `build_hop`'s
/// `a_front_with_cargo_words_waits_without_a_verified_build_dependency`, the unlocked-file
/// regression and the real Cargo covered-build control; B's holder keeps its claim until
/// the build's last line by `build_hop`'s
/// `a_claimed_build_runs_in_its_fronts_pid_under_its_holders_claim`; the hold's laws by atpkg's
/// `a_build_started_below_the_emergency_line_is_held_for_one_pass_and_then_starts` (the ledger is
/// empty while it holds) for `HoldHoldsNothing`,
/// `a_build_started_at_or_above_the_emergency_line_is_never_held_for_disk` (no hold under a
/// possible build lock) for `NoWaitOnAHold`, and
/// `the_emergency_hold_ends_at_its_bound_when_the_watch_does_not_answer` (a watch that never
/// answers, the hold ended at its bound) for `HoldBounded`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_build_admit_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgBuildAdmit {
            const Buggy = 0;
            // The steps of its clock B's hold for disk lasts at most: its bound.
            const HoldBound = 1;
            // A: 0 idle, 1 waits for its claim, 2 claimed, 3 builds (claim and lock), 4 done.
            var a = 0;
            // B: as A, 5 holds the lock with no claim (the wrapper's order), 7 killed, its
            // claim kept for its survivors with the lock gone.
            var b = 0;
            // C: 0 idle, 1 builds under the lock it took unclaimed, 2 its inner front waits for
            // a claim, 3 that front holds one, 5 that front is done, 4 done.
            var c = 0;
            // The build directory's lock: 0 free, 1 A's, 2 B's, 3 C's.
            var lock = 0;
            // The heavy claim the ledger holds for A or B: 0 none, 1 A's, 2 B's.
            var held = 0;
            // Whether C's inner front holds a claim.
            var hc = 0;
            // D: 0 idle, 1 waits for its claim, 2 holds it, 3 done.
            var d = 0;
            // Who keeps B's claim: 0 its front, 1 its front and its armed holder, 2 its holder
            // alone (the front became the build), 3 nobody (it went).
            var keep = 0;
            // Whether B's build runs on with no claim because its holder died (fail open).
            var orphaned = 0;
            // Each waiter's place in the ledger's queue (0: not waiting), and the next place.
            var qa = 0;
            var qb = 0;
            var qc = 0;
            var qd = 0;
            var tick = 0;
            // B's hold for disk: 0 none, 1 holding, 2 over; how many steps of its clock it has
            // held; and the window's watch it asked: 0 not answered yet, 1 answered, 2 never
            // answers (an older `aterm`, a hung window).
            var hb = 0;
            var ht = 0;
            var hw = 0;
            // C's inner front held for disk under C's lock (the mutant alone): 0 never, 1 held,
            // 2 over.
            var hx = 0;

            action AEnter when (a == 0) {
                a = 1;
                qa = tick + 1;
                tick = tick + 1;
            }
            action AAdmit when (a == 1 && held == 0 && hc == 0 && (d <= 1 || d > 2) && (qb == 0 || qb > qa)
                && (qc == 0 || qc > qa) && (qd == 0 || qd > qa)) {
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

            action BEnter when ((b == 0 && (hb == 0 || hb == 2) && (Buggy == 0 || Buggy > 1))
                || b == 5) {
                b = 1;
                qb = tick + 1;
                tick = tick + 1;
            }
            action BLockFirst when (b == 0 && (hb == 0 || hb == 2) && Buggy == 1 && lock == 0) {
                lock = 2;
                b = 5;
            }
            // B's front is held once for disk before it enters, holding nothing.
            action BHold when (b == 0 && hb == 0 && (Buggy <= 5 || Buggy > 6)) {
                hb = 1;
                ht = 0;
            }
            // The mutant: the hold comes once B entered, its place or its claim kept through it.
            action BHoldLate when ((b == 1 || b == 2) && hb == 0 && Buggy == 6) {
                hb = 1;
            }
            // The watch B asked answers, or never will.
            action WAnswer when (hb == 1 && hw == 0) {
                hw = 1;
            }
            action WHang when (hb == 1 && hw == 0) {
                hw = 2;
            }
            // The hold's clock runs, and its bound ends the hold whatever the watch does; in the
            // mutant the clock runs on past the bound and ends nothing.
            action BHoldTick when (hb == 1 && ht <= HoldBound) {
                ht = ht + 1;
                hb = if Buggy == 7 || HoldBound > ht + 1 { 1 } else { 2 };
            }
            // The watch's answer ends the hold.
            action BHoldEnd when (hb == 1 && hw == 1) {
                hb = 2;
            }
            action BAdmit when (b == 1 && held == 0 && hc == 0 && (d <= 1 || d > 2) && (qa == 0 || qa > qb)
                && (qc == 0 || qc > qb) && (qd == 0 || qd > qb)) {
                held = 2;
                qb = 0;
                b = if lock == 2 { 3 } else { 2 };
            }
            // The holder arms: the front and the holder hold the claim's lock.
            action BArm when (b == 2 && keep == 0 && (Buggy <= 1 || Buggy > 2)) {
                keep = 1;
            }
            // The front hands its copy off and becomes the build.
            action BExec when (b == 2 && keep == 1) {
                keep = 2;
            }
            // The mutant: the front becomes the build with no holder, and its claim closes.
            action BExecEarly when (b == 2 && keep == 0 && Buggy == 2) {
                held = if held == 2 { 0 } else { held };
                keep = 3;
            }
            // The build takes the lock: the exec'd one, or the one the front supervises.
            action BLock when (b == 2 && (keep == 0 || keep > 1) && lock == 0) {
                lock = 2;
                b = 3;
            }
            // The mutant: the holder takes a watch that failed for the build's end, and lets its
            // claim go while the build still runs under the lock.
            action HEndEarly when (b == 3 && keep == 2 && held == 2 && Buggy == 4) {
                held = 0;
                keep = 3;
            }
            // The holder dies: its claim goes with it, and the build runs on (fail open).
            action HDie when (keep == 2 && held == 2) {
                held = 0;
                keep = 3;
                orphaned = 1;
            }
            action BDone when (b == 3) {
                lock = 0;
                held = if held == 2 { 0 } else { held };
                b = 4;
            }
            // A signal kills the exec'd build: its lock goes, its holder keeps the claim.
            action BKilled when (b == 3 && keep == 2) {
                lock = 0;
                b = 7;
            }
            action BSurvived when (b == 7) {
                held = if held == 2 { 0 } else { held };
                b = 4;
            }

            action CLock when (c == 0 && lock == 0) {
                lock = 3;
                c = 1;
            }
            // C's inner front claims: at once, as a front inside a running build does, or, in the
            // mutant, once it waited its turn in the ledger's queue.
            action CClaim when (((c == 1 && (Buggy <= 2 || Buggy > 3)) || c == 2) && held == 0
                && (d <= 1 || d > 2) && (qa == 0 || qa > qc) && (qb == 0 || qb > qc)
                && (qd == 0 || qd > qc)) {
                hc = 1;
                qc = 0;
                c = 3;
                hx = if hx == 1 { 2 } else { hx };
            }
            // No unproven dependency forces a claim: a first look that would wait refuses and
            // lets the outer build release its lock. A covered nested build needs no claim.
            action CRefused when (c == 1 && (Buggy <= 2 || Buggy > 3) && (held > 0 || d == 2
                || qa > 0 || qb > 0 || qd > 0)) {
                c = 5;
                hx = if hx == 1 { 2 } else { hx };
            }
            // The mutant: C's inner front, under the lock C holds, is held for disk until it
            // claims or refuses.
            action CHold when (c == 1 && hx == 0 && Buggy == 8) {
                hx = 1;
            }
            // The mutant: the inner front waits for its claim like any other.
            action CWait when (c == 1 && Buggy == 3) {
                c = 2;
                qc = tick + 1;
                tick = tick + 1;
            }
            action CInnerDone when (c == 3) {
                hc = 0;
                c = 5;
            }
            action CDone when (c == 5) {
                lock = 0;
                c = 4;
            }

            action DEnter when (d == 0) {
                d = 1;
                qd = tick + 1;
                tick = tick + 1;
            }
            // D's claim is admitted in its turn, as any waiter's; in the mutant, at once.
            action DAdmit when (d == 1 && (Buggy == 5 || (held == 0 && hc == 0
                && (qa == 0 || qa > qd) && (qb == 0 || qb > qd) && (qc == 0 || qc > qd)))) {
                d = 2;
                qd = 0;
            }
            action DDone when (d == 2) {
                d = 3;
            }

            // A claimed builder holds the build directory's lock only under its own claim, or once
            // its holder died (fail open); C is no claimed builder.
            invariant LockUnderClaim: lock == 0 || lock == 3 || lock == held || orphaned == 1;
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
            // Neither unclaimed nested work nor ordinary work may force a new claim beside a
            // held heavy claim. Explicit Now is outside this model's configured Wait callers.
            invariant NoUnrequestedOvercommit:
                (held == 0 || (hc == 0 && (d <= 1 || d > 2))) && (hc == 0 || d <= 1 || d > 2);
            // Never does a claimed builder hold its claim and wait for C's lock while C's inner
            // front waits for that claim.
            invariant NoNestedWaitCycle:
                (if a == 2 && held == 1 && lock == 3 && c == 2 { 1 } else { 0 })
                    + (if b == 2 && held == 2 && lock == 3 && c == 2 { 1 } else { 0 })
                    == 0;
            // While B is held for disk it holds no place in the queue, no claim and no lock.
            invariant HoldHoldsNothing:
                hb == 0 || hb == 2 || (qb == 0 && held <= 1 && (lock <= 1 || lock > 2));
            // The hold ends by its own bound.
            invariant HoldBounded: ht <= HoldBound;
            // No claimed builder waits for C's lock while a hold for disk holds C's inner front:
            // no front is held under a possible build lock.
            invariant NoWaitOnAHold:
                (if a == 2 && lock == 3 && hx == 1 { 1 } else { 0 })
                    + (if b == 2 && lock == 3 && hx == 1 { 1 } else { 0 })
                    == 0;
        }
    }
}
