// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OCCUPANCY DOOR (`docs/DESIGN-harness-occupancy-2026-10-20.md` §7, as the red-team corrects it: F6's fairness, dials and bound, F7's hold clock, F11's upgrade input).

use aterm_spec::derive::{Liveness, Model};

/// THE DOOR.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn occupancy_door_model() -> Model {
    aterm_spec::ty_model! {
        OccupancyDoor {
            const Buggy = 0;
            const K = 2;
            const Mode = 1;
            scenario Mode = 0;
            scenario Mode = 2;
            var door = 0;
            var person = 0;
            var task = 1;
            var brake = 0;
            var untaken = 0;
            var cond = 0;
            var upg = 0;
            var paced = 0;
            var held = 0;
            var owed = 0;
            var due = 0;
            var intent = 0;
            var told = 0;
            var wrote = 0;
            var lines = 0;
            var age = 0;
            var esc = 0;
            var armed = 1;
            var sdoor = 1;
            var sperson = 0;
            var stask = 1;
            var smode = 1;
            var sbrake = 0;
            var suntaken = 0;
            var spaced = 0;
            var srow = 1;
            var scond = 1;
            var snotice = 1;
            var sopen = 1;
            var sclose = 0;
            var uwake = 0;
            var hbreak = 0;
            var hown = 0;
            var hanswer = 0;
            var restarted = 0;

            action Turn when (door > 0) {
                door = 0;
                lines = 0;
                owed = if (door == 2) { 0 } else { owed };
            }
            action Break when (door == 0) {
                door = 1;
                lines = 0;
                owed = 0;
            }
            action Point when (door == 0) {
                door = 2;
                lines = 0;
            }
            action HoldAges when (door == 1 && held == 0) {
                held = 1;
                cond = 1;
            }
            action IdleRead when (door == 1) { lines = 0; }
            action HoldCloses when (held == 1 && owed == 0 && (door == 0 || door == 2)) {
                held = 0;
                cond = 0;
                hbreak = if (door == 1) { 1 } else { 0 };
                hown = if (door == 0) { owed } else { 0 };
                hanswer = if (door == 2) { owed } else { 0 };
            }
            action PersonIn when (person == 0) { person = 1; }
            action PersonOut when (person == 1) { person = 0; }
            action TaskEnds when (task == 1) { task = 0; }
            action TaskBegins when (task == 0) { task = 1; }
            action BrakeOn when (brake == 0) { brake = 1; }
            action BrakeOff when (brake == 1) { brake = 0; }
            action ActOwed when (untaken == 0) { untaken = 1; }
            action ActTaken when (untaken == 1) { untaken = 0; }
            action Holds when (cond == 0) { cond = 1; }
            action Fails when (cond == 1) { cond = 0; }
            action UpgradeDue when (upg == 0) { upg = 1; }
            action PaceLapses when (paced == 1) { paced = 0; }
            action OpenNudge when (cond == 1 && held == 1 && due == 0 && told == 0) {
                sopen = held;
                due = 1;
                age = 0;
                armed = 1;
            }
            action OpenWake when (cond == 1 && due == 0 && told == 0) {
                due = 2;
                age = 0;
                armed = 1;
            }
            action Crash when (intent == 1) { intent = 0; }

            action Intend when (due > 0 && intent == 0 && cond == 1) { intent = 1; }
            action Type when (
                intent == 1 && cond == 1 && door > 0 && person == 0 && task == 1 && brake == 0
                    && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action Skip when (intent == 1 && (cond == 0 || door == 0)) { intent = 0; }
            action Lapse when (due > 0 && intent == 0 && cond == 0) {
                due = 0;
                age = 0;
                esc = 0;
            }
            action Close when (due == 0 && told > 0 && cond == 0) {
                sclose = cond;
                told = 0;
                wrote = 0;
            }
            action UpgradeTypes when (upg == 1 && door == 1 && person == 0 && lines == 0 && 2 > due) {
                uwake = if (due == 2) { 1 } else { 0 };
                upg = 0;
                lines = lines + 1;
                owed = held;
            }
            action Sample when (due > 0 && esc == 0 && K - 1 > age) { age = age + 1; }
            action SampleLast when (due > 0 && esc == 0 && age == K - 1 && armed == 1) {
                age = K;
                esc = 1;
            }

            action TypeAtAnyDoor when (
                Buggy == 2 && intent == 1 && cond == 1 && person == 0 && task == 1
                    && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeUnderPerson when (
                Buggy == 3 && intent == 1 && cond == 1 && door > 0 && task == 1
                    && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeWithoutAJob when (
                Buggy == 4 && intent == 1 && cond == 1 && door > 0 && person == 0
                    && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeUnderOff when (
                Buggy == 5 && intent == 1 && cond == 1 && door > 0 && person == 0 && task == 1
                    && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeThroughBrake when (
                Buggy == 6 && intent == 1 && cond == 1 && door > 0 && person == 0 && task == 1
                    && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeOverUntaken when (
                Buggy == 7 && intent == 1 && cond == 1 && door > 0 && person == 0 && task == 1
                    && brake == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeUnpaced when (
                Buggy == 8 && intent == 1 && cond == 1 && door > 0 && person == 0 && task == 1
                    && brake == 0 && untaken == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeWithoutRow when (
                Buggy == 9 && due > 0 && cond == 1 && door > 0 && person == 0 && task == 1
                    && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeStale when (
                Buggy == 10 && intent == 1 && door > 0 && person == 0 && task == 1
                    && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action StackSecond when (
                Buggy == 11 && intent == 1 && cond == 1 && door > 0 && person == 0 && task == 1
                    && brake == 0 && untaken == 0 && paced == 0 && lines <= 1
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action TypeKeepsDue when (
                Buggy == 12 && wrote <= 1 && intent == 1 && cond == 1 && door > 0 && person == 0
                    && task == 1 && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2)) && (due == 2 || upg == 0)
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action SeedIntentAsTold when (Buggy == 13 && intent == 1) {
                intent = 0;
                due = 0;
                told = told + 1;
            }
            action UpgradeFirst when (Buggy == 14 && upg == 1 && door == 1 && person == 0 && lines == 0) {
                uwake = if (due == 2) { 1 } else { 0 };
                upg = 0;
                lines = lines + 1;
                owed = held;
            }
            action LateRaise when (Buggy == 15 && due > 0 && esc == 0 && age == K - 1) { age = K; }
            action RestartOnIdle when (Buggy == 16 && door == 1 && held == 1) {
                lines = 0;
                held = 0;
                cond = 0;
                hbreak = if (door == 1) { 1 } else { 0 };
                hown = if (door == 0) { owed } else { 0 };
                hanswer = if (door == 2) { owed } else { 0 };
            }
            action CloseOnOurTurn when (Buggy == 17 && held == 1 && door == 0) {
                held = 0;
                cond = 0;
                hbreak = if (door == 1) { 1 } else { 0 };
                hown = if (door == 0) { owed } else { 0 };
                hanswer = if (door == 2) { owed } else { 0 };
            }
            action CloseAtTheAnswer when (Buggy == 18 && held == 1 && door == 2) {
                held = 0;
                cond = 0;
                hbreak = if (door == 1) { 1 } else { 0 };
                hown = if (door == 0) { owed } else { 0 };
                hanswer = if (door == 2) { owed } else { 0 };
            }
            action CloseEarly when (Buggy == 19 && due == 0 && told > 0) {
                sclose = cond;
                told = 0;
                wrote = 0;
            }
            action TypeOverNotice when (
                Buggy == 20 && intent == 1 && cond == 1 && door > 0 && person == 0 && task == 1
                    && brake == 0 && untaken == 0 && paced == 0 && lines == 0
                    && (Mode == 1 || (Mode == 2 && due == 2))
            ) {
                sdoor = if (door > 0) { 1 } else { 0 };
                sperson = person;
                stask = task;
                smode = if (Mode == 1 || (Mode == 2 && due == 2)) { 1 } else { 0 };
                sbrake = brake;
                suntaken = untaken;
                spaced = paced;
                srow = intent;
                scond = cond;
                snotice = if (due == 2 || upg == 0) { 1 } else { 0 };
                told = told + 1;
                wrote = wrote + 1;
                due = 0;
                intent = 0;
                lines = lines + 1;
                paced = 1;
                untaken = 1;
                owed = held;
                age = 0;
                esc = 0;
            }
            action NudgeUnheld when (Buggy == 21 && cond == 1 && due == 0 && told == 0) {
                sopen = held;
                due = 1;
                age = 0;
                armed = 1;
            }
            action RestartClock when (Buggy == 1 && due > 0 && esc == 0 && age > 0) {
                age = 0;
                restarted = 1;
            }
            action LoseDeadline when (Buggy == 1 && due > 0 && armed == 1) { armed = 0; }

            invariant NoLineIntoATurn: wrote == 0 || sdoor == 1;
            invariant NoLineOverAPerson: wrote == 0 || sperson == 0;
            invariant NoLineWithoutAJob: wrote == 0 || stask == 1;
            invariant LinesKeepTheirModes: wrote == 0 || smode == 1;
            invariant BrakesFirst: wrote == 0 || sbrake == 0;
            invariant OneActAtATime: wrote == 0 || suntaken == 0;
            invariant PaceHolds: wrote == 0 || spaced == 0;
            invariant IntentBeforeLine: wrote == 0 || srow == 1;
            invariant NeverToldStale: wrote == 0 || scond == 1;
            invariant OneLinePerDoor: lines <= 1;
            invariant OncePerEpisode: wrote <= 1;
            invariant TheBookMatchesTheScreen: told == wrote;
            invariant AWakeOutranksTheUpgrade: uwake == 0;
            invariant TheNoticeGoesFirst: wrote == 0 || snotice == 1;
            invariant TheNudgeWaitsForItsAge: sopen == 1;
            invariant AnEpisodeEndsWithItsCondition: sclose == 0;
            invariant RaisedByK: due == 0 || esc == 1 || K > age;
            invariant TheHoldOutlastsItsBreaks: hbreak == 0;
            invariant TheHarnessTurnKeepsTheHold: hown == 0;
            invariant TheAnswersPointKeepsTheHold: hanswer == 0;
            invariant TheClockNeverRestarts: restarted == 0;
            invariant TheDeadlineStaysArmed: due == 0 || esc == 1 || armed == 1;
            bound LooksBox: age <= K;
        }
    }
}

/// `TheDueIsToldOrRaised`, `[]<>(due == 0 || esc == 1)`: a due line is told, dropped, or raised as the K Entry.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(occupancy_door_model)]
pub fn occupancy_door_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "TheDueIsToldOrRaised",
        goal: or_(eq(var("due"), int(0)), eq(var("esc"), int(1))),
        weak: vec!["Sample", "SampleLast"],
        strong: vec![],
        mutants: vec!["RestartClock", "LoseDeadline"],
    }
}
