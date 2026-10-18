// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE TASK'S FACE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4.2):
// `TaskFace::of` over the policy's memory, the face the loop tells as the
// memory moves (`TurnEndState::face_moved`), someone else's live turn
// (`TurnEndState::took`), and the landing a loop before this one landed
// (`TurnEndState::seed_landed`, §9.4.5). Included from `turn_end_tests.rs`
// (`mod face`), so its readings and acts are in scope. Each behaviour has
// its negative control.

use crate::harness::acts::TaskFace;

/// The worker's own token, as Land's instruction asks for it.
const TOKEN_REPLY: &str =
    "Committed and pushed the parser and its tests; nothing is left.\nATERM-TASK-DONE";
/// A reply that says no done.
const PROGRESS: &str = "Committing the parser now; the lexer is next.";

/// A state at the policy's memory `set` leaves on a fresh one.
fn state(set: impl FnOnce(&mut TurnEndState)) -> TurnEndState {
    let mut st = TurnEndState::default();
    set(&mut st);
    st
}

/// THE FACE TABLE, line by line of §9.4.2, each line's state set by hand:
/// someone else's turn read live (open, or spent when the landing ended
/// quiet), a person's own exchange (theirs, before a landing's end), landed
/// (or a landing seeded for the first point), done, a spent landing whose
/// last act was answered with no done, and open. A spent landing whose last nudge is still
/// awaited, or owed again behind a wall, reads open; a quiet one whose
/// question's land answer is awaited stays spent. NEGATIVE CONTROLS in the
/// table itself: a done task is never landed, a landed one under someone
/// else's live turn is never landed, and a landing that landed at its LAST
/// nudge is never spent — not under someone else's turn, not once that turn
/// ended with no done (the agent did reply done; spent's words say it did
/// not).
#[test]
fn the_face_is_a_function_of_the_policys_memory_in_the_designs_order() {
    let nudged_out = |st: &mut TurnEndState| {
        st.land.typed = true;
        st.land.nudges = LAND_NUDGES;
    };
    let spent = |st: &mut TurnEndState| {
        nudged_out(st);
        st.land.quiet = true;
    };
    let landed = |st: &mut TurnEndState| {
        st.done = 2;
        st.landed = true;
    };
    let rows: Vec<(&str, TurnEndState, bool, TaskFace)> = vec![
        ("fresh", TurnEndState::default(), false, TaskFace::Open),
        (
            "one done report",
            state(|s| s.done = 1),
            false,
            TaskFace::Open,
        ),
        ("landed", state(landed), false, TaskFace::Landed),
        ("done", state(|s| s.done = 2), false, TaskFace::Done),
        (
            "finished by the worker",
            state(|s| {
                s.done = 2;
                s.finished = true;
            }),
            false,
            TaskFace::Done,
        ),
        (
            "a person's own exchange",
            state(|s| {
                s.done = 2;
                s.exchange = true;
            }),
            false,
            TaskFace::Theirs,
        ),
        (
            "a person's own exchange over a spent landing",
            state(|s| {
                spent(s);
                s.done = 2;
                s.exchange = true;
            }),
            false,
            TaskFace::Theirs,
        ),
        ("spent", state(spent), false, TaskFace::Spent),
        (
            "spent, its last nudge awaited",
            state(|s| {
                nudged_out(s);
                s.landing = true;
            }),
            false,
            TaskFace::Open,
        ),
        (
            "spent, its last nudge cut by a wall",
            state(|s| {
                nudged_out(s);
                s.cut_land = true;
            }),
            false,
            TaskFace::Open,
        ),
        (
            "quiet, a question's land answer awaited",
            state(|s| {
                spent(s);
                s.landing = true;
            }),
            false,
            TaskFace::Spent,
        ),
        (
            "landed at its last nudge",
            state(|s| {
                nudged_out(s);
                landed(s);
            }),
            false,
            TaskFace::Landed,
        ),
        (
            "someone else's turn after a landing at its last nudge",
            state(|s| {
                nudged_out(s);
                landed(s);
            }),
            true,
            TaskFace::Open,
        ),
        (
            "that turn ended with no done, the nudges spent",
            state(nudged_out),
            false,
            TaskFace::Open,
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
            "someone else's turn after done",
            state(|s| s.done = 2),
            true,
            TaskFace::Open,
        ),
        (
            "someone else's turn after theirs",
            state(|s| {
                s.done = 2;
                s.exchange = true;
            }),
            true,
            TaskFace::Open,
        ),
        (
            "someone else's turn after spent",
            state(spent),
            true,
            TaskFace::Spent,
        ),
    ];
    for (case, st, other, want) in &rows {
        assert_eq!(TaskFace::of(st, *other), *want, "{case}");
    }
    // NEGATIVE CONTROLS: what a reader of `task_done()` alone, or one that
    // never looks at someone else's turn, would say instead.
    let done = state(|s| s.done = 2);
    assert!(done.task_done() && TaskFace::of(&done, false) != TaskFace::Landed);
    let theirs = state(|s| {
        s.done = 2;
        s.exchange = true;
    });
    assert!(theirs.task_done() && TaskFace::of(&theirs, false) != TaskFace::Landed);
    let after = state(landed);
    assert_ne!(TaskFace::of(&after, true), TaskFace::of(&after, false));
}

/// THE FACE MOVES WITH THE POLICY'S OWN POINTS, told once each
/// ([`TurnEndState::face_moved`]): the land instruction's done reply lands
/// the task (and a person's quick turn after it is theirs); the worker's
/// own token under Cruise is done; the done check's second done report is
/// done; a landing whose instruction and two nudges ended with no done is
/// spent ONLY ONCE THE SECOND NUDGE'S TURN ENDED — typed, its reply may
/// still land it. A face read again is no move. NEGATIVE CONTROL: the second
/// nudge's done reply lands the task, never spent.
#[test]
fn the_face_moves_at_the_points_that_end_a_task_and_is_told_once() {
    // Land: instruction, done reply — landed.
    let mut now = t0();
    let mut st = TurnEndState::default();
    assert_eq!(st.face_start(), TaskFace::Open);
    assert_eq!(
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now),
        land_instruction()
    );
    assert_eq!(st.face_moved(), None, "an act moves no face");
    now += 2 * MIN;
    at(&mut st, &land(idle(TOKEN_REPLY, Some(2 * MIN))), now);
    assert_eq!(st.face_moved(), Some(TaskFace::Landed));
    assert_eq!(st.face_moved(), None, "told once");
    at(&mut st, &land(idle(TOKEN_REPLY, None)), now + MIN);
    assert_eq!(st.face_moved(), None, "the same point read again");
    // …and a person's quick turn after it is theirs.
    now += 5 * MIN;
    let thanks = land(TurnEndReading {
        person: Some(Duration::from_secs(8)),
        ..idle("You're welcome!", Some(Duration::from_secs(5)))
    });
    at(&mut st, &thanks, now);
    assert_eq!(st.face_moved(), Some(TaskFace::Theirs));

    // Cruise: the worker's own token — done, never landed.
    let mut st = TurnEndState::default();
    st.face_start();
    act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t0());
    at(&mut st, &idle(TOKEN_REPLY, Some(2 * MIN)), t0() + 3 * MIN);
    assert_eq!(st.face_moved(), Some(TaskFace::Done));

    // Cruise: a token over a report that names open work is no end — the
    // challenge is typed, the face stays open, and its token reply is done.
    let mut st = TurnEndState::default();
    st.face_start();
    act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t0());
    let open = idle(
        "Committed the parser.\nStill open: the lexer.\nATERM-TASK-DONE",
        Some(2 * MIN),
    );
    let a = act(&mut st, &open, t0() + 3 * MIN);
    assert!(
        matches!(a, TurnEndAction::Type { rule_id, .. } if rule_id == RULE_OPEN_ITEMS),
        "{a:?}"
    );
    assert_eq!(st.face_moved(), None, "challenged, not done");
    at(&mut st, &idle(TOKEN_REPLY, Some(2 * MIN)), t0() + 6 * MIN);
    assert_eq!(st.face_moved(), Some(TaskFace::Done));

    // Cruise: the done check's second done report — done.
    let mut now = t0();
    let mut st = TurnEndState::default();
    st.face_start();
    st.observe(&idle("All done.", Some(3 * MIN)), now);
    assert_eq!(st.face_moved(), None, "one done report is no end");
    now += st.short_wait().unwrap_or_default();
    assert_eq!(act(&mut st, &idle("All done.", None), now), checked());
    now += MIN;
    at(&mut st, &idle("All done.", Some(MIN)), now);
    assert_eq!(st.face_moved(), Some(TaskFace::Done));

    // Land spent: the instruction and two nudges, each answered with work.
    let spend = |last: &str| {
        let mut now = t0();
        let mut st = TurnEndState::default();
        st.face_start();
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now);
        for _ in 0..LAND_NUDGES {
            now += 3 * MIN;
            assert_eq!(
                act(&mut st, &land(idle(PROGRESS, Some(3 * MIN))), now),
                land_nudged()
            );
        }
        // The second nudge typed spends the episode, but its turn has not
        // ended: the face is open still, and nothing is told.
        assert_eq!(st.land_episode(), (true, LAND_NUDGES));
        assert_eq!(st.face(), TaskFace::Open, "its turn has not ended");
        assert_eq!(st.face_moved(), None);
        st.took();
        assert_eq!(st.face_moved(), None, "its own turn read live");
        now += 3 * MIN;
        at(&mut st, &land(idle(last, Some(3 * MIN))), now);
        (st.face_moved(), st)
    };
    assert_eq!(spend(PROGRESS).0, Some(TaskFace::Spent));
    // NEGATIVE CONTROL: the second nudge's done reply lands it.
    assert_eq!(spend(TOKEN_REPLY).0, Some(TaskFace::Landed));
}

/// The land episode spent to its last nudge, `last` its reply: what that
/// point tells, the state after it, and the time.
fn nudged_out(last: &str) -> (Option<TaskFace>, TurnEndState, Instant) {
    let mut now = t0();
    let mut st = TurnEndState::default();
    st.face_start();
    act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now);
    for _ in 0..LAND_NUDGES {
        now += 3 * MIN;
        assert_eq!(
            act(&mut st, &land(idle(PROGRESS, Some(3 * MIN))), now),
            land_nudged()
        );
    }
    now += 3 * MIN;
    at(&mut st, &land(idle(last, Some(3 * MIN))), now);
    (st.face_moved(), st, now)
}

/// A LANDING THAT LANDED AT ITS LAST NUDGE IS NEVER SPENT (the review of
/// slice C): the agent did reply done, and spent's words — the chip's title,
/// the window's record — say no done reply came. Someone else's turn after
/// it reads open at its first live read and stays open at its end with no
/// done; nothing is typed there, as before (the episode's nudges are spent:
/// what the policy types is unchanged). NEGATIVE CONTROL: the same turn
/// after a landing that ended QUIET (no done reply) stays spent throughout.
#[test]
fn a_landing_at_its_last_nudge_is_never_spent_after_it() {
    let after = |last: &str| {
        let (told, mut st, now) = nudged_out(last);
        let mut faces = vec![told];
        st.took();
        faces.push(st.face_moved());
        let ended = land(idle(PROGRESS, Some(12 * MIN)));
        let a = at(&mut st, &ended, now + 15 * MIN);
        assert_eq!(a, TurnEndAction::Nothing, "{last}: nothing typed of it");
        faces.push(st.face_moved());
        assert_eq!(st.land_episode(), (true, LAND_NUDGES), "{last}");
        (faces, st.face())
    };
    assert_eq!(
        after(TOKEN_REPLY),
        (
            vec![Some(TaskFace::Landed), Some(TaskFace::Open), None],
            TaskFace::Open
        )
    );
    // NEGATIVE CONTROL: the landing ended quiet.
    assert_eq!(
        after(PROGRESS),
        (vec![Some(TaskFace::Spent), None, None], TaskFace::Spent)
    );
}

/// A QUIET LANDING IS SPENT ONCE (the review of slice C): a question after
/// it gets the land answer, and while that answer's turn runs the face stays
/// spent — no move, so the window, which records a spent landing only on its
/// move from open, writes no second record of the same episode. The
/// answer's done reply LANDS the task (the quiet is over), and someone
/// else's turn after that is open. NEGATIVE CONTROL: the answer's reply with
/// no done leaves it spent, told nothing.
#[test]
fn a_quiet_landing_stays_spent_through_its_land_answer_until_a_done_reply_lands_it() {
    let answer = |reply: &str| {
        let (told, mut st, mut now) = nudged_out(PROGRESS);
        assert_eq!(told, Some(TaskFace::Spent));
        let mut faces = Vec::new();
        // Someone's turn that ends on a question: the land answer.
        st.took();
        faces.push(st.face_moved());
        now += 5 * MIN;
        let asked = land(idle("Did the suite pass on your machine?", Some(4 * MIN)));
        assert_eq!(act(&mut st, &asked, now), land_answered());
        faces.push(st.face_moved());
        // The answer's own turn, read live.
        st.took();
        assert!(!st.other_turn(), "the policy's own act");
        faces.push(st.face_moved());
        now += 2 * MIN;
        at(&mut st, &land(idle(reply, Some(2 * MIN))), now);
        faces.push(st.face_moved());
        // Someone else's turn after it.
        st.took();
        faces.push(st.face_moved());
        faces
    };
    assert_eq!(
        answer("All done. Nothing left to do."),
        [
            None,
            None,
            None,
            Some(TaskFace::Landed),
            Some(TaskFace::Open)
        ]
    );
    // NEGATIVE CONTROL: no done reply — spent throughout.
    assert_eq!(answer(PROGRESS), [None, None, None, None, None]);
}

/// SOMEONE ELSE'S LIVE TURN ([`TurnEndState::took`] with no act of the
/// policy's awaited): after landed, done or theirs the face is open again at
/// its first live read — told once, the same turn read again is no move —
/// and after spent it stays spent. A live read of a turn the policy's own act
/// started — a continuation, the host's line — moves nothing. The next point
/// ends it. NEGATIVE CONTROL: the same reads with the act awaited.
#[test]
fn someone_elses_live_turn_opens_the_face_and_the_policys_own_does_not() {
    let landed = || {
        let mut now = t0();
        let mut st = TurnEndState::default();
        st.face_start();
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now);
        now += 2 * MIN;
        at(&mut st, &land(idle(TOKEN_REPLY, Some(2 * MIN))), now);
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

    // Done and theirs open the same way.
    for closing in [
        idle(TOKEN_REPLY, Some(2 * MIN)),
        TurnEndReading {
            person: Some(Duration::from_secs(8)),
            ..idle(
                "Hi! What would you like me to work on?",
                Some(Duration::from_secs(5)),
            )
        },
    ] {
        let mut st = TurnEndState::default();
        st.face_start();
        act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t0());
        if closing.person.is_some() {
            // A task the policy is not carrying: the person's greeting is
            // theirs (the carry ends with the done task first).
            at(&mut st, &idle(TOKEN_REPLY, Some(2 * MIN)), t0() + 3 * MIN);
            st.face_moved();
        }
        at(&mut st, &closing, t0() + 10 * MIN);
        let face = st.face_moved();
        assert!(
            matches!(face, Some(TaskFace::Done | TaskFace::Theirs)),
            "{face:?}"
        );
        st.took();
        assert_eq!(st.face_moved(), Some(TaskFace::Open), "{face:?}");
    }

    // Spent stays spent.
    let mut now = t0();
    let mut st = TurnEndState::default();
    st.face_start();
    act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), now);
    for _ in 0..=LAND_NUDGES {
        now += 3 * MIN;
        act(&mut st, &land(idle(PROGRESS, Some(3 * MIN))), now);
    }
    assert_eq!(st.face_moved(), Some(TaskFace::Spent));
    st.took();
    assert!(st.other_turn());
    assert_eq!(st.face_moved(), None, "nothing new after spent");

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
/// first point no live read came before whose reply still says done — the
/// task landed, the episode's instruction typed, the face landed, and
/// NOTHING TYPED, now or later. It is dropped by a live read before that
/// point and by a reply that no longer says done, and the point is then
/// decided as any first point is. A loop started as Off lifted keeps the
/// done task done, its episode over; under Cruise the episode is none of its
/// own. NEGATIVE CONTROL: with no seed the same first point gets the land
/// instruction after its back-off — the instruction typed into finished work
/// after every restart.
#[test]
fn a_seeded_landing_holds_only_at_a_first_point_that_still_says_done() {
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
    for said in [TOKEN_REPLY, "All done. Nothing left to do."] {
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
        // NEGATIVE CONTROL: no seed — a done reply gets the land instruction
        // typed into finished work; the worker's own token ends the task,
        // but as one nobody asked to land (`✓ done`, never `✓ landed`).
        let mut st = TurnEndState::default();
        let (_, later) = first(&mut st, said);
        if said == TOKEN_REPLY {
            assert_eq!(later, TurnEndAction::Nothing);
            assert!(st.finished_by_worker() && !st.landed());
            assert_eq!(st.face_moved(), Some(TaskFace::Done));
        } else {
            assert_eq!(later, land_instruction(), "{said}");
            assert_eq!(st.face_moved(), None, "{said}");
        }
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
    // Started as Off lifted: done stays done, the episode is over.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    st.start_lifted_off();
    let (a, _) = first(&mut st, TOKEN_REPLY);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed());
    assert_eq!(st.land_episode(), (false, 0));
    // Under Cruise: landed, nothing typed, no episode of Land's.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    st.observe(&idle(TOKEN_REPLY, None), t0());
    assert!(st.landed());
    assert_eq!(st.land_episode(), (false, 0));
    assert_eq!(
        decide_turn_end(&st, &idle(TOKEN_REPLY, None), &cfg(), t0() + 60 * MIN),
        TurnEndAction::Nothing
    );
    // A wall over the reply: dropped.
    let mut st = TurnEndState::default();
    st.seed_landed(None);
    st.observe(
        &land(TurnEndReading {
            wall: Some(WallKind::Overloaded),
            wall_message: "API Error: 529 Overloaded.".to_string(),
            ..idle(TOKEN_REPLY, None)
        }),
        t0(),
    );
    assert!(!st.landed() && !st.landed_seed_armed());
}

/// A SEEDED LANDING HOLDS ONLY WHERE THE LAST WORDS ARE STILL THE LANDING'S
/// OWN ([`said_mark`], kept by the journal at the landing's point): the same
/// words take it up — wrapped onto other rows too (a resize, a relaunched
/// agent's conversation drawn again) — and another reply drops it, though it
/// ends on the worker's token as well. That reply is a turn no loop watched
/// (typed while Off had stopped the loop: the review of Together D3,
/// 2026-10-17): its token is THEIR task's end, read `done`, never `landed`,
/// and nothing is typed; a done said in words alone there is a first done
/// report, which Land asks to wrap up once. NEGATIVE CONTROLS: a seed with
/// no words (a journal a build before the mark wrote) holds over that reply
/// as before; and this test with the words' check taken out of
/// [`TurnEndState::observe`] fails at the first `!st.landed()`.
#[test]
fn a_seeded_landing_holds_only_where_the_last_words_are_the_landings_own() {
    let landing = said_mark(Some(TOKEN_REPLY));
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
    let rewrapped =
        "Committed and pushed the parser\nand its tests; nothing\nis left.\nATERM-TASK-DONE";
    assert_eq!(said_mark(Some(rewrapped)), landing);
    for said in [TOKEN_REPLY, rewrapped] {
        let (mut st, a, later) = first(landing, said);
        assert_eq!((a, later), (TurnEndAction::Nothing, TurnEndAction::Nothing));
        assert!(st.landed(), "{said}");
        assert_eq!(st.face_moved(), None, "{said}: landed from the start");
    }
    // ANOTHER REPLY ON THE TOKEN: their task done, never landed.
    let theirs = "Added the module docstring and committed it.\nATERM-TASK-DONE";
    assert_ne!(said_mark(Some(theirs)), landing);
    let (mut st, a, later) = first(landing, theirs);
    assert!(!st.landed(), "their reply is no landing");
    assert!(st.finished_by_worker() && st.task_done());
    assert_eq!((a, later), (TurnEndAction::Nothing, TurnEndAction::Nothing));
    assert_eq!(st.face_moved(), Some(TaskFace::Done));
    // …and a done in words alone: a first done report, Land's instruction
    // typed once after the back-off, as at any first point.
    let (st, _, later) = first(landing, "All done. Nothing left to do.");
    assert!(!st.landed() && !st.landed_seed_armed());
    assert_eq!(later, land_instruction());
    // NEGATIVE CONTROL: no words kept, no words compared.
    let (st, a, _) = first(None, theirs);
    assert!(st.landed());
    assert_eq!(a, TurnEndAction::Nothing);
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
/// typed in the carry-on's place). NEGATIVE CONTROLS: the first done report itself says
/// done, and the landing's done reply in the banner's place lands the task,
/// which says done.
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
    // NEGATIVE CONTROL: the landing's done reply in the banner's place.
    let mut st = reported();
    act(&mut st, &land(idle(done, Some(s(300)))), t + s(400));
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
