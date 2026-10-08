// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! "NO UPGRADE STUCK FOREVER", as a bounded model (design record 2026-09-28,
//! `docs/DESIGN-upgrade-never-stuck-2026-09-28.md` §4, adapted to what main
//! built of it — steps 1–7, `f5626ba78` and `136ff0735` — and to the owner's
//! alert rules of 2026-09-28/29). Tab #1 sat on Claude Code 2.1.280 for three
//! days, and the owner was told nothing that said why: every bound the
//! upgrade had lived inside a step, and a step ran only at a point the
//! session offered. The design put a timer on every record (the watch,
//! `upgrade_drive::watch`), a class on every wait (`WaitClass`), and a tell
//! on every class. This states what the FIRST TWO buy as a property of whole
//! behaviours: THE UPGRADE MOVES, OR ITS RECORD NAMES WHAT HOLDS IT — now,
//! not what held it once.
//!
//! WHAT THIS MODEL NO LONGER SAYS (merged 2026-09-29, one implementation of
//! the tell): the third half — *how* the owner is told — was a tab MARK
//! raised at six hours (`Row::badge`) and modelled here as `Tell`/`Untell`.
//! The owner's rule of 2026-09-28 retired it: an alert persists only where
//! the owner can act, and a timer is not an inability. The tell is now
//! `upgrade_status::Row::attention` (quiet record, log fault, one digest
//! line, or an interrupt only for an agent the upgrade ended), whose
//! horizons rest on *progress* (a day without an act) and on classes the
//! model does not clock. Its law — no wait is quiet without an end — is
//! bound where the classifier runs, over the states this machine reaches:
//! aterm-agent's `upgrade_progress_tests.rs` (`the_real_view_...`). What the
//! record says (`cause`) is this model's; what the owner is then told is
//! that test's. The tell's variables (`age`, `behind`, `stood`, `told`,
//! `hold_rec`, `held`, `turned`, `hide`, `forever`, `resets`, `walled_mark`,
//! `unmarked`), actions (`Tick`, `HoldTurns`, `HoldStands`, `Tell`, `Untell`)
//! and mutants (`AsksOnItsOwnHides`, `RoundResetsAge`, `LimitRecordForEver`,
//! `MarkAtWall`, `UnmarkAtRearm`) went with it.
//!
//! A SIBLING of `HarnessUpgradeLadder` (`models_harness.rs`), whose law is
//! "the move lands, or a floor stands": this one is about the RECORD, not the
//! move — no point steps are assumed to come, and a floor that stands for ever
//! is a thing the record must say. As everywhere in this crate the model is
//! hand-written Rust DESCRIBING the code; the Tier-1 binds in aterm-agent's
//! `harness::upgrade_drive` tests (`upgrade_progress_tests.rs`, and the
//! watch's in `upgrade_watch_tests.rs`) are what make it a statement about
//! the program that compiled.

use super::*;

/// THE UPGRADE'S PROGRESS, AS ITS RECORD SAYS IT — one conversation's record
/// (`upgrade_drive::St`), its reducer's steps at the points the loop offers,
/// and the host's watch off any point.
///
/// THE WORLD (all unfair; nothing about it is assumed):
///
/// * `posture` — what the session shows: 0 a turn runs (no point: the loop's
///   `NotIdle`/`NotBusy`), 1 a settled BREAK of the agent's own background work
///   (a point: `host_steps_in_background` → `WorkerIdle::at_background`), 2 an
///   IDLE point (`host_steps_here` → `WorkerIdle::at_idle`), 3 a usage limit's
///   WALL (`upgrade::limited`; the loop's `Wall`/`LimitEpisode` withhold its
///   points, and every step waits `limited` — a notice left standing a
///   `LIMIT_GRACE_S` past its known reset is no wall since the owner's ruling
///   of 2026-09-29, C9: the loop offers it and the quiet step goes there, the
///   world's `IdlePoint`), 4 a PERSON'S HOLD — a box or a draft standing (the
///   loop's `Prompt`). The design's four postures, and a fifth main's guards
///   need (the assessment of 2026-09-29).
/// * `reset` — the wall's NAMED reset has passed (the transcript's
///   `resetsAt`, `upgrade::transcript_limit_until`; with the design's
///   `LIMIT_GRACE`), kept until the next wall.
/// * `work` — processes run under the agent (`Facts::background`,
///   `St::held_by`): the restart's gate (`upgrade::gate_restart`).
/// * `ready` — the agent's READY to the round's notice.
/// * The round's clock `Window` (`REASK_S` since the notice, `RETRY_S` since
///   the stop, the step deadline `due_by` of a pending round). A clock a
///   limit holds (`upgrade::clock_held`, `St::hold_clock`) is a `Window`
///   that does not come.
///
/// THE RECORD, AND WHAT READS IT:
///
/// * `phase` — 0 Pending, 1 Announced AT ITS LAST ASK (`MAX_ASKS` is the
///   model's one ask, so a re-ask is the give-up), 2 Failed(`unanswered`),
///   resting to its re-arm, 3 THE MOVE (Exiting, Relaunched, Done).
///   `rearmed` — the round was re-armed after a give-up and has typed no
///   notice yet (`St::last_stop`).
/// * `cause` — the CLASS of the recorded wait (`WaitClass` of `St::wait`,
///   `Row::overdue_cause`): 0 the agent's own work, 1 a person, 2 a limit,
///   3 aterm's own — the word an act clears (`St::note_step`; the view's
///   `Unrecorded`), the watch's word for a read whose step nothing holds
///   (`upgrade::NEXT_POINT`, the view's `NextPoint`) and the design's
///   `unoffered:<guard>` (A3).
/// * `clock` — the round's clock has run (the reducer's give-up and re-arm).
///
/// THE LANE:
///
/// * `StepAtIdle` — `upgrade::requested_step` over `upgrade::next_step` at an
///   idle point: announce (Pending), give up (the round's clock run), re-arm
///   (rested). Unfair: the design's point — the record must say what holds
///   the move whether or not points come.
/// * `RestartAtIdle` — the same step's restart (`upgrade::gate_restart`): the
///   READY heard and nothing under the agent.
/// * `StepAtBreak` — `upgrade::break_step` over the same at a settled break:
///   everything but the restart. Unfair.
/// * `Watch` — the host's look at the record OFF ANY POINT
///   (`upgrade_drive::watch`, woken at `upgrade_drive::watch_at` by
///   `upgrade_status::next_change`), and the visit's own look at a point
///   (`St::note_step`). It records the class of what holds the move NOW — the
///   model's truth, `T` below: a read that waits, its wait's; a read that
///   would take the round's own step, what holds the point that step waits
///   for (`upgrade::held_point`, which the dry read says after its act), else
///   `upgrade::NEXT_POINT` (`upgrade_watch::look_word`) — holds an announced
///   round's clock at a wall whose reset is ahead (its second sanctioned
///   change, `St::hold_clock`), and types nothing. WEAKLY FAIR: aterm's own
///   timer.
/// * `WatchRearms` — the watch's third sanctioned change (`sanctioned` →
///   `upgrade_drive::rearm`): a rested round re-armed with no hands. Unfair.
///
/// THE TRUTH (`T`, the class of what holds the move now): a limit at a wall
/// (a resting round's rest there, which `failed` names); at a person's hold,
/// the person — but the round's own answer inside its window, and the rest of
/// a resting round; at a turn, and at a settled BREAK of its own background
/// work, its own work (the restart is never taken at a break,
/// `upgrade::gate_restart`, and the quiet upgrade takes nothing there; the
/// watch's dry read, which knows no break, reads it as the agent at work,
/// which it is). THE BREAK IS A RE-DECISION OF THE YARDSTICK, NOT A CHANGE OF
/// CODE (the review of 2026-09-29): until that day `T` at a break was aterm's
/// own class wherever the ask flow's step there would act (`StepAtBreak`: a
/// notice, a give-up, a re-arm withheld by the loop's guard), and the dry
/// read's `not-idle:busy` there was half the gap `RecordedWordStands`. Nothing
/// in the code moved for that half: the truth did.
/// `derived_harness_upgrade_progress_break_is_a_redecision` pins exactly where
/// the two yardsticks differ. At an idle point, ATERM'S OWN when the step
/// there would act and has not (nothing holds the move:
/// `upgrade::NEXT_POINT`), else its own work (`awaiting-ready`, `background`,
/// `failed`). The healthy `Watch` records `T`;
/// [`harness_upgrade_progress_truth`] is the same expression for the law,
/// pinned equal to this one in the Tier-0 test.
///
/// MAIN TODAY IS THIS MACHINE, ITS LATCH CLEAR (since 2026-09-29): the gap
/// the assessment of 2026-09-29 found in the record, `RecordedWordStands`
/// (`keep`) — a genuine path on which the record never names what holds the
/// move, breaking the law ALONE — is CLOSED FOR A CLAUDE CODE RECORD, and its
/// latch is a negative control (`aterm-spec`'s
/// `derived_harness_upgrade_progress_gaps_are_closed_on_main` proves the law
/// on main and replays the gap, and the Tier-1 binds drive main's code
/// against the committed machine):
///
/// * `RecordedWordStands` (`keep`) — CLOSED on 2026-09-29: main's watch
///   recorded a word only when its dry read WAITED (`wait:…`), so a read that
///   would act (`would-announce`, `gave-up`, `would-restart`, `would-rearm`)
///   left the last word — "held by its own work" after the work had ended —
///   and the look was stamped beside a cause that no longer held the move.
///   Not closed by a word of the loop's guard alone (the assessment's A3,
///   `unoffered:<guard>`): the give-up and the re-arm are taken at any
///   posture, and one neutral word for every act would name aterm's own class
///   at a running turn or a person's box (the Tier-1 reducer bind replays
///   that posture-blind word as a negative control: it hides a person's hold
///   the law names). The read now reports its own posture: its dry run says
///   what holds the point of a give-up or a re-arm after the act's word
///   (`upgrade::held_point`: `gave-up@box`, the re-armed round's own step),
///   and the watch records that — else `upgrade::NEXT_POINT`, where nothing
///   holds it (`upgrade_watch::look_word`) — but never over a word the act's
///   own last checks recorded past where the read stops (the notice's fence,
///   the restart's last look: `St::held_past_go`, outside this model). THREE
///   LIMITS, stated (the review of 2026-09-29): CLOSED FOR A CLAUDE CODE
///   RECORD ONLY — a Codex record's act read records nothing still
///   (`look_word`: the Codex lane's read says nothing of its point, and the
///   Codex lane is not in this model), so a Codex tab keeps the last word a
///   look found, the gap as it was; the SETTLED BREAK half is a re-decision
///   of `T` (THE TRUTH above), not a change of code; and the machine's steps
///   are the ask flow's, so the quiet flow's words are bound to their own
///   truth in aterm-agent's
///   `the_quiet_upgrades_read_records_what_holds_its_restart`, not to this
///   machine's `T`.
///
/// Its sibling, `LimitRecordForEver` — a limit a record at any age, past its
/// named reset too — was closed the same day: the record keeps the reset,
/// `St::limit_until` (the transcript's; a Codex wall's since 2026-09-30), and
/// the classifier faults it `LIMIT_GRACE_S` past it, `Row::limit_passed`; that
/// is a claim of the classifier's test, not of this machine.
///
/// ADAPTED FROM THE DESIGN (§4), for main's code: `NoStaleTell` becomes
/// `NoStaleLook` — main's view is stateless and reads the record, so it
/// cannot see whether the record is current, and what keeps the named cause
/// honest is the look recording what it read: the obligation is placed on the
/// watch. The design's `WatchTypes` and `RestartOverWork` keep their names.
///
/// NOT IN THE MODEL, stated: a late READY a gave-up round still hears (its
/// give-up forgets the answer here), the owner's word, the login wall, a
/// taskless restart, a retarget (the watch's first sanctioned change), the
/// Codex lane, the QUIET flow's steps (`upgrade_ask = false`, the default
/// since 2026-09-29: the restart alone, at a genuinely idle point — its
/// recorded words are bound to their own truth in aterm-agent's
/// `the_quiet_upgrades_read_records_what_holds_its_restart`), the act's own
/// checks past the reducer's go (the notice's fence, the restart's hand and
/// last look, whose words the watch keeps: `St::held_past_go`), a person's
/// hold standing past the window (the classifier's `will_ask_again`), the
/// move clock and every tell — the owner-facing class (`Row::attention`),
/// its horizons and the instants that wake the view (`next_change`) — and the
/// host itself staying alive: the weak fairness of `Watch`, ASSUMED and not
/// proved. Its failure is told at run time since 2026-09-29 by the view's
/// read-time `unwatched` (design record C10, the owner's ruling "with a grace
/// after wake": `upgrade_status::Row::unwatched` — a record no look has read
/// for two watch gaps, its watch a gap late, the Mac awake past its wake
/// grace), a tell of the classifier's outside this machine.
///
/// SAFETY, each invariant caught by its own `Buggy` member (each dead action
/// ALONE, `verify::audit_dead_negative_controls`):
/// `NoHandsOffAPoint` (`WatchTypes`: the watch announcing off a point — H1 of
/// `tools/grep_guard.sh`, and the watch's Tier-1 zero input requests),
/// `NoStaleLook` (`RecordedWordStands`), `NeverEndsRunningWork`
/// (`RestartOverWork`: the restart over processes under the agent,
/// `gate_restart`'s `background`). `NoLookWithoutAPoint` (looks only at the
/// points the session offers — main before `136ff0735`) is caught by the
/// liveness alone. Each mutant fires only from a state no mutant has
/// touched (`slipped`): one slip per behaviour, and the `Buggy = 1` space
/// inside the interpreter's budget.
///
/// LIVENESS: [`harness_upgrade_progress_liveness`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_progress_model() -> Model {
    crate::ty_model! {
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

            // THE WORLD: what the session shows, while the move is owed. A
            // person's hold that the posture leaves is gone, and the law's
            // ghosts of it with it.
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
            // The wall's end passes (and its grace), the notice still on the
            // screen: the reset the transcript named, or — none read — the
            // longest a usage limit lasts since the wait began.
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
            // The clocks.
            action Window when (phase <= 2 && clock == 0) {
                clock = 1;
            }
            // THE STEP AT AN IDLE POINT (`requested_step`): announce, give up
            // (the window run, no restart to take), re-arm (rested). An act
            // clears the recorded word. What it did is recorded as it did it:
            // a phase moved off a point (`offpoint`).
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
            // THE RESTART, the same step's (`requested_step` →
            // `upgrade::gate_restart`): the READY heard and nothing under the
            // agent. It records what ran under the agent as it restarted
            // (`at_work`), and where (`offpoint`).
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
            // THE STEP AT A SETTLED BREAK (`break_step`): never the restart.
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

            // THE WATCH'S LOOK, off any point, and a visit's at one: it records
            // the class of what holds the move now (`T`) — a read that waits,
            // its wait; a read that would act, what holds the point its step
            // waits for (`upgrade::held_point`), else nothing at all
            // (`upgrade::NEXT_POINT`, aterm's own class here). Under `keep` —
            // main until 2026-09-29 — it recorded only what its dry read WAITS
            // on: a read that would act left the word. Under `blind` — main
            // before `136ff0735` — nothing wakes it off a point: the instant
            // its timer names passes and nothing is read.
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
                // THE LIMIT HOLD (the watch's second sanctioned change,
                // `St::hold_clock`; a visit's `upgrade::clock_held`): at a wall
                // whose reset is ahead, an announced round's clock starts again
                // at the reset.
                clock = if posture == 3 && phase == 1 && reset == 0 && blind == 0 { 0 } else { clock };
                // WHAT THE LOOK SAW HOLD THE MOVE (`T`), whatever it recorded
                // or failed to read: `NoStaleLook` holds the recorded class to it.
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
            // THE WATCH'S RE-ARM (its third sanctioned change): a rested
            // round, with no hands. The same pass records the NEW round's word
            // after it (`upgrade::held_point` of the re-arm): this, then
            // `Watch`.
            action WatchRearms when (
                phase == 2 && clock == 1 && (blind == 0 || posture == 1 || posture == 2)
            ) {
                phase = 0;
                rearmed = 1;
                clock = 0;
            }

            // THE MUTANTS, each its own dead action, each deciding once
            // (`slipped`). Main before `136ff0735`: no look but at a point the
            // session offers (`next_change` had no `watch_at` arm).
            action NoLookWithoutAPoint when (
                Buggy == 1 && slipped == 0 && blind == 0 &&
                (posture == 0 || posture == 3 || posture == 4)
            ) {
                blind = 1;
                slipped = 1;
            }
            // Main until 2026-09-29 (the gap closed by `upgrade::held_point`
            // and `upgrade::NEXT_POINT`): the watch records only a read that
            // waits.
            action RecordedWordStands when (Buggy == 1 && slipped == 0 && keep == 0) {
                keep = 1;
                slipped = 1;
            }
            // The watch with hands: a notice typed off a point.
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
            // The restart over processes under the agent.
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

/// THE TRUTH the law is stated over (the class of what holds the move now,
/// `T` of [`harness_upgrade_progress_model`]): the same expression the
/// healthy `Watch` records, pinned equal to it over every reachable state by
/// the Tier-0 test (`derived_harness_upgrade_progress.rs`), so the yardstick
/// and the look cannot drift apart.
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

/// "THE UPGRADE MOVES, OR ITS RECORD NAMES WHAT HOLDS IT" (design record
/// 2026-09-28, §4.2, adapted to main and to the owner's alert rules):
/// `[]<>(moved \/ cause = T)` — again and again, the move is made
/// (`phase = 3`), or the record names what holds the move NOW (`T`). A
/// behaviour that leaves it for good is one where the record keeps a cause
/// that no longer holds, look after look: the tab #1 of 2026-09-24, whose
/// class was read once and never again.
///
/// THE ENVIRONMENT IS NOT FAIR, AND NEITHER ARE THE POINTS: turns, breaks,
/// idle points, walls, a person, work under the agent, its answer and time
/// itself are the world's, and the steps at a point may never come — the
/// point is that the record is kept true WHETHER OR NOT they do. What the
/// verdict ASSUMES, and nothing more, aterm's own code: `Watch`, weakly fair
/// — the host's timer wakes at every record's `watch_at` (`next_change`'s
/// `watch_at` arm, `View::watch_due`) and hands the tab to the watch,
/// whatever the session is doing — on the wall clock too, so an instant slept
/// through is past at the wake (aterm-gui's `bell_wait`), and where it does
/// not fire at all the view says so (`unwatched`, design record C10), outside
/// this law.
///
/// The mutants that break it, each alone: `NoLookWithoutAPoint` (main before
/// `136ff0735`: a wall after a posture change is never looked at again) and
/// `RecordedWordStands` (main until 2026-09-29: the watch leaves a stale word
/// where its read would act). Livelocks and stalls: nothing is ever stuck in a
/// way a deadlock check sees, and both are also a bad STATE once they bite
/// (`NoStaleLook`).
///
/// What the owner is then TOLD is the classifier's, bound over these same
/// states in aterm-agent (`upgrade_progress_tests.rs`): a quiet record has
/// an end, and a horizon past it that becomes a fault, a digest line or an
/// interrupt (`Row::attention`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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
