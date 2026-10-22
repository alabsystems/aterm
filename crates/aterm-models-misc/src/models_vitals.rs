// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE STUCK VERDICT (`docs/DESIGN-system-awareness-2026-10-16.md` §11.7,
//! §17): how one job's verdict moves with its class, its suspect quiet and
//! the readings, in `aterm_vitals::jobs::step::classify_tick`. THE SCOPE
//! BALANCE (§7.7, §17): how `aterm_vitals::explain::split` divides the busy
//! CPU of one window into parts that add up.

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

/// THE SCOPE BALANCE, CPU (`docs/DESIGN-system-awareness-2026-10-16.md` §7.7,
/// §17): how `aterm_vitals::explain::split` divides the machine's busy CPU
/// over one matched window into the caller's tab, the rest of aterm, others,
/// services and a signed residual.
///
/// The readings, in units of work over the window (each environment action
/// adds one): `g` the cores' busy figure; `a` aterm's coalitions `C(I)`; `t`
/// the caller tab's live members in them over the split's window, and `tf`
/// over the last sweep's window (the fast set); `d` the tab's delegated
/// members under a launcher outside aterm, live; `l` that launcher's
/// coalition, `d`'s work in it; `o` the user's other coalitions; `s` the
/// services' coalitions. `Move` moves the delegated work to aterm's side,
/// capped at the launcher's figure, and takes the tab's figure; `Close`
/// makes the rest of aterm and the residual. `fresh` is 0 once a reading
/// moved, 1 after `Move`, 2 after `Close`: the laws read the printed parts.
///
/// Laws (each broken by its own mutant):
///
/// * `PartsSumToWhole`: the four parts and the residual add up to the busy
///   figure exactly (Buggy 2 `ChargeSharedLauncher`: the launcher keeps the
///   work its tab counts; Buggy 1 too);
/// * `PartsNeverNegative` (Buggy 3: the delegated subtraction uncapped, so a
///   member read ahead of its launcher's counter takes the launcher below 0);
/// * `ResidualIsShown`: the residual is the work outside every listed
///   coalition, signed, plus the tab's live figure past aterm's coalitions,
///   never clamped and never folded into a part (Buggy 1
///   `ClampResidualSilently`; Buggy 4 `ServicesSwallowUnaccountedWithLaneOn`,
///   the built split's subtraction with the lane on);
/// * `DelegatedCountedOnce`: the launcher gives up exactly what the split
///   moved to aterm's side (Buggy 2);
/// * `MatchedWindowsOnly`: every part is over the split's window, so the
///   printed tab is its live members' work over it, `t + d` (Buggy 5
///   `MixFastAndSplitWindows`: the tab's figure from the last sweep, a 1 s
///   rate against a 5 s split, shows once the two differ).
///
/// Tier-1: `aterm-vitals`' `tests/conformance_scope_balance.rs` reads scenes
/// through the real `Vitals::absorb`, projects each scene's readings and the
/// split's parts onto this model, and requires `Move` and then `Close` to
/// take the one to the other with every law holding. NEGATIVE CONTROLS: the
/// real split with one injected defect (`aterm_vitals::explain::Fault`) is
/// refused at Buggy 0 and admitted only by its own mutant, which breaks its
/// own law — services by subtraction with the lane on (Buggy 4,
/// `ResidualIsShown`) and the tab over the sweep's window (Buggy 5,
/// `MatchedWindowsOnly`). No `#[refines]` anchors: the binding is the
/// conformance test.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scope_balance_model() -> Model {
    aterm_spec::ty_model! {
        ScopeBalance {
            const Buggy = 0;
            const Work = 3;
            var n = 0;
            var g = 0;
            var a = 0;
            var t = 0;
            var tf = 0;
            var d = 0;
            var l = 0;
            var o = 0;
            var s = 0;
            var moved = 0;
            var tab = 0;
            var rest = 0;
            var others = 0;
            var services = 0;
            var residual = 0;
            var fresh = 2;

            // Work in aterm's coalitions beside the caller tab's live
            // members: aterm itself, its helpers, other tabs, finished work.
            action AtermWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                a = a + 1;
                fresh = 0;
            }
            // The caller tab's live members work, in aterm's coalitions.
            action TabWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                a = a + 1;
                t = t + 1;
                tf = tf + 1;
                fresh = 0;
            }
            // A build starts: the tab's last-sweep rate runs past its
            // window's average.
            action BuildStarts when (n <= Work - 1) {
                n = n + 1;
                tf = tf + 1;
                fresh = 0;
            }
            // The tab's live figure read ahead of aterm's coalition counter.
            action TabAhead when (n <= Work - 1) {
                n = n + 1;
                t = t + 1;
                tf = tf + 1;
                fresh = 0;
            }
            action LauncherWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                l = l + 1;
                fresh = 0;
            }
            // A delegated member works: its launcher's coalition counts it,
            // and so does its tab.
            action DelegatedWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                l = l + 1;
                d = d + 1;
                fresh = 0;
            }
            // A delegated member's live figure read ahead of its launcher's
            // counter.
            action DelegatedAhead when (n <= Work - 1) {
                n = n + 1;
                d = d + 1;
                fresh = 0;
            }
            action OthersWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                o = o + 1;
                fresh = 0;
            }
            action ServicesWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                s = s + 1;
                fresh = 0;
            }
            // Work no listed coalition read: a kernel thread, a coalition
            // reaped inside the window.
            action Unread when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                fresh = 0;
            }
            // A coalition's counter read past the end of the cores' window.
            action CounterAhead when (n <= Work - 1) {
                n = n + 1;
                s = s + 1;
                fresh = 0;
            }

            action Move when (fresh == 0) {
                moved = if (Buggy == 3 || d <= l) { d } else { l };
                others = if (Buggy == 2) {
                    o + l
                } else {
                    if (Buggy == 3 || d <= l) { o + l - d } else { o }
                };
                tab = if (Buggy == 5) { tf + d } else { t + d };
                fresh = 1;
            }
            action Close when (fresh == 1) {
                rest = if (tab <= a + moved) { a + moved - tab } else { 0 };
                services = if (Buggy == 4 && g > a + l + o + s) { g - a - l - o } else { s };
                residual = if (Buggy == 4 && g > a + l + o + s) {
                    if (a + moved <= tab) { a + moved - tab } else { 0 }
                } else {
                    if (Buggy == 1
                        && g + (if (a + moved <= tab) { a + moved - tab } else { 0 })
                            <= a + l + o + s)
                    {
                        0
                    } else {
                        g - a - l - o - s + (if (a + moved <= tab) { a + moved - tab } else { 0 })
                    }
                };
                fresh = 2;
            }

            invariant PartsSumToWhole:
                fresh <= 1 || g == tab + rest + others + services + residual;
            invariant PartsNeverNegative:
                fresh <= 1 || (0 <= tab && 0 <= rest && 0 <= others && 0 <= services);
            invariant ResidualIsShown:
                fresh <= 1
                    || residual
                        == g - a - l - o - s
                            + (if (a + moved <= tab) { a + moved - tab } else { 0 });
            invariant DelegatedCountedOnce: fresh == 0 || others + moved == o + l;
            invariant MatchedWindowsOnly: fresh == 0 || tab == t + d;
            bound Box: n <= Work;
        }
    }
}
