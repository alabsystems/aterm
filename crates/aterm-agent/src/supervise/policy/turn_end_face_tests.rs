// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE TASK'S FACE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4.2):
// `TaskFace::of` over the policy's memory, the face the loop tells as the
// memory moves (`TurnEndState::face_moved`), someone else's live turn
// (`TurnEndState::took`), and the landing a loop before this one landed
// (`TurnEndState::seed_landed`, §9.4.5). Three faces: open, landed, theirs —
// FOREVER CRUISE has no done face, and FOREVER LAND no spent one. Included
// from `turn_end_tests.rs` (`mod face`), so its readings and acts are in
// scope. Each behaviour has its negative control.

use crate::harness::acts::TaskFace;

/// The landing's own reply: a clean defense ([`clean_defense`]).
const LANDING_REPLY: &str = "Committed and pushed the parser and its tests.
Nothing left: all of                              it is merged into the remote main branch.";
/// A reply that says no done.
const PROGRESS: &str = "Committing the parser now; the lexer is next.";

/// A state at the policy's memory `set` leaves on a fresh one.
fn state(set: impl FnOnce(&mut TurnEndState)) -> TurnEndState {
    let mut st = TurnEndState::default();
    set(&mut st);
    st
}

/// THE FACE TABLE, line by line of §9.4.2, each line's state set by hand:
/// someone else's turn read live (open), a person's own exchange (theirs),
/// landed (or a landing seeded for the first point), and open — a done claim
/// owed its act, a land episode however many nudges deep. NEGATIVE CONTROLS
/// in the table itself: a person's exchange is never landed, and a landed
/// task under someone else's live turn is never landed.
#[test]
fn the_face_is_a_function_of_the_policys_memory_in_the_designs_order() {
    let landed = |st: &mut TurnEndState| {
        st.done = 2;
        st.landed = true;
    };
    let theirs = |st: &mut TurnEndState| {
        st.done = 2;
        st.exchange = true;
    };
    let rows: Vec<(&str, TurnEndState, bool, TaskFace)> = vec![
        ("fresh", TurnEndState::default(), false, TaskFace::Open),
        ("a done claim", state(|s| s.done = 1), false, TaskFace::Open),
        (
            "a land episode five nudges deep",
            state(|s| {
                s.land.typed = true;
                s.land.nudges = 5;
            }),
            false,
            TaskFace::Open,
        ),
        ("landed", state(landed), false, TaskFace::Landed),
        (
            "a person's own exchange",
            state(theirs),
            false,
            TaskFace::Theirs,
        ),
        (
            "a landing seeded for the first point",
            state(|s| s.landed_seed = Some(true)),
            false,
            TaskFace::Landed,
        ),
        (
            "someone else's turn after landed",
            state(landed),
            true,
            TaskFace::Open,
        ),
        (
            "someone else's turn after theirs",
            state(theirs),
            true,
            TaskFace::Open,
        ),
    ];
    for (case, st, other, want) in &rows {
        assert_eq!(TaskFace::of(st, *other), *want, "{case}");
    }
    // NEGATIVE CONTROLS: what a reader of `task_done()` alone, or one that
    // never looks at someone else's turn, would say instead.
    let theirs = state(theirs);
    assert!(theirs.task_done() && TaskFace::of(&theirs, false) != TaskFace::Landed);
    let after = state(landed);
    assert_ne!(TaskFace::of(&after, true), TaskFace::of(&after, false));
}

/// THE FACE MOVES WITH THE POLICY'S OWN POINTS, told once each
/// ([`TurnEndState::face_moved`]): the land act's clean defense lands the
/// task (and a person's quick turn after it is theirs). A face read again is
/// no move. NEGATIVE CONTROLS: under Cruise no done claim moves the face —
/// the retired token, the check's second done report — and under Land
/// nudge after nudge answered with work leaves it open.
#[test]
fn the_face_moves_at_the_points_that_end_a_task_and_is_told_once() {
    // Land: instruction, clean defense — landed.
    let mut now = t0();
    let mut st = TurnEndState::default();
    assert_eq!(st.face_start(), TaskFace::Open);
    assert_eq!(
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now),
        land_instruction()
    );
    assert_eq!(st.face_moved(), None, "an act moves no face");
    now += 2 * MIN;
    at(&mut st, &land(idle(LANDING_REPLY, Some(2 * MIN))), now);
    assert_eq!(st.face_moved(), Some(TaskFace::Landed));
    assert_eq!(st.face_moved(), None, "told once");
    at(&mut st, &land(idle(LANDING_REPLY, None)), now + MIN);
    assert_eq!(st.face_moved(), None, "the same point read again");
    // …and a person's quick turn after it is theirs.
    now += 5 * MIN;
    let thanks = land(TurnEndReading {
        person: Some(Duration::from_secs(8)),
        ..idle("You're welcome!", Some(Duration::from_secs(5)))
    });
    at(&mut st, &thanks, now);
    assert_eq!(st.face_moved(), Some(TaskFace::Theirs));

    // NEGATIVE CONTROL, Cruise: the retired token moves nothing.
    let mut st = TurnEndState::default();
    st.face_start();
    act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t0());
    at(
        &mut st,
        &idle(
            "All in.
ATERM-TASK-DONE",
            Some(2 * MIN),
        ),
        t0() + 3 * MIN,
    );
    assert_eq!(st.face_moved(), None);
    assert_eq!(st.face(), TaskFace::Open);

    // NEGATIVE CONTROL, Cruise: the done check's second done report.
    let (mut st, now) = checked_at(t0());
    st.face_start();
    at(&mut st, &idle("All done.", Some(MIN)), now + MIN);
    assert_eq!(st.face_moved(), None);

    // NEGATIVE CONTROL, Land: nudge after nudge answered with work.
    let mut now = t0();
    let mut st = TurnEndState::default();
    st.face_start();
    act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now);
    for _ in 0..4 {
        now += 3 * MIN;
        assert_eq!(
            act(&mut st, &land(idle(PROGRESS, Some(3 * MIN))), now),
            land_nudged()
        );
        assert_eq!(st.face_moved(), None);
    }
    st.took();
    assert_eq!(st.face_moved(), None, "its own turn read live");
    now += 3 * MIN;
    at(&mut st, &land(idle(LANDING_REPLY, Some(3 * MIN))), now);
    assert_eq!(
        st.face_moved(),
        Some(TaskFace::Landed),
        "the fourth nudge's defense"
    );
}

/// SOMEONE ELSE'S LIVE TURN ([`TurnEndState::took`] with no act of the
/// policy's awaited): after landed or theirs the face is open again at its
/// first live read — told once, the same turn read again is no move. A live
/// read of a turn the policy's own act started — a continuation, the host's
/// line — moves nothing. The next point ends it. NEGATIVE CONTROL: the same
/// reads with the act awaited.
#[test]
fn someone_elses_live_turn_opens_the_face_and_the_policys_own_does_not() {
    let landed = || {
        let mut now = t0();
        let mut st = TurnEndState::default();
        st.face_start();
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now);
        now += 2 * MIN;
        at(&mut st, &land(idle(LANDING_REPLY, Some(2 * MIN))), now);
        assert_eq!(st.face_moved(), Some(TaskFace::Landed));
        (st, now)
    };
    let (mut st, now) = landed();
    st.took();
    assert!(st.other_turn());
    assert_eq!(st.face_moved(), Some(TaskFace::Open));
    st.took();
    assert_eq!(st.face_moved(), None, "the same turn read again");
    // Its end: a point. The person's job's report under Land is no landing.
    at(
        &mut st,
        &land(idle(PROGRESS, Some(12 * MIN))),
        now + 15 * MIN,
    );
    assert!(!st.other_turn(), "the point ends it");
    assert_eq!(st.face(), TaskFace::Open);

    // Theirs opens the same way.
    let mut st = TurnEndState::default();
    st.face_start();
    let greeting = TurnEndReading {
        person: Some(Duration::from_secs(8)),
        ..idle(
            "Hi! What would you like me to work on?",
            Some(Duration::from_secs(5)),
        )
    };
    at(&mut st, &greeting, t0());
    assert_eq!(st.face_moved(), Some(TaskFace::Theirs));
    st.took();
    assert_eq!(st.face_moved(), Some(TaskFace::Open));

    // NEGATIVE CONTROLS: the policy's own act awaited — a continuation after
    // landed (the task reopened by a person's report), the host's line.
    let (mut st, now) = landed();
    st.host_typed(now + MIN, HostTurn::Notice);
    st.took();
    assert!(!st.other_turn(), "the host's own line is the policy's");
    assert_eq!(st.face_moved(), None);
    let mut st = TurnEndState::default();
    st.face_start();
    act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t0());
    st.took();
    assert!(!st.other_turn(), "its own continuation");
    assert_eq!(st.face_moved(), None);
}

/// THE LANDING A LOOP BEFORE THIS ONE LANDED (§9.4.5): seeded, it holds at a
/// first point no live read came before whose reply is still a clean
/// defense ([`clean_defense`], as every landing's is) — the task landed, the
/// episode's instruction typed, the face landed, and NOTHING TYPED, now or
/// later. It is dropped by a live read before that point and by a reply
/// that is no clean defense — a done in words, the retired token an older
/// build's journal noted a `land@v1` landing at (the review of 2026-10-20) —
/// and the point is then decided as any first point is. A loop started as Off lifted keeps the
/// landed task landed, its episode over; under Cruise the seed is dropped and
/// the done claim owed Cruise's prompt (Cruise takes nothing as the end).
/// NEGATIVE CONTROL: with no seed the same first point gets the land
/// instruction after its back-off — the instruction typed into landed work
/// after every restart; the retired token ends nothing there either.
#[test]
fn a_seeded_landing_holds_only_at_a_first_point_still_a_clean_defense() {
    let first = |st: &mut TurnEndState, said: &str| {
        let now = t0();
        let start = st.face_start();
        assert_eq!(
            start == TaskFace::Landed,
            st.landed_seed_armed(),
            "the start tells a seeded landing: {said}"
        );
        let r = land(idle(said, None));
        let a = at(st, &r, now);
        let later = now + st.short_wait().unwrap_or_default();
        (a, decide_turn_end(st, &r, &cfg(), later))
    };
    let other = "All of it is in.\nNothing left: merged into the remote main branch.";
    for said in [LANDING_REPLY, other] {
        let mut st = TurnEndState::default();
        st.seed_landed(None);
        assert!(st.landed_seed_armed());
        let (a, later) = first(&mut st, said);
        assert_eq!((a, later), (TurnEndAction::Nothing, TurnEndAction::Nothing));
        assert!(st.task_done() && st.landed(), "{said}");
        assert_eq!(st.land_episode(), (true, 0), "{said}");
        assert!(!st.landed_seed_armed(), "taken up");
        assert_eq!(st.face(), TaskFace::Landed, "{said}");
        assert_eq!(
            st.face_moved(),
            None,
            "{said}: told landed at the start, the point restates it"
        );
        // NEGATIVE CONTROL: no seed — a done reply, a defense included,
        // gets the land instruction typed into landed work.
        let mut st = TurnEndState::default();
        let (_, later) = first(&mut st, said);
        assert_eq!(later, land_instruction(), "{said}");
        assert!(!st.landed(), "{said}");
        assert_eq!(st.face_moved(), None, "{said}");
    }
    let mut st = TurnEndState::default();
    let (_, later) = first(&mut st, "All in.\nATERM-TASK-DONE");
    assert_eq!(later, land_instruction(), "the retired token");
    // Dropped by a done that is no defense — in words, or the retired token
    // an older build's journal landed on: the instruction owed.
    for said in ["All done. Nothing left to do.", "All in.\nATERM-TASK-DONE"] {
        let mut st = TurnEndState::default();
        st.seed_landed(None);
        let (a, later) = first(&mut st, said);
        assert!(!st.landed() && !st.landed_seed_armed(), "{said}: {a:?}");
        assert_eq!(later, land_instruction(), "{said}");
    }
    // Dropped by a live read before the first point.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    st.took();
    assert!(!st.landed_seed_armed());
    let (_, later) = first(&mut st, "All done. Nothing left to do.");
    assert!(!st.landed());
    assert_eq!(later, land_instruction());
    // Dropped by a reply that no longer says done, for good: the landing
    // told at the start moves to open.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    let (_, later) = first(&mut st, PROGRESS);
    assert!(!st.landed() && !st.landed_seed_armed());
    assert_eq!(st.face_moved(), Some(TaskFace::Open));
    assert_eq!(later, land_instruction());
    // Started as Off lifted: landed stays landed, the episode is over.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    st.start_lifted_off();
    let (a, _) = first(&mut st, LANDING_REPLY);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed());
    assert_eq!(st.land_episode(), (false, 0));
    // Under Cruise: dropped, the done claim owed Cruise's prompt.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    st.observe(&idle(LANDING_REPLY, None), t0());
    assert!(!st.landed() && !st.task_done());
    assert_eq!(st.land_episode(), (false, 0));
    let a = decide_turn_end(&st, &idle(LANDING_REPLY, None), &cfg(), t0() + 60 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_DONE_CHECK), "{a:?}");
    // A wall over the reply: dropped.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    st.observe(
        &land(TurnEndReading {
            wall: Some(WallKind::Overloaded),
            wall_message: "API Error: 529 Overloaded.".to_string(),
            ..idle(LANDING_REPLY, None)
        }),
        t0(),
    );
    assert!(!st.landed() && !st.landed_seed_armed());
}

/// A SEEDED LANDING HOLDS ONLY WHERE THE LAST WORDS ARE STILL THE LANDING'S
/// OWN ([`said_mark`], kept by the journal at the landing's point): the same
/// words take it up — wrapped onto other rows too (a resize, a relaunched
/// agent's conversation drawn again) — and another reply drops it, though it
/// claims done as well. That reply is a turn no loop watched (typed while
/// Off had stopped the loop: the review of Together D3, 2026-10-17): never
/// `landed`, and its done claim — the retired token, a done said in words —
/// is a first one, which Land asks to land. NEGATIVE CONTROLS: a seed with
/// no words (a journal a build before the mark wrote) holds over another
/// clean defense, though not over the token reply, which is none; and this
/// test with the words' check taken out of [`TurnEndState::observe`] fails
/// at the first `!st.landed()`.
#[test]
fn a_seeded_landing_holds_only_where_the_last_words_are_the_landings_own() {
    let landing = said_mark(Some(LANDING_REPLY));
    assert!(landing.is_some());
    let first = |words: Option<u64>, said: &str| {
        let mut st = TurnEndState::default();
        st.seed_landed(words);
        let now = t0();
        assert_eq!(st.face_start(), TaskFace::Landed, "armed: {said}");
        let r = land(idle(said, None));
        let a = at(&mut st, &r, now);
        let later = now + st.short_wait().unwrap_or_default();
        let later = decide_turn_end(&st, &r, &cfg(), later);
        (st, a, later)
    };
    // The landing's own words, as drawn and wrapped at another width.
    let rewrapped = "Committed and pushed the parser\nand its tests.\nNothing left: all of it is \
                     merged\ninto the remote main branch.";
    assert_eq!(said_mark(Some(rewrapped)), landing);
    for said in [LANDING_REPLY, rewrapped] {
        let (mut st, a, later) = first(landing, said);
        assert_eq!((a, later), (TurnEndAction::Nothing, TurnEndAction::Nothing));
        assert!(st.landed(), "{said}");
        assert_eq!(st.face_moved(), None, "{said}: landed from the start");
    }
    // ANOTHER REPLY ON THE TOKEN: never landed, Land's instruction owed.
    let theirs = "Added the module docstring and committed it.\nATERM-TASK-DONE";
    assert_ne!(said_mark(Some(theirs)), landing);
    let (mut st, _, later) = first(landing, theirs);
    assert!(!st.landed() && !st.task_done(), "their reply is no landing");
    assert_eq!(later, land_instruction());
    assert_eq!(st.face_moved(), Some(TaskFace::Open));
    // …and a done in words alone: a first done report, Land's instruction
    // typed once after the back-off, as at any first point.
    let (st, _, later) = first(landing, "All done. Nothing left to do.");
    assert!(!st.landed() && !st.landed_seed_armed());
    assert_eq!(later, land_instruction());
    // NEGATIVE CONTROL: no words kept, no words compared — another clean
    // defense holds it; the token reply, no defense, does not.
    let defended = "Added the module docstring.\nNothing left: it is merged into the remote main \
                    branch.";
    assert_ne!(said_mark(Some(defended)), landing);
    let (st, a, _) = first(None, defended);
    assert!(st.landed());
    assert_eq!(a, TurnEndAction::Nothing);
    let (st, _, later) = first(None, theirs);
    assert!(!st.landed());
    assert_eq!(later, land_instruction());
    // No words at all mark nothing.
    assert_eq!(said_mark(None), None);
    assert_eq!(said_mark(Some(" ⏺ … ")), None);
}

/// THE LAST REPLY SAID DONE ([`TurnEndState::said_done`]: under Land the
/// loop's `IdlePoint::done_under_land`, §9.4.8) only where the last point's
/// reply was read and said so. A REPLY A WALL CUT SAID NOTHING, though the
/// done report before it stands: under Land a first done report, the land
/// instruction typed, its turn ended at the memory banner, the restart in
/// place made there, and the relaunched agent's first point read with no
/// read that saw it run (its resumed reply progress). That agent was
/// mid-landing, and the relaunch's carry-on is owed it (the review of
/// 2026-10-16: read as done, its carry-on was settled with nothing typed,
/// its ledger row said "its last reply said done", and a land nudge was
/// typed in the carry-on's place). NEGATIVE CONTROLS: the first done report
/// itself says done, and the landing's clean defense in the banner's place
/// lands the task, which says done.
#[test]
fn a_reply_a_wall_cut_never_said_done_though_the_done_report_before_it_stands() {
    let t = t0();
    let s = Duration::from_secs;
    let done = "All done. Nothing left to do.";
    let reported = || {
        let mut st = TurnEndState::default();
        let a = act(&mut st, &land(idle(done, Some(s(600)))), t);
        assert_eq!(a.rule_id(), Some(RULE_LAND), "{a:?}");
        st
    };
    // NEGATIVE CONTROL: the first done report says done.
    let st = reported();
    assert_eq!(st.done, 1);
    assert!(st.said_done());
    // The landing's turn ends at the memory banner: restarted in place.
    let mut st = reported();
    let banner = land(walled(
        WallKind::Memory,
        "Claude Code is running low on memory",
        Some(s(300)),
    ));
    let a = act(&mut st, &banner, t + s(400));
    assert!(
        matches!(
            a,
            TurnEndAction::Restart {
                why: Restart::Memory,
                ..
            }
        ),
        "{a:?}"
    );
    assert_eq!(st.done, 1, "the done report before it stands");
    assert!(!st.said_done(), "the banner said nothing of the task");
    // The relaunched agent's first point, no read having seen it run.
    st.observe(&land(idle(PROGRESS, None)), t + s(500));
    assert!(!st.said_done(), "mid-landing: the carry-on is owed");
    // NEGATIVE CONTROL: the landing's defense in the banner's place.
    let mut st = reported();
    act(
        &mut st,
        &land(idle(LANDING_REPLY, Some(s(300)))),
        t + s(400),
    );
    assert!(st.landed());
    assert!(st.said_done());
}

/// NOR A REPLY SOMEONE ELSE'S TURN IS WRITING, NOR A PERSON'S OWN EXCHANGE
/// ([`TurnEndState::said_done`]): a done report, then a live read of a turn
/// no act of the policy's asked (a person's, an orchestrator's), says done
/// no more; and a person's own exchange whose reply says done is theirs to
/// answer, never the task's done under Land. NEGATIVE CONTROLS: the same
/// done report with no live read since, and the same reply with nobody's
/// hand in its turn, say done.
#[test]
fn someone_elses_live_turn_and_a_persons_own_exchange_never_said_done() {
    let t = t0();
    let done = "All done. Nothing left to do.";
    let mut st = TurnEndState::default();
    st.observe(&land(idle(done, Some(Duration::from_secs(600)))), t);
    assert!(st.said_done(), "the negative control: no live read since");
    st.took();
    assert!(st.other_turn());
    assert!(!st.said_done(), "someone else's turn runs");
    // A person's own exchange, answered in seconds: theirs.
    let quick = Some(Duration::from_secs(5));
    let mut st = TurnEndState::default();
    st.observe(
        &land(TurnEndReading {
            person: Some(Duration::from_secs(8)),
            ..idle(done, quick)
        }),
        t,
    );
    assert!(st.left_to_person());
    assert!(!st.said_done(), "their exchange is theirs to answer");
    // NEGATIVE CONTROL: nobody's hand in the turn.
    let mut st = TurnEndState::default();
    st.observe(&land(idle(done, quick)), t);
    assert!(!st.left_to_person());
    assert!(st.said_done());
}
