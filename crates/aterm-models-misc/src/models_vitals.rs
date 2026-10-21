// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE STUCK VERDICT (`docs/DESIGN-system-awareness-2026-10-16.md` §11.7,
//! §17): how one job's verdict moves with its class, its suspect quiet and
//! the readings, in `aterm_vitals::jobs::step::classify_tick`.

use aterm_spec::derive::*;

/// One job's verdict machine, scalar: `cls` is how the job counts this tick
/// (0 running, 1 a legitimate wait in effect, 2 a stuck-capable suspect, 3 a
/// suspect capped at quiet, 4 exited); `q` is its suspect quiet in φ bands
/// (`T3` the band where φ reaches 3, `T8` where it reaches 8); `v` is the
/// verdict's rank (0 fine or a legitimate word, 1 quiet/busy/slow, 2 stuck).
/// One latch per law records the way the classifier could be wrong, and each
/// latch can be set only under its own `Buggy` value.
///
/// Laws (each broken by its own mutant):
///
/// * `NeverStuckWhileProgressing` (Buggy 1: progress keeps the verdict);
/// * `LegitWaitNeverAccrues` (Buggy 2: a legitimate wait's age accrues);
/// * `RefusedReadingIsNotZeroProgress` (Buggy 3: a refused reading resets
///   the age — unknown is never zero);
/// * `UnnamedNeverEntersStuck` (Buggy 4: a capped wait reaches stuck);
/// * `NoFlap` (Buggy 5: stuck falls to quiet while φ ≥ 3 because μ grew);
/// * `FlaggedOnTime` (Buggy 6: a named stuck-capable wait is never raised);
/// * `AgeResetsOnlyOnProgress`: the suspect quiet restarts only on progress,
///   a legitimate run or a new suspect run — broken by the three
///   environment mutants the liveness obligation names
///   ([`stuck_verdict_liveness`]): a CR redraw, idle churn, or a refused
///   reading taken for progress.
///
/// Why `FlaggedOnTime` is not a ghost: with no mutant, every path to
/// `cls == 2 && q ≥ T8` sets `v = 2` on the way (`QuietTick` from
/// `q = T8 − 1`, or `Relabel` from a capped wait at `q ≥ T8`), and the only
/// action that lowers `v` (`MuGrows`) lowers `q` below `T8` first, so `Watch`
/// never latches `late`. Under Buggy 6 `QuietTick` leaves a named wait at
/// quiet while `q` reaches `T8`.
///
/// `MuGrows` is bounded (`grows ≤ GrowCap` per suspect run): μ grows from a
/// finite set of intervals and a bounded stretch, so φ cannot be pushed back
/// for ever; without the bound an endless grow-and-tick loop would starve the
/// flag and no fairness could rescue the liveness.
///
/// Tier-1: `aterm-vitals`' `tests/conformance_stuck_verdict.rs` replays every
/// scenario of the design's §18 table through the real `classify_tick`
/// (through `Jobs::absorb`), projects each tick onto `(cls, q band, v)` and
/// fires the action the tick's own record names (progress, a refused reading,
/// μ grown, the class's change, the band's move); every step must be enabled
/// here and every law hold after it. NEGATIVE CONTROLS: the real step with
/// one injected defect (`aterm_vitals::jobs::Fault`) is rejected at Buggy 0
/// and admitted only by its own mutant, which breaks its own law — φ accrued
/// on `tail -f` (`LegitWaitNeverAccrues`), the age reset on an EPERM reading
/// (`RefusedReadingIsNotZeroProgress`), an unnamed `flock` wait let reach
/// stuck (`UnnamedNeverEntersStuck`), a silent TCP peer never raised
/// (`FlaggedOnTime`); and a CR redraw, idle churn and M7's logical-write noise
/// taken for progress fail the scenarios' flags (the liveness side). No
/// `#[refines]` anchors: the binding is the conformance test.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn stuck_verdict_model() -> Model {
    aterm_spec::ty_model! {
        StuckVerdict {
            const Buggy = 0;
            const T3 = 2;
            const T8 = 4;
            const QMax = 4;
            const GrowCap = 4;
            var cls = 2;
            var q = 0;
            var v = 0;
            var grows = 0;
            var stuck_while_progressing = 0;
            var legit_accrued = 0;
            var refused_moved = 0;
            var capped_stuck = 0;
            var flapped = 0;
            var late = 0;
            var faked = 0;

            // LF output, side writes, churn with work, typed input read.
            action Progress when (cls <= 3) {
                stuck_while_progressing = if (Buggy == 1 && v == 2) { 1 } else { stuck_while_progressing };
                v = if (Buggy == 1) { v } else { 0 };
                cls = 0;
                q = 0;
                grows = 0;
            }
            // Time passes in a suspect wait: φ climbs one band.
            action QuietTick when (2 <= cls && cls <= 3 && q <= QMax - 1) {
                capped_stuck = if (cls == 3 && T8 <= q + 1 && Buggy == 4) { 1 } else { capped_stuck };
                // Buggy 6: a named suspect wait is never raised to stuck.
                v = if (T8 <= q + 1 && cls == 2) {
                    if (Buggy == 6) { if (v == 0) { 1 } else { v } } else { 2 }
                } else {
                    if (T8 <= q + 1 && cls == 3 && Buggy == 4) {
                        2
                    } else {
                        if (T3 <= q + 1 && v == 0) { 1 } else { v }
                    }
                };
                q = q + 1;
            }
            // EPERM or no reading: unknown, never zero progress. (Buggy 3: the
            // age restarts, and with φ under 1 the verdict falls to fine.)
            action RefusedTick when (2 <= cls && cls <= 3) {
                refused_moved = if (Buggy == 3 && q > 0) { 1 } else { refused_moved };
                v = if (Buggy == 3) { 0 } else { v };
                q = if (Buggy == 3) { 0 } else { q };
            }
            action LegitTick when (cls == 1) {
                legit_accrued = if (Buggy == 2) { 1 } else { legit_accrued };
                q = if (Buggy == 2 && q <= QMax - 1) { q + 1 } else { 0 };
                v = 0;
            }
            // μ grows (its own median, its class's, the stretch): φ falls one
            // band; stuck holds while φ ≥ 3, quiet leaves under it.
            action MuGrows when (2 <= cls && cls <= 3 && q > 0 && grows <= GrowCap - 1) {
                flapped = if (Buggy == 5 && v == 2 && T3 <= q - 1) { 1 } else { flapped };
                v = if (v == 2 && T3 <= q - 1) {
                    if (Buggy == 5) { 1 } else { 2 }
                } else {
                    if (q - 1 <= T3 - 1) { 0 } else { v }
                };
                q = q - 1;
                grows = grows + 1;
            }
            action BecomeLegit when (2 <= cls && cls <= 3) {
                v = 0;
                cls = 1;
                q = 0;
                grows = 0;
            }
            action BecomeSuspect when (cls <= 1) {
                cls = 2;
                q = 0;
                grows = 0;
            }
            // A capped wait gets its name, or a named one loses it.
            action Relabel when (2 <= cls && cls <= 3) {
                v = if (cls == 3 && T8 <= q) { if (Buggy == 6) { v } else { 2 } } else { v };
                cls = if (cls == 2) { 3 } else { 2 };
            }
            action Exit when (cls <= 3) {
                v = 0;
                cls = 4;
            }
            // The observer: a named suspect wait at φ ≥ 8 must read stuck.
            action Watch when (cls == 2 && T8 <= q) {
                late = if (v == 2) { late } else { 1 };
            }
            // ENVIRONMENT MUTANTS (liveness): a reading taken for progress
            // that is none restarts the age while the class stays suspect.
            action CrRedrawAsProgress when (Buggy == 1 && cls == 2 && q > 0) {
                q = 0;
                v = if (v == 2) { v } else { 0 };
                faked = 1;
            }
            action IdleChurnAsProgress when (Buggy == 1 && cls == 2 && q > 0) {
                q = 0;
                v = if (v == 2) { v } else { 0 };
                faked = 1;
            }
            action RefusedResetsAge when (Buggy == 1 && cls == 2 && q > 0) {
                q = 0;
                v = if (v == 2) { v } else { 0 };
                faked = 1;
            }

            invariant NeverStuckWhileProgressing: stuck_while_progressing == 0;
            invariant LegitWaitNeverAccrues: legit_accrued == 0;
            invariant RefusedReadingIsNotZeroProgress: refused_moved == 0;
            invariant UnnamedNeverEntersStuck: capped_stuck == 0;
            invariant NoFlap: flapped == 0;
            invariant FlaggedOnTime: late == 0;
            invariant AgeResetsOnlyOnProgress: faked == 0;
        }
    }
}

/// `SuspectWaitIsEventuallyFlagged`: a stuck-capable suspect wait is flagged
/// or stops being one, again and again (goal `v == 2 || cls != 2`), with only
/// time's passing assumed (`QuietTick` weakly fair). This replaces the old
/// `AlwaysFlagsATrueStall`, which cannot hold for BUSY-SILENT (capped by
/// design). Each environment mutant alone breaks it: a reading taken for
/// progress that is none (a CR redraw, idle churn, a refused reading) can
/// restart the age for ever.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(stuck_verdict_model)]
pub fn stuck_verdict_liveness() -> Liveness {
    Liveness {
        name: "SuspectWaitIsEventuallyFlagged",
        goal: or_(eq(var("v"), int(2)), neq(var("cls"), int(2))),
        weak: vec!["QuietTick"],
        strong: vec![],
        mutants: vec![
            "CrRedrawAsProgress",
            "IdleChurnAsProgress",
            "RefusedResetsAge",
        ],
    }
}
