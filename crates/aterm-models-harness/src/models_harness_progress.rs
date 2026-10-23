// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! "No upgrade stuck forever" as a bounded model (`docs/DESIGN-upgrade-never-stuck-2026-09-28.md` §4).

use aterm_spec::derive::*;

/// The upgrade's progress as its record says it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_progress_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeProgress {
            const Buggy = 0;
            var posture = 0;
            var reset = 0;
            var work = 0;
            var ready = 0;
            var phase = 0;
            var rearmed = 0;
            var cause = 0;
            var clock = 0;
            var offpoint = 0;
            var seen = 0;
            var at_work = 0;
            var blind = 0;
            var keep = 0;
            var slipped = 0;

            action TurnRuns when (phase <= 2 && posture > 0) {
                posture = 0;
            }
            action BreakSettles when (phase <= 2 && (posture == 0 || posture > 1)) {
                posture = 1;
            }
            action IdlePoint when (phase <= 2 && (posture <= 1 || posture > 2)) {
                posture = 2;
            }
            action WallStands when (phase <= 2 && (posture <= 2 || posture == 4)) {
                posture = 3;
                reset = 0;
            }
            action PersonHolds when (phase <= 2 && posture <= 3) {
                posture = 4;
            }
            action ResetPasses when (phase <= 2 && posture == 3 && reset == 0) {
                reset = 1;
            }
            action WorkStarts when (phase <= 2 && work == 0) {
                work = 1;
            }
            action WorkEnds when (phase <= 2 && work == 1) {
                work = 0;
            }
            action Answers when (phase == 1 && ready == 0) {
                ready = 1;
            }
            action Window when (phase <= 2 && clock == 0) {
                clock = 1;
            }
            action StepAtIdle when (
                posture == 2 && (
                    phase == 0 ||
                    (phase == 1 && clock == 1 && (ready == 0 || work == 1)) ||
                    (phase == 2 && clock == 1)
                )
            ) {
                phase = if phase == 0 { 1 } else if phase == 1 { 2 } else { 0 };
                rearmed = if phase == 2 { 1 } else { 0 };
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
            }
            action RestartAtIdle when (
                posture == 2 && phase == 1 && ready == 1 && work == 0
            ) {
                phase = 3;
                rearmed = 0;
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                at_work = work;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
            }
            action StepAtBreak when (
                posture == 1 && (phase == 0 || ((phase == 1 || phase == 2) && clock == 1))
            ) {
                phase = if phase == 0 { 1 } else if phase == 1 { 2 } else { 0 };
                rearmed = if phase == 2 { 1 } else { 0 };
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
            }

            action Watch when (phase <= 2) {
                cause = if blind == 1 && (posture == 0 || posture > 2) { cause }
                    else if keep == 1 && (
                            if phase == 0 { posture == 2 }
                            else if phase == 1 {
                                (posture == 2 && ready == 1 && work == 0) ||
                                (clock == 1 && (posture <= 2 || posture == 4) &&
                                    (ready == 0 || work == 1))
                            }
                            else { clock == 1 }
                        ) { cause }
                    else { (
                            if posture == 3 { if phase == 2 { 0 } else { 2 } }
                            else if posture == 4 {
                                if phase == 0 { 1 } else if phase == 2 { 0 }
                                else if clock == 1 || (ready == 1 && work == 0) { 1 } else { 0 }
                            }
                            else if posture <= 1 { 0 }
                            else if phase == 0 || clock == 1 ||
                                (phase == 1 && ready == 1 && work == 0) { 3 }
                            else { 0 }
                        ) };
                clock = if posture == 3 && phase == 1 && reset == 0 && blind == 0 { 0 } else { clock };
                seen = if posture == 3 { if phase == 2 { 0 } else { 2 } }
                    else if posture == 4 {
                        if phase == 0 { 1 } else if phase == 2 { 0 }
                        else if clock == 1 || (ready == 1 && work == 0) { 1 } else { 0 }
                    }
                    else if posture <= 1 { 0 }
                    else if phase == 0 || clock == 1 ||
                        (phase == 1 && ready == 1 && work == 0) { 3 }
                    else { 0 };
            }
            action WatchRearms when (
                phase == 2 && clock == 1 && (blind == 0 || posture == 1 || posture == 2)
            ) {
                phase = 0;
                rearmed = 1;
                clock = 0;
            }

            action NoLookWithoutAPoint when (
                Buggy == 1 && slipped == 0 && blind == 0 &&
                (posture == 0 || posture == 3 || posture == 4)
            ) {
                blind = 1;
                slipped = 1;
            }
            action RecordedWordStands when (Buggy == 1 && slipped == 0 && keep == 0) {
                keep = 1;
                slipped = 1;
            }
            action WatchTypes when (
                Buggy == 1 && slipped == 0 && phase == 0 &&
                (posture == 0 || posture == 3 || posture == 4)
            ) {
                phase = 1;
                clock = 0;
                cause = 3;
                seen = 3;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
                slipped = 1;
            }
            action RestartOverWork when (
                Buggy == 1 && slipped == 0 && posture == 2 && phase == 1 &&
                ready == 1 && work == 1
            ) {
                phase = 3;
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                at_work = work;
                slipped = 1;
            }

            invariant NoHandsOffAPoint: offpoint == 0;
            invariant NoStaleLook: cause == seen;
            invariant NeverEndsRunningWork: at_work == 0;
        }
    }
}

/// `T` of [`harness_upgrade_progress_model`], the class of what holds the move now.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_progress_truth() -> Expr {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    if_(
        is("posture", 3),
        if_(is("phase", 2), int(0), int(2)),
        if_(
            is("posture", 4),
            if_(
                is("phase", 0),
                int(1),
                if_(
                    is("phase", 2),
                    int(0),
                    if_(
                        or_(is("clock", 1), and_(is("ready", 1), is("work", 0))),
                        int(1),
                        int(0),
                    ),
                ),
            ),
            if_(
                le(var("posture"), int(1)),
                int(0),
                if_(
                    or_(
                        or_(is("phase", 0), is("clock", 1)),
                        and_(is("phase", 1), and_(is("ready", 1), is("work", 0))),
                    ),
                    int(3),
                    int(0),
                ),
            ),
        ),
    )
}

/// The upgrade moves, or its record names what holds it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_upgrade_progress_model)]
pub fn harness_upgrade_progress_liveness() -> Liveness {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    Liveness {
        name: "TheUpgradeMovesOrTheRecordNamesWhatHoldsIt",
        goal: or_(
            is("phase", 3),
            eq(var("cause"), harness_upgrade_progress_truth()),
        ),
        weak: vec!["Watch"],
        strong: vec![],
        mutants: vec!["NoLookWithoutAPoint", "RecordedWordStands"],
    }
}
