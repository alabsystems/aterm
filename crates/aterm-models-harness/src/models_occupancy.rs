// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OCCUPANCY DOOR (`docs/DESIGN-harness-occupancy-2026-10-20.md` §7, as the red-team corrects
//! it: F6's fairness, dials and bound, F7's hold clock, F11's upgrade input).
//!
//! Where it departs from §7 as written, and why:
//!
//! * F6: only `Sample` and `SampleLast` are fair (the goal needs nothing else, and the liveness
//!   audit refuses an assumption the proof never used); the two liveness mutants share
//!   `Buggy = 1`, the baseline the audit adds each back to alone, and each safety mutant has a
//!   dial of its own; the bound is `age <= K` alone, since `TypeKeepsDue` and `StackSecond` break
//!   the counts §7 boxed.
//! * F7: the hold's clock is in the model (`held`, `HoldAges`, `IdleRead`, `HoldCloses`), with its
//!   own restart mutant (`RestartOnIdle`) and law (`TheHoldOutlastsItsBreaks`); a nudge opens on
//!   a hold of age only (`TheNudgeWaitsForItsAge`), and a hold coming of age is its condition
//!   holding.
//! * §3.1 and F5, the harness's own line: `owed` from a line typed in a hold of age until its
//!   answer ends, at the next break (whose tail the loop reads) or at the point after its turn.
//!   Neither that turn nor that point ends the hold (`TheHarnessTurnKeepsTheHold`,
//!   `TheAnswersPointKeepsTheHold`); a point that decides nothing while the work shows is `Point`
//!   with no `HoldCloses`.
//! * F11, upgrade-first: a nudge waits while the upgrade's notice is due (`upg`, what
//!   `IdleHost::upgrade_due` is to answer, asked only after the grant; `TheNoticeGoesFirst`); a
//!   wake does not.
//! * An episode closes only with its condition failed (`AnEpisodeEndsWithItsCondition`): closed
//!   while it holds, the same hold is nudged twice. The dwell itself is not modelled.
//! * The interpreter's budget (100,000 states a walk, and the sweep walks each dial whole; the
//!   largest, `Buggy = 1` under Cruise or Land, is 97,280): `K` is 2, enough for one look before
//!   the last; the autopilot is a constant, Cruise committed and Off and Land its scenarios, not a
//!   world variable — only `Type` and its copies read it, and Cruise lets every line go that Land
//!   or Off does, so a run that switches reaches, the mode aside, only what a Cruise run reaches;
//!   `held` is one bit (of the rule's age or not), and `owed` is set only with it (`owed = held`:
//!   the hold the laws keep is the one of age); and each record is the bit of the term it stands
//!   for (`smode` folds §7's `sdue` in, and `uwake` replaces `udue`).
//! * `StackSecond` and `TypeKeepsDue` are capped at a second line, so their spaces stay finite.
//!
//! Tier-0 is `tests/models_occupancy.rs`. The Tier-1 bind is
//! `crates/aterm-agent/tests/conformance_occupancy.rs`: it walks this model's states and drives the
//! real pure decider (`supervise::occupancy::rules::decide`), the hold's clock
//! (`HoldClock::step`), the books (`Books`) and the facts seam's seed (`FactsState::seed`) along
//! them. `Type`, `Skip` and `UpgradeTypes` (the typing path under `prompt`), and `Sample` and
//! `SampleLast` (the K Entry), are not implemented yet: nothing anchors them.

use aterm_spec::derive::{Liveness, Model};

/// THE DOOR. The world, unfair: `door` (0 a live turn, 1 a background break, 2 an idle point),
/// `person` (the keystroke grace, a driver's hand, `hold=1`, or a draft), `task` (0 a taskless
/// session, or a person's own exchange), `brake` (a stall or FROZEN, a host withhold, the memory
/// wall, reach Down, credits, a limit), `untaken` (an act stands untaken, a host turn awaiting its
/// answer among them), `cond` (the rule's condition as the read at the door finds it), `upg` (the
/// upgrade's notice is due), `paced` (the session's or the machine's pace runs) and `held` (THE
/// HOLD'S CLOCK: 1 once the open hold has stood the rule's age, 0 with none open or a younger
/// one), and `owed` (a line the harness typed in the hold of age is being answered). `Mode` is the
/// autopilot (0 Off, 1 Cruise, 2 Land). A hold comes of age at a break (`HoldAges`), an idle read
/// with no point begins a new break and leaves it standing (`IdleRead`), and it ends only away
/// from a break and from a harness line's answer, at a live turn the harness did not type or a
/// point that acted (`HoldCloses`), its rules' condition with it. The loop: `due` (0 none, 1 a
/// Cruise-only nudge, opened on a hold of age; 2 a Cruise-or-Land wake), `intent` (a
/// `fact-intent@v1` row with no outcome after it), `told` and `wrote` (this episode's tells as the
/// book and the screen hold them), `lines` (lines typed at this door), and the due line's looks
/// toward the K Entry (`age`, `esc`, and `armed`, its deadline).
///
/// Each write records what it saw, 1 where the term it stands for held (`sdoor`, `sperson`,
/// …), and each invariant is a relation over that record; `uwake` records an upgrade notice
/// typed while a wake was due, `sopen` a nudge opened on a hold of age, `sclose` an episode
/// closed with its condition holding, and `hbreak`, `hown` and `hanswer` a hold of age ended at a
/// break read, at a harness line's own turn, or at the point that ends its answer. Each safety
/// mutant (its own `Buggy = n`, 2 to 21) is a copy of a real action with one term dropped or
/// one assignment changed, dead at `Buggy = 0`, and breaks one invariant alone. The two
/// liveness mutants, `RestartClock` (a due line's looks restarted) and `LoseDeadline` (F1: a
/// due line with no deadline), share `Buggy = 1`, the baseline the liveness audit adds each back
/// to alone, and keep the ghost flag their invariant reads.
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
            // The world (unfair).
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
            // The loop.
            var due = 0;
            var intent = 0;
            var told = 0;
            var wrote = 0;
            var lines = 0;
            var age = 0;
            var esc = 0;
            var armed = 1;
            // What each write saw.
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
            // Liveness mutants only.
            var restarted = 0;

            // A harness line's answer ends at the break after its turn (`Break`), or at the
            // point after it (`Turn` leaving that point).
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
            // The hold's clock: of age at a break; an idle read with no point is a new break
            // and the hold stands; it ends away from a break only, and never at a harness
            // line's own turn or the point that ends it, its rules' condition with it.
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
            // Re-seeded from the rows: an intent with no outcome after it is untold.
            action Crash when (intent == 1) { intent = 0; }

            // The loop. Only `Sample` and `SampleLast` are fair (the liveness below).
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
            // Outcome `skipped`.
            action Skip when (intent == 1 && (cond == 0 || door == 0)) { intent = 0; }
            // Dropped, never told late.
            action Lapse when (due > 0 && intent == 0 && cond == 0) {
                due = 0;
                age = 0;
                esc = 0;
            }
            // Past the dwell: the next episode.
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
            // The Entry.
            action SampleLast when (due > 0 && esc == 0 && age == K - 1 && armed == 1) {
                age = K;
                esc = 1;
            }

            // Mutants: copies of `Type` with one term dropped (2 to 12), then the rest.
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
            // `intent == 1` read as `due > 0`: a line with no intent row before it.
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
            // `lines == 0` read as `lines <= 1`: a second line at the door.
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
            // `due = 0` left out of its body: the episode stays due after its line (its first
            // two tells only, so the space stays finite).
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
            // A copy of `Crash` that seeds the intent as told.
            action SeedIntentAsTold when (Buggy == 13 && intent == 1) {
                intent = 0;
                due = 0;
                told = told + 1;
            }
            // A copy of `UpgradeTypes` without `due < 2`.
            action UpgradeFirst when (Buggy == 14 && upg == 1 && door == 1 && person == 0 && lines == 0) {
                uwake = if (due == 2) { 1 } else { 0 };
                upg = 0;
                lines = lines + 1;
                owed = held;
            }
            // A copy of `SampleLast` without `esc = 1`.
            action LateRaise when (Buggy == 15 && due > 0 && esc == 0 && age == K - 1) { age = K; }
            // A copy of `IdleRead` that restarts the hold's clock: the hold of age ends at a
            // break read (10-19: a clock restarted at each short idle window never reaches 2 h).
            action RestartOnIdle when (Buggy == 16 && door == 1 && held == 1) {
                lines = 0;
                held = 0;
                cond = 0;
                hbreak = if (door == 1) { 1 } else { 0 };
                hown = if (door == 0) { owed } else { 0 };
                hanswer = if (door == 2) { owed } else { 0 };
            }
            // Copies of `HoldCloses` with `owed == 0` dropped from one arm: the hold ended at
            // the turn that answers a harness line (17), or at the point that ends it (18; F5).
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
            // A copy of `Close` without `cond == 0`: the episode closed while its condition
            // holds, so the same hold is nudged again.
            action CloseEarly when (Buggy == 19 && due == 0 && told > 0) {
                sclose = cond;
                told = 0;
                wrote = 0;
            }
            // A copy of `Type` without `(due == 2 || upg == 0)`: a nudge over the upgrade's
            // notice (F11).
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
            // A copy of `OpenNudge` without `held == 1`: a nudge before its hold's age.
            action NudgeUnheld when (Buggy == 21 && cond == 1 && due == 0 && told == 0) {
                sopen = held;
                due = 1;
                age = 0;
                armed = 1;
            }
            // The liveness mutants.
            action RestartClock when (Buggy == 1 && due > 0 && esc == 0 && age > 0) {
                age = 0;
                restarted = 1;
            }
            // F1: a due line with no deadline.
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

/// `TheDueIsToldOrRaised`, `[]<>(due == 0 || esc == 1)`: a due line is told, dropped, or
/// raised as the K Entry. Weak fairness of `Sample` (a 60 s look) and `SampleLast` (the look
/// that raises the Entry) alone (F6): the screen, the person, the mode and the condition are not
/// fair, and neither is the loop's own telling — an Entry meets the goal, and only the safety
/// invariants say how a line is typed. `RestartClock` and `LoseDeadline` each break it alone.
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
