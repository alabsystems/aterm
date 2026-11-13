// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The test run's budget dispatcher (`aterm_verify::test_plan::Dispatcher`,
//! `docs/DESIGN-fast-build-release-2026-11-05.md` §4.4 A9).

use aterm_spec::derive::*;

/// Four test binaries considered in the plan's priority order on a share of `Share` cores and a bound of
/// `Memory` bytes: binary `i` holds `Ci` cores and `Pi` bytes while it runs. The first fitting queued binary
/// starts only while its charge fits in what the running binaries' charges leave of the share and
/// its peak in what their peaks leave of the claim, and, beside a running binary, only while the
/// running binaries' live rate (`live`, which their reads move a core at a time while one runs)
/// and its charge stay within the share and one core. An ending binary gives both back. Once halted (the run's ceiling, or a
/// red under fail-fast) nothing more starts.
///
/// The constants make each rule bind somewhere: the second binary beside the first passes the
/// share and fits the claim; the third beside the second fits the share and passes the claim; the
/// fourth beside the third fits both, so only the live rate can hold it back. A blocked earlier
/// binary cannot prevent that fourth from using spare capacity. Each charge is at
/// most the share and each peak at most the claim, so the head always starts once nothing runs.
///
/// The production projection: `pi` is binary `i`'s phase (0 queued, 1 running, 2 done), `next`
/// the number started plus one (5 once all started), `held` and `bytes`
/// what the dispatcher counts held, `halted` whether it halted. `started_halted` and `extra` are
/// what only a mutant moves.
///
/// Buggy 1 is each of the dispatchers the design's negative controls name and more: one that
/// admits while the free share is below the charge (`StartOverShare2`), one that ignores the claim
/// (`StartOverMemory3`), one that releases a charge twice (`ReleaseAgain1`), one that ends a
/// binary keeping what it held (`FinishKeepingCharge2`), and one that starts after a halt
/// (`StartAfterHalt1`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn test_dispatch_model() -> Model {
    aterm_spec::ty_model! {
        TestDispatch {
            const Buggy = 0;
            const Share = 3;
            const Memory = 2;
            const C1 = 2;
            const C2 = 2;
            const C3 = 1;
            const C4 = 1;
            const P1 = 1;
            const P2 = 1;
            const P3 = 2;
            const P4 = 0;

            var p1 = 0;
            var p2 = 0;
            var p3 = 0;
            var p4 = 0;
            var next = 1;
            var held = 0;
            var bytes = 0;
            var live = 0;
            var halted = 0;
            var started_halted = 0;
            var extra = 0;

            action Start1 when (halted == 0 && p1 == 0 && held + C1 <= Share
                && bytes + P1 <= Memory && (held == 0 || live + C1 <= Share + 1)) {
                p1 = 1;
                next = next + 1;
                held = held + C1;
                bytes = bytes + P1;
            }
            action Start2 when (halted == 0 && p2 == 0 && (p1 > 0 || held + C1 > Share || bytes + P1 > Memory || (held > 0 && live + C1 > Share + 1)) && held + C2 <= Share
                && bytes + P2 <= Memory && (held == 0 || live + C2 <= Share + 1)) {
                p2 = 1;
                next = next + 1;
                held = held + C2;
                bytes = bytes + P2;
            }
            action Start3 when (halted == 0 && p3 == 0 && (p1 > 0 || held + C1 > Share || bytes + P1 > Memory || (held > 0 && live + C1 > Share + 1)) && (p2 > 0 || held + C2 > Share || bytes + P2 > Memory || (held > 0 && live + C2 > Share + 1)) && held + C3 <= Share
                && bytes + P3 <= Memory && (held == 0 || live + C3 <= Share + 1)) {
                p3 = 1;
                next = next + 1;
                held = held + C3;
                bytes = bytes + P3;
            }
            action Start4 when (halted == 0 && p4 == 0 && (p1 > 0 || held + C1 > Share || bytes + P1 > Memory || (held > 0 && live + C1 > Share + 1)) && (p2 > 0 || held + C2 > Share || bytes + P2 > Memory || (held > 0 && live + C2 > Share + 1)) && (p3 > 0 || held + C3 > Share || bytes + P3 > Memory || (held > 0 && live + C3 > Share + 1)) && held + C4 <= Share
                && bytes + P4 <= Memory && (held == 0 || live + C4 <= Share + 1)) {
                p4 = 1;
                next = next + 1;
                held = held + C4;
                bytes = bytes + P4;
            }
            action Finish1 when (p1 == 1) {
                p1 = 2;
                held = held - C1;
                bytes = bytes - P1;
            }
            action Finish2 when (p2 == 1) {
                p2 = 2;
                held = held - C2;
                bytes = bytes - P2;
            }
            action Finish3 when (p3 == 1) {
                p3 = 2;
                held = held - C3;
                bytes = bytes - P3;
            }
            action Finish4 when (p4 == 1) {
                p4 = 2;
                held = held - C4;
                bytes = bytes - P4;
            }
            action LiveRises when (held > 0 && live <= Share) {
                live = live + 1;
            }
            action LiveFalls when (held > 0 && live > 0) {
                live = live - 1;
            }
            action Halt when (halted == 0) {
                halted = 1;
            }

            action StartOverShare2 when (Buggy == 1 && halted == 0 && next == 2
                && bytes + P2 <= Memory) {
                p2 = 1;
                next = 3;
                held = held + C2;
                bytes = bytes + P2;
            }
            action StartOverMemory3 when (Buggy == 1 && halted == 0 && next == 3
                && held + C3 <= Share) {
                p3 = 1;
                next = 4;
                held = held + C3;
                bytes = bytes + P3;
            }
            action ReleaseAgain1 when (Buggy == 1 && p1 == 2 && extra == 0) {
                held = held - C1;
                bytes = bytes - P1;
                extra = 1;
            }
            action FinishKeepingCharge2 when (Buggy == 1 && p2 == 1) {
                p2 = 2;
            }
            action StartAfterHalt1 when (Buggy == 1 && halted == 1 && next == 1) {
                p1 = 1;
                next = 2;
                held = held + C1;
                bytes = bytes + P1;
                started_halted = 1;
            }

            invariant ChargesWithinShare: (if (p1 == 1) { C1 } else { 0 })
                + (if (p2 == 1) { C2 } else { 0 }) + (if (p3 == 1) { C3 } else { 0 })
                + (if (p4 == 1) { C4 } else { 0 }) <= Share;
            invariant PeaksWithinClaim: (if (p1 == 1) { P1 } else { 0 })
                + (if (p2 == 1) { P2 } else { 0 }) + (if (p3 == 1) { P3 } else { 0 })
                + (if (p4 == 1) { P4 } else { 0 }) <= Memory;
            invariant HeldIsTheRunningCharges: held == (if (p1 == 1) { C1 } else { 0 })
                + (if (p2 == 1) { C2 } else { 0 }) + (if (p3 == 1) { C3 } else { 0 })
                + (if (p4 == 1) { C4 } else { 0 });
            invariant BytesAreTheRunningPeaks: bytes == (if (p1 == 1) { P1 } else { 0 })
                + (if (p2 == 1) { P2 } else { 0 }) + (if (p3 == 1) { P3 } else { 0 })
                + (if (p4 == 1) { P4 } else { 0 });
            invariant NothingStartsAfterHalt: started_halted == 0;
            bound DispatchIsFinite: p1 <= 2 && p2 <= 2 && p3 <= 2 && p4 <= 2 && next <= 5
                && live <= Share + 1 && extra <= 1;
        }
    }
}

/// Every queued binary starts unless the run halts: `[]<>(halted == 1 || next == 5)`, with each
/// start and finish weakly fair. Backfill can start the fourth binary before the third, so its
/// finish now shares the same progress assumption. The live rate holds a binary back only beside
/// a running one, and a running one ends, so the head starts once nothing runs. Ending a binary
/// while keeping what it held (`FinishKeepingCharge2`) leaves the third binary's peak no room, ever.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(test_dispatch_model)]
pub fn test_dispatch_liveness() -> Liveness {
    Liveness {
        name: "EveryQueuedBinaryStarts",
        goal: or_(eq(var("halted"), int(1)), eq(var("next"), int(5))),
        weak: vec![
            "Start1", "Start2", "Start3", "Start4", "Finish1", "Finish2", "Finish3", "Finish4",
        ],
        strong: vec![],
        mutants: vec!["FinishKeepingCharge2"],
    }
}
