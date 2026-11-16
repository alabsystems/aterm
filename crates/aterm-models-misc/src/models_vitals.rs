// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! System-awareness and process-termination models.

use aterm_spec::derive::*;

/// One job's verdict machine, scalar.
///
/// `held` is an input: macOS's program check holds the program the job waits in
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` §5.1). `Hold` puts the job in the
/// legitimate class (φ 0) and `BecomeSuspect` waits for its `Release`. Law `HeldWaitNeverQuiet`
/// (Buggy 7: `Hold` leaves the class as it was, so a held job accrues and reads quiet).
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
            var held = 0;
            var held_quiet = 0;

            action Progress when (cls <= 3) {
                stuck_while_progressing = if (Buggy == 1 && v == 2) { 1 } else { stuck_while_progressing };
                v = if (Buggy == 1) { v } else { 0 };
                cls = 0;
                q = 0;
                grows = 0;
            }
            action QuietTick when (2 <= cls && cls <= 3 && q <= QMax - 1) {
                capped_stuck = if (cls == 3 && T8 <= q + 1 && Buggy == 4) { 1 } else { capped_stuck };
                held_quiet = if (held == 1 && T3 <= q + 1) { 1 } else { held_quiet };
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
            action BecomeSuspect when (cls <= 1 && held == 0) {
                cls = 2;
                q = 0;
                grows = 0;
            }
            action Hold when (cls <= 3 && held == 0) {
                held = 1;
                cls = if (Buggy == 7) { cls } else { 1 };
                v = if (Buggy == 7) { v } else { 0 };
                q = if (Buggy == 7) { q } else { 0 };
                grows = 0;
            }
            action Release when (held == 1) {
                held = 0;
            }
            action Relabel when (2 <= cls && cls <= 3) {
                v = if (cls == 3 && T8 <= q) { if (Buggy == 6) { v } else { 2 } } else { v };
                cls = if (cls == 2) { 3 } else { 2 };
            }
            action Exit when (cls <= 3) {
                v = 0;
                cls = 4;
            }
            action Watch when (cls == 2 && T8 <= q) {
                late = if (v == 2) { late } else { 1 };
            }
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
            invariant HeldWaitNeverQuiet: held_quiet == 0;
        }
    }
}

/// END BY PROOF (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.4).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn end_by_proof_model() -> Model {
    aterm_spec::ty_model! {
        EndByProof {
            const Buggy = 0;
            var v = 0;
            var fresh = 0;
            var moved = 0;
            var lost = 0;
            var bad = 0;

            action Move when (moved == 0) {
                moved = 1;
            }
            action Lose when (lost == 0) {
                lost = 1;
            }
            action Judge when (v <= 1 && fresh == 0 && moved == 0 && lost == 0) {
                fresh = 1;
            }
            action Term when (v == 0 && (Buggy == 1 || (fresh == 1 && moved == 0 && lost == 0))) {
                bad = if (fresh == 0 || moved == 1 || lost == 1) { 1 } else { bad };
                fresh = 0;
                v = 1;
            }
            action Kill when (v == 1 && (Buggy == 1 || (fresh == 1 && moved == 0 && lost == 0))) {
                bad = if (fresh == 0 || moved == 1 || lost == 1) { 1 } else { bad };
                fresh = 0;
                v = 2;
            }
            action Stop when (v <= 1 && (moved == 1 || lost == 1)) {
                v = 2;
            }

            invariant NoUnprovenSignal: bad == 0;
        }
    }
}

/// `SuspectWaitIsEventuallyFlagged`.
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

/// THE SCOPE BALANCE, CPU (`docs/DESIGN-system-awareness-2026-10-16.md` §7.7, §17).
///
/// `a` is aterm's coalitions' counters, which hold the work of members that
/// exited inside the window (`x`, SA's `Container`); `d` is the caller tab's
/// live delegated members under a launcher outside aterm, `e` another tab's
/// under the same launcher. `l` is its raw coalition counter; independently
/// observed foreign own CPU (`f`) stays outside aterm even when that raw
/// counter trails the delegated or foreign floors. Scope totals retain those
/// floors without capping them to `l`, and a signed residual shows the skew.
/// Buggy 2 counts delegated work in both owner groups; Buggy 3 substitutes
/// raw `l - d - e` for the independent foreign floor. Law
/// `ExitedWorkStaysCounted` (Buggy 6, SA's `DropReapedChildTime`: aterm's
/// part is its live members' work and the reaped members' time is charged to
/// services, the reaper's).
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
            var e = 0;
            var x = 0;
            var l = 0;
            var f = 0;
            var o = 0;
            var s = 0;
            var moved = 0;
            var tab = 0;
            var rest = 0;
            var others = 0;
            var services = 0;
            var residual = 0;
            var fresh = 2;

            action AtermWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                a = a + 1;
                fresh = 0;
            }
            action TabWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                a = a + 1;
                t = t + 1;
                tf = tf + 1;
                fresh = 0;
            }
            action ExitedWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                a = a + 1;
                x = x + 1;
                fresh = 0;
            }
            action BuildStarts when (n <= Work - 1) {
                n = n + 1;
                tf = tf + 1;
                fresh = 0;
            }
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
                f = f + 1;
                fresh = 0;
            }
            action ForeignAhead when (n <= Work - 1) {
                n = n + 1;
                f = f + 1;
                fresh = 0;
            }
            action DelegatedWork when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                l = l + 1;
                d = d + 1;
                fresh = 0;
            }
            action DelegatedElsewhere when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                l = l + 1;
                e = e + 1;
                fresh = 0;
            }
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
            action Unread when (n <= Work - 1) {
                n = n + 1;
                g = g + 1;
                fresh = 0;
            }
            action CounterAhead when (n <= Work - 1) {
                n = n + 1;
                s = s + 1;
                fresh = 0;
            }

            action Move when (fresh == 0) {
                moved = d + e;
                others = if (Buggy == 2) {
                    o + f + d + e
                } else {
                    if (Buggy == 3) { o + l - d - e } else { o + f }
                };
                tab = if (Buggy == 5) { tf + d } else { t + d };
                fresh = 1;
            }
            action Close when (fresh == 1) {
                rest = if (tab <= a + moved) {
                    if (Buggy == 6) {
                        if (x <= a + moved - tab) { a + moved - tab - x } else { 0 }
                    } else {
                        a + moved - tab
                    }
                } else {
                    0
                };
                services = if (Buggy == 4 && g > a + moved + others + s) {
                    g - a - moved - others
                } else {
                    if (Buggy == 6 && tab <= a + moved) {
                        s + (if (x <= a + moved - tab) { x } else { a + moved - tab })
                    } else {
                        s
                    }
                };
                residual = if (Buggy == 4 && g > a + moved + others + s) {
                    if (a + moved <= tab) { a + moved - tab } else { 0 }
                } else {
                    if (Buggy == 1
                        && g + (if (a + moved <= tab) { a + moved - tab } else { 0 })
                            <= a + moved + others + s)
                    {
                        0
                    } else {
                        g - a - moved - others - s + (if (a + moved <= tab) { a + moved - tab } else { 0 })
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
                        == g - a - moved - others - s
                            + (if (a + moved <= tab) { a + moved - tab } else { 0 });
            invariant DelegatedCountedOnce:
                fresh == 0 || (moved == d + e && others == o + f);
            invariant MatchedWindowsOnly: fresh == 0 || tab == t + d;
            invariant ExitedWorkStaysCounted:
                fresh <= 1
                    || rest + tab
                        == a + moved + (if (a + moved <= tab) { tab - a - moved } else { 0 });
            bound Box: n <= Work;
        }
    }
}

/// Published CPU counters never qualify physical execution inside their read interval.
///
/// `prior` is execution from before the first read that can publish later; `actual` is
/// execution inside the interval, and `reported` is independently published counter growth.
/// Complete ownership (`complete`) cannot establish an execution window (`window`). The
/// diagnostic growth and independent machine context stay visible, while physical CPU roles,
/// shares, covering causes, residual bounds, absence claims and retained clauses do not.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn explain_honesty_model() -> Model {
    aterm_spec::ty_model! {
        ExplainHonesty {
            const Buggy = 0;
            var prior = 0;
            var actual = 0;
            var reported = 0;
            var complete = 1;
            var context = 0;
            var fresh = 0;
            var window = 0;
            var role = 0;
            var share = 0;
            var cause = 0;
            var residual = 0;
            var none = 0;
            var clause = 0;
            var diagnostic = 0;
            var shown_context = 0;

            action DeferBefore when (prior == 0 && actual == 0 && reported == 0) {
                prior = 4;
                fresh = 0;
            }
            action ExecuteUnpublished when (2 > actual) {
                actual = actual + 1;
                fresh = 0;
            }
            action PublishOne when (prior + actual > reported) {
                reported = reported + 1;
                fresh = 0;
            }
            action IncompleteCoverage when (complete == 1) {
                complete = 0;
                fresh = 0;
            }
            action MachineContext when (context == 0) {
                context = 1;
                fresh = 0;
            }
            action Evaluate when (fresh == 0) {
                window = if (Buggy == 7 && complete == 1) { 1 } else { 0 };
                role = if (Buggy == 1) { 1 } else { 0 };
                share = if (Buggy == 2) { 1 } else { 0 };
                cause = if (Buggy == 3) { 1 } else { 0 };
                residual = if (Buggy == 4) { 1 } else { 0 };
                none = if (Buggy == 5 && reported == 0) { 1 } else { 0 };
                clause = if (Buggy == 6) { 1 } else { 0 };
                diagnostic = if (Buggy == 8) { 0 } else { reported };
                shown_context = if (Buggy == 9) { 0 } else { context };
                fresh = 1;
            }

            invariant OwnershipDoesNotQualifyExecutionWindow: window == 0;
            invariant NoRoleWithoutExecutionWindow: fresh == 0 || role == 0;
            invariant NoShareWithoutExecutionWindow: fresh == 0 || share == 0;
            invariant NoCoveringCauseWithoutExecutionWindow: fresh == 0 || cause == 0;
            invariant NoPhysicalResidualWithoutExecutionWindow: fresh == 0 || residual == 0;
            invariant ZeroReportedNeverProvesNoWork: fresh == 0 || reported > 0 || none == 0;
            invariant NoRetainedPhysicalCpuCause: fresh == 0 || clause == 0;
            invariant ReportedFigureRetained: fresh == 0 || diagnostic == reported;
            invariant IndependentMachineContextRetained: fresh == 0 || shown_context == context;
            bound CounterBox: prior <= 4 && actual <= 2 && reported <= 6;
        }
    }
}

/// THE REAP CREDIT (`docs/DESIGN-system-awareness-2026-10-16.md` §7.5): a tab's cumulative CPU,
/// live and reaped, across one member parent `p`, alive throughout, and one member child `m` at
/// a time (`aterm_vitals::explain::reap`). Its `Tick` reads every process at one instant, so the
/// sweep's order (a zombie read after its parent) is outside it, as are members gone together,
/// a parent that dies or waits as a zombie (what it leaves is uncertain), and a parent the kernel
/// reaps for (`P_NOCLDWAIT`, which adds `m`'s time to `pk` twice, its growth then counting
/// nothing and its step marked): the walk is already 63,082 states of the interpreter's 100,000,
/// and that parent's flag would double them, so the ledger's and the sweep's own tests hold
/// those.
///
/// `pc` and `pk` are `p`'s own CPU and its reaped children's (the kernel adds a child's own and
/// reaped time at its reap); `mc` and `mk` are `m`'s; `ms` is `m` running (1), a zombie (2) or
/// gone (0); `mo` is `m` under `p` (0) or under launchd (1), where a wrapper between them that
/// died leaves it; `gen` is `m`'s generation (a new process, its pid perhaps reused); `w` is the
/// tab's true work. `Tick` is the ledger's step on one whole sweep: `p`'s row (`rpc`, `rpk`, and
/// `pend`, the work counted that its reaped time has yet to show), `m`'s (`row`, `rgen`, `rmc`,
/// `rmk`, `rpar` its parent as last read, `zread` whether that reading was of a zombie, `gone`
/// whether it was reaped since), the counter `c` (`cp` before the step), the live-only figure
/// `live` (`livep` before: the own CPU of the members read at both readings), `pendp` (`pend`
/// before) and `fr` (the step found a member gone that its last reading put under launchd);
/// `lpc`, `lmc`, `lgen`, `lpres` and `lmo` are what the last reading read. Laws `TabCounterMonotone` and `RateNeverBelowLive` (Buggy 1:
/// the naive Δ(live + reaped), the counter its members' `cpu + child` now, which dips when
/// launchd reaps a member), `ZombieReadOnce` (Buggy 2, SA's `ForgetZombieRead`: a zombie keeps
/// its last live reading), `PendingDropsOnForeignReap` (Buggy 3: a member keeps the parent it
/// was first read under), `CountedOnce` (Buggy 4: nothing is absorbed, so a member its parent
/// reaped counts twice).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn reap_credit_model() -> Model {
    aterm_spec::ty_model! {
        ReapCredit {
            const Buggy = 0;
            const Work = 3;
            var n = 0;
            var w = 0;
            var pc = 0;
            var pk = 0;
            var mc = 0;
            var mk = 0;
            var ms = 1;
            var mo = 0;
            var gen = 0;
            var lpc = 0;
            var lmc = 0;
            var lgen = 0;
            var lpres = 1;
            var lmo = 0;
            var rpc = 0;
            var rpk = 0;
            var pend = 0;
            var row = 1;
            var rgen = 0;
            var rmc = 0;
            var rmk = 0;
            var rpar = 0;
            var zread = 0;
            var gone = 0;
            var c = 0;
            var cp = 0;
            var live = 0;
            var livep = 0;
            var pendp = 0;
            var fr = 0;

            action PWork when (n <= Work - 1) {
                n = n + 1;
                w = w + 1;
                pc = pc + 1;
            }
            action PChildWork when (n <= Work - 1) {
                n = n + 1;
                w = w + 1;
                pk = pk + 1;
            }
            action MWork when (n <= Work - 1 && ms == 1) {
                n = n + 1;
                w = w + 1;
                mc = mc + 1;
            }
            action MChildWork when (n <= Work - 1 && ms == 1) {
                n = n + 1;
                w = w + 1;
                mk = mk + 1;
            }
            action Reparent when (ms > 0 && mo == 0) {
                mo = 1;
            }
            action Exit when (ms == 1) {
                ms = 2;
            }
            action PReap when (ms == 2 && mo == 0) {
                ms = 0;
                pk = pk + mc + mk;
                gone = if (row == 1 && rgen == gen) { 1 } else { gone };
            }
            action ForeignReap when (ms == 2 && mo == 1) {
                ms = 0;
                gone = if (row == 1 && rgen == gen) { 1 } else { gone };
            }
            action Respawn when (ms == 0 && gen == 0) {
                gen = 1;
                ms = 1;
                mo = 0;
                mc = 0;
                mk = 0;
            }
            action Tick {
                c = if (Buggy == 1) {
                    pc + pk + (if (ms > 0) { mc + mk } else { 0 })
                } else {
                    c + pc - rpc + pk - rpk
                        - (if (Buggy == 4) {
                            0
                        } else {
                            if (pend + (if (gone == 1 && rpar == 0) { rmc + rmk } else { 0 })
                                <= pk - rpk)
                            {
                                pend + (if (gone == 1 && rpar == 0) { rmc + rmk } else { 0 })
                            } else {
                                pk - rpk
                            }
                        })
                        + (if (ms > 0) {
                            if (row == 1 && rgen == gen) {
                                if (Buggy == 2 && ms == 2) { 0 } else { mc + mk - rmc - rmk }
                            } else {
                                mc + mk
                            }
                        } else {
                            0
                        })
                };
                cp = c;
                live = live
                    + (pc - lpc)
                    + (if (lpres == 1 && lgen == gen && ms > 0) { mc - lmc } else { 0 });
                livep = live;
                pend = if (Buggy == 4) {
                    pend + (if (gone == 1 && rpar == 0) { rmc + rmk } else { 0 })
                } else {
                    if (pend + (if (gone == 1 && rpar == 0) { rmc + rmk } else { 0 }) <= pk - rpk) {
                        0
                    } else {
                        pend + (if (gone == 1 && rpar == 0) { rmc + rmk } else { 0 }) - (pk - rpk)
                    }
                };
                pendp = pend;
                fr = if (gone == 1 && lmo == 1) { 1 } else { 0 };
                gone = 0;
                rpc = pc;
                rpk = pk;
                row = if (ms > 0) { 1 } else { 0 };
                rgen = gen;
                rmc = if (ms > 0) {
                    if (Buggy == 2 && row == 1 && rgen == gen && ms == 2) { rmc } else { mc }
                } else {
                    0
                };
                rmk = if (ms > 0) {
                    if (Buggy == 2 && row == 1 && rgen == gen && ms == 2) { rmk } else { mk }
                } else {
                    0
                };
                rpar = if (ms > 0) {
                    if (Buggy == 3 && row == 1 && rgen == gen) { rpar } else { mo }
                } else {
                    0
                };
                zread = if (ms == 2) { 1 } else { 0 };
                lpc = pc;
                lmc = mc;
                lgen = gen;
                lpres = if (ms > 0) { 1 } else { 0 };
                lmo = mo;
            }

            invariant TabCounterMonotone: cp <= c;
            invariant RateNeverBelowLive: live - livep <= c - cp;
            invariant PendingDropsOnForeignReap: fr == 0 || pend <= pendp;
            invariant ZombieReadOnce:
                zread == 0 || rgen > gen || gen > rgen || rmc + rmk == mc + mk;
            invariant CountedOnce: c <= w;
            bound Box: n <= Work;
        }
    }
}
