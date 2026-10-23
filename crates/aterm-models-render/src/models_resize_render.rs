// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal models for resize-ledger render verdicts and repaint nudges.

use aterm_spec::derive::{Liveness, Model};

/// Resize-ledger render verdict model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resize_render_model() -> Model {
    aterm_spec::ty_model! {
        ResizeRender {
            const Buggy = 0;
            const Cap = 2;
            var geom = 0;
            var app_geom = 0;
            var disp = 0;
            var ro = 0;
            var rs = 0;
            var rd = 0;
            var rw = 0;
            var late = 0;
            var led = 0;
            var lr = 0;
            var lb = 0;
            var br = 0;
            var blind = 0;
            var drew = 0;
            var evaluated = 0;
            var verdict = 0;
            var shown = 0;
            var ls = 0;
            var st = 0;
            var d0 = 0;
            var bot = 0;
            var out = 0;
            var debt = 0;
            var eg = 0;
            var judged = 0;
            var ev = 0;

            action Demote when (geom == 0 && (bot == 0 || bot == 2) && disp <= Cap - 1) {
                rs = if (ro == 1) { rs } else { geom };
                rd = if (ro == 1) { rd + 1 } else { 1 };
                rw = if (ro == 1) { rw } else { 0 };
                d0 = if (ro == 1) { d0 } else { disp };
                out = if (ro == 1) { out } else { 0 };
                ro = 1;
                geom = 1;
                disp = disp + 1;
                st = st + 1;
                late = 0;
                eg = if (debt <= 0) { geom } else { eg };
                debt = if (debt <= Cap - 1) { debt + 1 } else { debt };
                evaluated = 0;
            }
            action Trim when (geom == 0 && bot <= 1) {
                rs = if (ro == 1) { rs } else { geom };
                rd = if (ro == 1) { rd } else { 0 };
                rw = if (ro == 1) { rw } else { 0 };
                d0 = if (ro == 1) { d0 } else { disp };
                out = if (ro == 1) { out } else { 0 };
                ro = 1;
                geom = 1;
                late = 0;
                eg = if (debt <= 0) { 0 } else { eg };
                evaluated = 0;
            }
            action Grow when (geom == 1) {
                rs = if (ro == 1) { rs } else { geom };
                rd = if (ro == 1) {
                    if (Buggy == 1) { rd } else { rd - st }
                } else {
                    if (Buggy == 1) { 0 } else { 0 - st }
                };
                rw = if (ro == 1) { rw } else { 0 };
                d0 = if (ro == 1) { d0 } else { disp };
                out = if (ro == 1) { out } else { 0 };
                ro = 1;
                geom = 0;
                disp = if (Buggy == 1) { disp } else { disp - st };
                bot = if (Buggy == 1) { 1 } else { if (1 <= st) { 2 } else { 1 } };
                blind = if (Buggy == 1) {
                    if (disp <= 0) { 0 } else { blind }
                } else {
                    if (disp - st <= 0) { 0 } else { blind }
                };
                eg = if (Buggy == 1) {
                    if (debt <= 0) { geom } else { eg }
                } else {
                    if (debt - st <= 0) { 0 } else { if (debt <= 0) { geom } else { eg } }
                };
                debt = if (Buggy == 1) { debt } else { debt - st };
                st = 0;
                late = 0;
                evaluated = 0;
            }
            action Step {
                app_geom = geom;
                disp = if (geom == app_geom) { disp } else { 0 };
                led = if (geom == app_geom) {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { led } else {
                        if (ro == 1 && geom == rs && 1 <= rd) { led + rd } else { led }
                    }
                } else {
                    0
                };
                lr = if (geom == app_geom) {
                    if (1 <= led) { 1 } else {
                        if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lr } else {
                            if (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1)) { 1 } else { lr }
                        }
                    }
                } else {
                    0
                };
                lb = if (geom == app_geom) {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lb } else {
                        if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { lb }
                    }
                } else {
                    0
                };
                br = if (geom == app_geom) {
                    if (lb == 1) { 1 } else {
                        if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { br } else {
                            if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { br }
                        }
                    }
                } else {
                    0
                };
                blind = if (geom == app_geom) { blind } else { 0 };
                drew = if (geom == app_geom) {
                    if (1 <= disp) { 1 } else { drew }
                } else {
                    0
                };
                rw = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { 1 } else { 0 };
                out = if (geom == app_geom && ro == 1) { 1 } else { out };
                ro = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { ro } else { 0 };
                rs = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { rs } else { 0 };
                rd = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { rd } else { 0 };
                late = 0;
                ls = 0;
                debt = if (geom == app_geom) { debt } else { if (Buggy == 1) { debt } else { 0 } };
                eg = if (geom == app_geom) { eg } else { 0 };
                st = 0;
                bot = 0;
                evaluated = 0;
            }
            action Draw {
                led = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { led } else {
                    if (ro == 1 && geom == rs && 1 <= rd) { led + rd } else { led }
                };
                lr = if (1 <= led) { 1 } else {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lr } else {
                        if (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1)) { 1 } else { lr }
                    }
                };
                lb = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lb } else {
                    if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { lb }
                };
                br = if (lb == 1) { 1 } else {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { br } else {
                        if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { br }
                    }
                };
                drew = if (1 <= disp) { 1 } else { drew };
                rw = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { 1 } else { 0 };
                out = if (ro == 1) { 1 } else { out };
                ro = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { ro } else { 0 };
                rs = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { rs } else { 0 };
                rd = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { rd } else { 0 };
                late = 0;
                ls = 0;
                st = 0;
                bot = 0;
                evaluated = 0;
            }
            action Output {
                st = 0;
                out = if (ro == 1) { 1 } else { out };
                evaluated = 0;
            }
            action Evaluate {
                evaluated = 1;
                ev = if (Buggy == 1) {
                    if (geom + app_geom == 1) { 1 } else { 0 }
                } else {
                    if (Buggy == 7) {
                        0
                    } else {
                        if (geom == eg && (1 <= debt || Buggy == 8)) {
                            if (judged == 1 || Buggy == 6) { 1 } else { 2 }
                        } else {
                            0
                        }
                    }
                };
                verdict = if (Buggy == 1) {
                    if (geom == app_geom) { 0 } else { 1 }
                } else {
                    if (lr == 1 || (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1) && rw == 1)) {
                        if (Buggy == 4) { 2 } else { 1 }
                    } else {
                        if (Buggy == 2) { 0 } else {
                            if (br == 1 || (ro == 1 && geom + rs == 1 && 1 <= rd && rw == 1)) { 2 } else { 0 }
                        }
                    }
                };
                shown = if (Buggy == 1) {
                    if (geom == app_geom) { 0 } else { 1 }
                } else {
                    if (ls == 1 || (ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0)) { 1 } else { 0 }
                };
                ls = if (Buggy == 3 && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { 1 } else { ls };
                lr = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    lr
                } else {
                    if (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1) && rw == 1) { 1 } else { lr }
                };
                led = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    led
                } else {
                    if (ro == 1 && geom == rs && 1 <= rd) { led + rd } else { led }
                };
                lb = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    lb
                } else {
                    if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { lb }
                };
                br = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    br
                } else {
                    if (ro == 1 && geom + rs == 1 && 1 <= rd && rw == 1) { 1 } else { br }
                };
                blind = if (1 <= disp && geom == 1) { 1 } else { blind };
                ro = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { 1 } else { 0 };
                rs = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { rs } else { 0 };
                rd = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { rd } else { 0 };
                late = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { 1 } else { 0 };
                rw = 0;
            }

            action Classify when (judged == 0) {
                judged = 1;
                evaluated = 0;
            }

            invariant RiskIsFlagged: evaluated == 0 || disp <= 0 || drew == 0 || blind == 1 || verdict == 1;
            invariant NoSilentPass: evaluated == 0 || disp <= 0 || drew == 0 || 1 <= verdict;
            invariant NoFalseVerdict: evaluated == 0 || verdict == 0 || (1 <= disp && drew == 1);
            invariant DisplacedIsShown: evaluated == 0 || disp <= 0 || drew == 1 || shown == 1;
            invariant NoFalseDisplaced: evaluated == 0 || shown == 0 || 1 <= disp;
            invariant QuietFlapIsIdentity: ro == 0 || rs == 1 || geom == 1 || out == 1 || disp == d0;
            invariant EpochFlagsTheResidual: evaluated == 0 || disp <= 0 || geom + eg == 1 || 1 <= ev;
            invariant EpochNoFalseAlarm: evaluated == 0 || ev == 0 || 1 <= disp;
            invariant OnlyAJudgedProgramIsAccused: evaluated == 0 || judged == 1 || ev == 0 || 2 <= ev;
            invariant DebtIsTheDisplacement: debt == disp;
            bound Bounded: disp <= Cap && led <= Cap && rd <= Cap && st <= Cap && debt <= Cap;
        }
    }
}

/// Host closed-loop repaint nudge model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn repaint_nudge_model() -> Model {
    aterm_spec::ty_model! {
        RepaintNudge {
            const Buggy = 0;
            const Lim = 2;
            const MaxTries = 1;
            const Cap = 2;
            var phase = 0;
            var held = 0;
            var engine = 0;
            var debt = 0;
            var safe = 0;
            var ack1 = 0;
            var ack2 = 0;
            var tries = 0;
            var tick = 0;
            var healed = 0;
            var gaveup = 0;
            var halt = 0;
            var age = 0;
            var orphan = 0;
            var moved = 0;
            var superseded = 0;
            var lost = 0;

            action Debt when (phase == 0 && debt == 0) { debt = 1; }
            action Quiet when (safe == 0) { safe = 1; }
            action Busy when (safe == 1) { safe = 0; }
            action App1 when (phase == 2 && ack1 == 0) { ack1 = 1; }
            action App2 when (phase == 3 && ack2 == 0) { ack2 = 1; }
            action AltLeft when (debt == 1) { debt = 0; }
            action Halt when (halt == 0) { halt = 1; }
            action Lift when (halt == 1) { halt = 0; }
            action Flap when (phase == 2 && held == 1) { held = 0; }
            action Resize when (2 <= phase && phase <= 3 && moved == 0) { held = 0; moved = 1; }
            action Return when (superseded == 1 && moved == 1) { moved = 0; }

            action Arm when (phase == 0 && debt == 1) { phase = 1; tick = 0; }
            action Hold when (phase == 1 && halt == 0 && (safe == 1 || tick > Lim - 1)) {
                phase = if (debt == 1) { 2 } else { 4 };
                held = if (debt == 1) { 1 } else { 0 };
                tries = if (debt == 1) { if (tries > MaxTries) { tries } else { tries + 1 } } else { tries };
                tick = 0;
                age = 0;
                ack1 = 0;
                ack2 = 0;
            }
            action Release when (phase == 2 && (ack1 == 1 || tick > Lim - 1 || debt == 0 || halt == 1)) {
                phase = 3;
                held = 0;
                age = 0;
                tick = 0;
            }
            action Forget when (phase == 2 && held == 0) {
                phase = 3;
                age = 0;
                tick = 0;
            }
            action Finish when (phase == 3 && ack1 == 1 && ack2 == 1) {
                phase = 4;
                healed = 1;
                debt = 0;
            }
            action GiveUp when (phase == 3 && tick > Lim - 1 && (ack1 == 0 || ack2 == 0)
                                && (debt == 0 || MaxTries <= tries) && moved == 0) {
                phase = 4;
                gaveup = if (debt == 1) { 1 } else { 0 };
            }
            action Supersede when (phase == 3 && tick > Lim - 1 && (ack1 == 0 || ack2 == 0)
                                   && moved == 1) {
                phase = 4;
                superseded = 1;
            }
            action Report when (superseded == 1 && moved == 0) {
                superseded = 0;
                gaveup = if (debt == 1) { 1 } else { gaveup };
            }
            action Tick when (1 <= phase && phase <= 3 && tick <= Lim - 1) { tick = tick + 1; }

            action DriverVanishes when (phase == 2) { phase = 4; orphan = 1; lost = 1; }
            action Age when (held == 1 && age <= Cap - 1) { age = age + 1; }
            action ReleaseByDeadline when (held == 1 && age > Cap - 1) {
                held = 0;
                age = 0;
                orphan = 0;
                phase = if (phase == 2) { 3 } else { phase };
            }

            action LoseTheHold when (Buggy == 1 && phase == 2) { phase = 4; }
            action HealOnFirstAnswer when (Buggy == 1 && phase == 2 && ack1 == 1) {
                phase = 4;
                held = 0;
                healed = 1;
                debt = 0;
            }
            action HoldTheEngine when (Buggy == 1 && phase == 1 && debt == 1) {
                phase = 2;
                held = 1;
                engine = 1;
                tries = if (tries > MaxTries) { tries } else { tries + 1 };
                tick = 0;
                ack1 = 0;
                ack2 = 0;
            }
            action NoCap when (Buggy == 1 && held == 1 && age == Cap) { age = Cap + 1; }
            action RetryForever when (Buggy == 1 && phase == 3 && tick > Lim - 1
                                      && (ack1 == 0 || ack2 == 0) && debt == 1) {
                phase = 1;
                tick = 0;
            }
            action DropOnFlap when (Buggy == 1 && phase == 2 && held == 0 && moved == 0) {
                phase = 4;
            }
            action ReturnUnwatched when (Buggy == 1 && superseded == 1 && moved == 1) {
                moved = 0;
                superseded = 0;
            }

            invariant HeldOnlyWhileShort: held == 0 || phase == 2 || orphan == 1;
            invariant AHeldPtyIsReleasedByTheDeadline: held == 0 || age <= Cap;
            invariant HealedNeedsBothAnswers: healed == 0 || (ack1 == 1 && ack2 == 1);
            invariant OneTryPerEpoch: tries <= MaxTries;
            invariant NudgeNeverTouchesTheEngine: engine == 0;
            invariant AnUnrepairedEndIsReported: phase <= 3 || debt == 0 || healed == 1
                || gaveup == 1 || superseded == 1 || lost == 1;
        }
    }
}

/// Liveness property for [`repaint_nudge_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(repaint_nudge_model)]
pub fn repaint_nudge_liveness() -> Liveness {
    use aterm_spec::derive::{and_, eq, int, or_, var};
    Liveness {
        name: "TheNudgeEnds",
        goal: or_(
            and_(
                eq(var("phase"), int(4)),
                or_(eq(var("superseded"), int(0)), eq(var("moved"), int(1))),
            ),
            or_(
                and_(eq(var("phase"), int(0)), eq(var("debt"), int(0))),
                eq(var("halt"), int(1)),
            ),
        ),
        weak: vec![
            "Arm",
            "Hold",
            "Release",
            "Finish",
            "GiveUp",
            "Supersede",
            "Report",
            "Tick",
        ],
        strong: vec![],
        mutants: vec!["RetryForever"],
    }
}
