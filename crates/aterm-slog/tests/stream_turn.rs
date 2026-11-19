// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STREAM'S TURNS (HX-15h): a harness's `turn` record through the tracker's stream source
//! ([`TurnTracker::stream_turn`]), stamped as a harness claim, and the stand-down of every other
//! source of turns while the stream feeds them: the screen's verdicts, the program's claimed
//! edges and the transcript's prompts and ends.

use std::collections::BTreeSet;

use aterm_models_session::turn_tracker_model;
use aterm_slog::{
    ActWord, Actor, BoxWord, ClaimEdge, Conf, EdgeOutcome, HarnessStamp, Key, Log, Lvl, Node, Op,
    Rec, SETTLE_MS, ScreenStep, ShortId, Src, StreamTurn, TurnOut, TurnTracker, TxEvent, TxPos,
    TxState, Verdict, WallJudge, WallKind,
};
use aterm_spec::interp::{State, admits, with_buggy};
use aterm_spec::rng::XorShift;

use crate::conformance_turn::{
    Rig, Step, StreamEdge, StreamSays, apply, checked, drive, project, see_in,
};
use crate::support::{WALL0, agent_session, at, lines, time_order_holds};

const HOLDER: &str = "claude-code-alab@4242";

fn sid() -> Actor {
    Actor::sid(ShortId::new("t0000000001").expect("an ID"))
}

fn stamp(hseq: u64) -> HarnessStamp {
    HarnessStamp::new(HOLDER, sid(), Some(hseq)).expect("an ID")
}

fn rig() -> (Log, TurnTracker) {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    (log, TurnTracker::new(run))
}

fn turns(log: &Log, op: Op) -> Vec<Rec> {
    log.window()
        .filter(|r| r.op == op && r.node.lvl == Lvl::Turn)
        .cloned()
        .collect()
}

fn stamped(r: &Rec, hseq: u64) {
    assert_eq!(
        (
            r.src,
            r.conf,
            r.by,
            r.f.get_str(Key::Holder),
            r.f.get_u(Key::Hseq)
        ),
        (Src::Harness, Conf::Claim, sid(), Some(HOLDER), Some(hseq)),
        "{r:?}"
    );
}

fn started(turn: u64, by: &str) -> StreamTurn<'_> {
    StreamTurn::Started {
        turn,
        by: Some(by),
        peer: None,
    }
}

/// A STREAM TURN OPENS AND CLOSES under the stamp: `open t1 turn turn_by=person hseq= holder=
/// by=<sid> src=harness conf=claim`, then its close `out=done` stamped the same. A repeated
/// `started` of the open turn writes nothing; one of a new number supersedes it. NEGATIVE
/// CONTROL: an end that names another turn than the open one closes nothing.
#[test]
fn a_stream_turn_opens_and_closes_stamped_as_a_harness_claim() {
    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(1, "person"), &stamp(3), at(100)));
    let opened = turns(&log, Op::Open);
    assert_eq!(opened.len(), 1);
    stamped(&opened[0], 3);
    assert_eq!(opened[0].f.get_word(Key::TurnBy), Some("person"));
    assert!(tr.stream_turns());

    assert!(tr.stream_turn(&mut log, started(1, "person"), &stamp(4), at(150)));
    assert_eq!(turns(&log, Op::Open).len(), 1, "a repeat writes nothing");
    assert_eq!(log.window().filter(|r| r.op == Op::Amend).count(), 0);

    assert!(tr.stream_turn(
        &mut log,
        StreamTurn::Ended {
            turn: 7,
            out: TurnOut::Done
        },
        &stamp(5),
        at(200)
    ));
    assert!(
        turns(&log, Op::Close).is_empty(),
        "CONTROL: another turn's end"
    );

    assert!(tr.stream_turn(
        &mut log,
        StreamTurn::Ended {
            turn: 1,
            out: TurnOut::Done
        },
        &stamp(6),
        at(300)
    ));
    let closed = turns(&log, Op::Close);
    assert_eq!(closed.len(), 1);
    stamped(&closed[0], 6);
    assert_eq!(closed[0].f.get_word(Key::Out), Some("done"));
    assert_eq!(tr.turn(), None);

    assert!(tr.stream_turn(&mut log, started(2, "harness"), &stamp(7), at(400)));
    assert!(tr.stream_turn(&mut log, started(3, "agent"), &stamp(8), at(500)));
    let closed = turns(&log, Op::Close);
    assert_eq!(closed.len(), 2);
    stamped(&closed[1], 8);
    assert_eq!(closed[1].f.get_word(Key::Out), Some("superseded"));
    assert_eq!(turns(&log, Op::Open).len(), 3);
    let got = lines(&log);
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
}

/// A STREAM TURN TAKES A SCREEN TURN ALREADY OPEN: the screen's `busy` opened `t1 src=screen`;
/// the stream's `started` amends it stamped (`turn_by=`, `hseq=`, `holder=`, `src=harness
/// conf=claim by=<sid>`) and opens no second turn. From then the screen's rest closes nothing,
/// held far past the settle, and only the stream's end closes it. NEGATIVE CONTROL: without the
/// stream, the same rest closes the screen's turn at the settle.
#[test]
fn a_stream_turn_takes_the_screens_open_turn_and_the_screens_rest_closes_nothing() {
    let (mut log, mut tr) = rig();
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(100));
    let screen = turns(&log, Op::Open);
    assert_eq!(screen.len(), 1);
    assert_eq!(screen[0].src, Src::Screen);

    assert!(tr.stream_turn(&mut log, started(1, "person"), &stamp(2), at(200)));
    assert_eq!(turns(&log, Op::Open).len(), 1, "no second turn");
    let amend: Vec<Rec> = log
        .window()
        .filter(|r| r.op == Op::Amend && r.node == screen[0].node)
        .cloned()
        .collect();
    assert_eq!(amend.len(), 1);
    stamped(&amend[0], 2);
    assert_eq!(amend[0].f.get_word(Key::TurnBy), Some("person"));

    let _ = tr.observe(&mut log, Verdict::Idle, None, at(300));
    let _ = tr.tick(&mut log, at(300 + 10 * SETTLE_MS));
    assert!(turns(&log, Op::Close).is_empty(), "the screen's rest");
    assert_eq!(tr.wake_at(), None, "no settle is armed");
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(20_000));
    assert_eq!(turns(&log, Op::Open).len(), 1, "the screen opens none");

    assert!(tr.stream_turn(
        &mut log,
        StreamTurn::Ended {
            turn: 1,
            out: TurnOut::Interrupted
        },
        &stamp(3),
        at(21_000)
    ));
    let closed = turns(&log, Op::Close);
    assert_eq!(closed.len(), 1);
    stamped(&closed[0], 3);
    assert_eq!(closed[0].f.get_word(Key::Out), Some("interrupted"));

    // CONTROL: no stream, the same screen.
    let (mut log, mut tr) = rig();
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(100));
    let _ = tr.observe(&mut log, Verdict::Idle, None, at(300));
    let _ = tr.tick(&mut log, at(300 + 10 * SETTLE_MS));
    assert_eq!(
        turns(&log, Op::Close).len(),
        1,
        "CONTROL: the screen's rest"
    );
}

/// ANOTHER HOLDER'S RECORDS START OVER (HX-15h review, D8): the stream's hold on the turns is its
/// holder's. A turn holder A left open under its number 5 as its feed ended (the screen busy, so
/// nothing settles it) is no one's to holder B: B's first record, the end of its own turn 1,
/// closes it in B's stamp; and where B's first record is a `started` of A's number, it takes the
/// turn with B's amend rather than repeating A's. NEGATIVE CONTROL: A's own end of another number
/// than the one it holds closes nothing.
#[test]
fn another_holders_turn_records_start_over() {
    const B: &str = "claude-code-alab@5151";
    let b = |hseq: u64| HarnessStamp::new(B, sid(), Some(hseq)).expect("an ID");
    let ended = |turn: u64| StreamTurn::Ended {
        turn,
        out: TurnOut::Done,
    };

    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(5, "person"), &stamp(1), at(100)));
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(200));
    tr.set_stream_turns(false, at(300));
    assert!(tr.stream_turn(&mut log, ended(1), &stamp(2), at(400)));
    assert!(
        turns(&log, Op::Close).is_empty(),
        "CONTROL: A's end of another number"
    );
    tr.set_stream_turns(false, at(500));
    assert!(tr.stream_turn(&mut log, ended(1), &b(1), at(600)));
    let closed = turns(&log, Op::Close);
    assert_eq!(closed.len(), 1, "B's end closes the turn A left");
    assert_eq!(
        (
            closed[0].f.get_str(Key::Holder),
            closed[0].f.get_word(Key::Out)
        ),
        (Some(B), Some("done"))
    );
    assert_eq!(tr.turn(), None);

    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(5, "person"), &stamp(1), at(100)));
    tr.set_stream_turns(false, at(300));
    assert!(tr.stream_turn(&mut log, started(5, "harness"), &b(1), at(400)));
    let amends: Vec<Rec> = log
        .window()
        .filter(|r| r.op == Op::Amend && r.node.lvl == Lvl::Turn)
        .cloned()
        .collect();
    assert_eq!(amends.len(), 1, "B takes the turn: {amends:?}");
    assert_eq!(amends[0].f.get_str(Key::Holder), Some(B));
    assert_eq!(amends[0].f.get_word(Key::TurnBy), Some("harness"));
    assert_eq!(turns(&log, Op::Open).len(), 1, "no second turn");
    assert!(tr.stream_turn(&mut log, ended(5), &b(2), at(500)));
    assert_eq!(turns(&log, Op::Close).len(), 1, "B's end closes it");
}

/// THE STAND-DOWN ENDS: once the stream no longer feeds turns, a turn it left open stays open
/// and the screen's rest, already read, closes it at the settle; the screen opens turns again.
#[test]
fn when_the_stream_stops_feeding_turns_the_screen_closes_and_opens_them_again() {
    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(1, "harness"), &stamp(1), at(100)));
    let _ = tr.observe(&mut log, Verdict::Idle, None, at(200));
    tr.set_stream_turns(false, at(300));
    assert!(!tr.stream_turns());
    let _ = tr.tick(&mut log, at(300 + SETTLE_MS));
    let closed = turns(&log, Op::Close);
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].src, Src::Screen);
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(5_000));
    assert_eq!(turns(&log, Op::Open).len(), 2, "the screen's own turn");
}

/// THE PROGRAM'S CLAIM AND THE TRANSCRIPT STAND ASIDE: under the stream a claimed beginning
/// opens nothing ([`EdgeOutcome::Aside`]), and a transcript prompt names the stream's turn
/// (`vref=`) and opens or supersedes none, and its turn end closes none.
#[test]
fn the_claim_and_the_transcript_open_and_close_no_turn_under_the_stream() {
    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(1, "ctl"), &stamp(1), at(100)));
    assert_eq!(
        tr.claim_edge(&mut log, ClaimEdge::Began, at(150), &mut || true),
        EdgeOutcome::Aside
    );
    let pos = |off: u64, ms: u64| TxPos {
        fp: 0x5eed,
        id: Some((1, 2)),
        off,
        wall_ms: WALL0 + ms,
    };
    let _ = tr.transcript(
        &mut log,
        TxEvent::Bind {
            len: 0,
            floor_ms: WALL0 - 60_000,
        },
        pos(0, 160),
        TxState::On,
        at(160),
    );
    let vref = ShortId::new("prompt-1").expect("an id");
    let _ = tr.transcript(
        &mut log,
        TxEvent::Prompt { vref },
        pos(200, 170),
        TxState::On,
        at(170),
    );
    let _ = tr.transcript(
        &mut log,
        TxEvent::Prompt {
            vref: ShortId::new("prompt-2").expect("an id"),
        },
        pos(400, 180),
        TxState::On,
        at(180),
    );
    let _ = tr.transcript(
        &mut log,
        TxEvent::TurnEnd {
            vdur_ms: 80,
            tok_in: 1,
            tok_out: 2,
        },
        pos(600, 190),
        TxState::On,
        at(190),
    );
    assert_eq!(turns(&log, Op::Open).len(), 1, "one turn, the stream's");
    assert!(turns(&log, Op::Close).is_empty(), "nothing closed it");
    let t: Node = tr.turn().expect("the stream's turn runs");
    let named = log
        .window()
        .any(|r| r.op == Op::Amend && r.node == t && r.f.get(Key::Vref).is_some());
    assert!(named, "the prompt names the stream's turn");
}

/// A STEP THE SCREEN STILL SHOWS OUTLIVES THE STREAM'S CLOSE OF ITS TURN: an approval box under
/// the stream's turn closes with it (`out=gone`, in the stream's stamp) and opens again on the
/// run, continuing it (`cont=`), so its need stands while the screen shows it; the screen's next
/// word that does not hold it closes it there. NEGATIVE CONTROL: a survey only ever stands in a
/// turn, and the stream's end ends it with the turn.
#[test]
fn the_streams_end_keeps_the_step_the_screen_still_shows_on_the_run() {
    let (mut log, mut tr) = rig();
    let run = tr.run();
    assert!(tr.stream_turn(&mut log, started(1, "person"), &stamp(1), at(100)));
    let boxed = Verdict::Prompt(BoxWord::from_detail("bash:not-read-only"));
    let _ = tr.observe(&mut log, boxed, None, at(200));
    let (first, ..) = tr.view(200).step.expect("the box");
    assert!(tr.stream_turn(
        &mut log,
        StreamTurn::Ended {
            turn: 1,
            out: TurnOut::Done
        },
        &stamp(2),
        at(300)
    ));
    assert_eq!(tr.turn(), None);
    let (again, what, under_turn) = tr.view(300).step.expect("the box stands on the run");
    assert!(matches!(what, ScreenStep::Approval(_)) && !under_turn);
    assert_ne!(again, first);
    let reopened = log
        .open_entry(again)
        .map(|e| e.parent)
        .expect("the box is open");
    assert_eq!(reopened, run);
    let opened = log
        .window()
        .find(|r| r.op == Op::Open && r.node == again)
        .expect("its open");
    assert_eq!(opened.src, Src::Screen);
    assert_eq!(
        opened.f.get_str(Key::Cont),
        Some(first.to_string().as_str())
    );
    let gone = log
        .window()
        .find(|r| r.op == Op::Close && r.node == first)
        .expect("the turn's box closed with it");
    assert_eq!(gone.f.get_word(Key::Out), Some("gone"));
    assert_eq!(
        (gone.src, gone.conf, gone.by),
        (Src::Harness, Conf::Claim, sid())
    );
    let _ = tr.observe(&mut log, Verdict::Idle, None, at(400));
    assert!(!log.is_open(again), "the screen's next word closes it");
    assert_eq!(tr.view(400).step, None);
    let got = lines(&log);
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));

    // CONTROL: a survey under the turn ends with it.
    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(1, "person"), &stamp(1), at(100)));
    let _ = tr.observe(&mut log, Verdict::Survey, None, at(200));
    assert!(tr.view(200).step.is_some(), "the survey stands in the turn");
    assert!(tr.stream_turn(
        &mut log,
        StreamTurn::Ended {
            turn: 1,
            out: TurnOut::Done
        },
        &stamp(2),
        at(300)
    ));
    assert_eq!(tr.view(300).step, None, "CONTROL: the survey ended");
}

/// THE STAND-DOWN'S END SETTLES A WALL AT REST: the stream's turn, the screen walled under it;
/// once the stream no longer feeds turns, the wall arms its settle, which closes the turn
/// `out=wall:<kind>` and keeps the wall on the run, as the verdict would have. NEGATIVE CONTROL:
/// a wall the transcript's judge refuted arms nothing.
#[test]
fn when_the_stream_stops_feeding_turns_a_wall_at_rest_settles_the_turn() {
    let wall = Verdict::Wall(WallKind::UsageSession);
    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(1, "harness"), &stamp(1), at(100)));
    let _ = tr.observe(&mut log, wall, None, at(200));
    assert_eq!(tr.wake_at(), None, "no settle while the stream feeds turns");
    tr.set_stream_turns(false, at(300));
    assert_eq!(tr.wake_at(), Some(300 + SETTLE_MS));
    let _ = tr.tick(&mut log, at(300 + SETTLE_MS));
    let closed = turns(&log, Op::Close);
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].src, Src::Screen);
    assert_eq!(closed[0].f.get_word(Key::Out), Some("wall:usage-session"));
    assert!(
        tr.view(300 + SETTLE_MS)
            .step
            .is_some_and(|(_, w, under)| matches!(w, ScreenStep::Wall(_)) && !under),
        "the wall stands on the run"
    );

    // CONTROL: the judge refuted the wall.
    let (mut log, mut tr) = rig();
    assert!(tr.stream_turn(&mut log, started(1, "harness"), &stamp(1), at(100)));
    let _ = tr.observe(&mut log, wall, None, at(200));
    let pos = |off: u64, ms: u64| TxPos {
        fp: 0x5eed,
        id: Some((1, 2)),
        off,
        wall_ms: WALL0 + ms,
    };
    let _ = tr.transcript(
        &mut log,
        TxEvent::Bind {
            len: 0,
            floor_ms: WALL0 - 60_000,
        },
        pos(0, 210),
        TxState::On,
        at(210),
    );
    let _ = tr.transcript(
        &mut log,
        TxEvent::Wall {
            kind: WallKind::UsageSession,
            judge: WallJudge::Refute,
        },
        pos(200, 220),
        TxState::On,
        at(220),
    );
    tr.set_stream_turns(false, at(300));
    assert_eq!(tr.wake_at(), None, "CONTROL: a refuted wall arms nothing");
}

const SEE: &[Verdict] = &[
    Verdict::Busy,
    Verdict::Busy,
    Verdict::Idle,
    Verdict::Idle,
    Verdict::Prompt(BoxWord::QUESTION),
    Verdict::Unknown,
    Verdict::Survey,
    Verdict::Question,
    Verdict::Unresponsive,
    Verdict::Gone,
    Verdict::Wall(WallKind::UsageSession),
    Verdict::Wall(WallKind::Overloaded),
];

const BY: &[&str] = &["harness", "person", "ctl", "agent", "peer"];

const ENDS: &[TurnOut] = &[
    TurnOut::Done,
    TurnOut::Error,
    TurnOut::Interrupted,
    TurnOut::Wall(None),
];

/// What a walk step drove, beside the model's names: the stream source's cases (`says`: what
/// the source did with a stream step, by the rig's reading of the hold before it; `held`:
/// whether the stream held the open turn before the step).
fn stream_cases(
    says: Option<StreamSays>,
    held: bool,
    step: Step,
    prev: &State,
    next: &State,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    match (step, says) {
        (Step::Stream(StreamEdge::Started(..)), Some(StreamSays::Opens)) => {
            out.push("a started opens a turn");
        }
        (Step::Stream(_), Some(StreamSays::Supersedes)) => {
            out.push("a started of a new number supersedes the stream's turn");
        }
        (Step::Stream(_), Some(StreamSays::Takes | StreamSays::Repeats)) => {
            out.push("a started takes or repeats the turn");
        }
        (Step::Stream(StreamEdge::Started(..)), Some(StreamSays::History)) => {
            out.push("a started of a number the stream named writes nothing");
        }
        (Step::Stream(StreamEdge::Ended(..)), Some(StreamSays::History)) => {
            out.push("an end of a number the stream named closes no other turn");
        }
        (Step::Stream(StreamEdge::Ended(..)), Some(StreamSays::Closes))
            if prev["turn"] == 1 && next["turn"] == 0 =>
        {
            out.push(if held {
                "an end closes the turn"
            } else {
                "an end of a newer number closes a turn the stream did not hold"
            });
            if prev["sturn"] == 1 {
                out.push(match prev["step"] {
                    1 => "the end keeps the box on the run",
                    2 => "the end keeps the dialog on the run",
                    3 => "the end keeps the wall on the run",
                    4 => "the end keeps the stall on the run",
                    _ => "the end ends the survey",
                });
            }
        }
        (Step::Stream(StreamEdge::Ended(..)), _) => out.push("an end that ends nothing"),
        _ => {}
    }
    match step {
        Step::StreamFeeds(false) if prev["sf"] == 1 && next["pend"] == 1 => {
            out.push(if prev["word"] == 5 {
                "the stand-down's end arms a wall's settle"
            } else {
                "the stand-down's end arms a rest's settle"
            });
        }
        Step::StreamFeeds(false) if prev["sf"] == 1 && prev["turn"] == 1 => {
            out.push("the stand-down's end leaves the turn open");
        }
        Step::See(Verdict::Busy) if prev["sf"] == 1 && prev["turn"] == 0 => {
            out.push("the screen's busy opens no turn under the stream");
        }
        Step::See(Verdict::Idle) if prev["sf"] == 1 && prev["turn"] == 1 => {
            out.push("the screen's rest arms nothing under the stream");
        }
        Step::See(Verdict::Prompt(b))
            if b.is_question() && prev["sf"] == 1 && prev["turn"] == 0 =>
        {
            out.push("a dialog hangs on the run under the stream");
        }
        Step::See(Verdict::Gone) if prev["sf"] == 1 && prev["turn"] == 1 => {
            out.push("the program's end ends the stream's turn");
        }
        Step::TxEnd if prev["sf"] == 1 && prev["turn"] == 1 => {
            out.push("the transcript's end closes nothing under the stream");
        }
        Step::Half | Step::Wake
            if prev["turn"] == 1 && next["turn"] == 0 && held && prev["sf"] == 0 =>
        {
            out.push("the screen's rest closes a turn the stream left");
        }
        _ => {}
    }
    out
}

/// TIER-1 CONFORMANCE OF THE STREAM SOURCE: random walks that mix the harness's `turn` records
/// and the host's word on whether the stream feeds the turns into the screen's, the
/// transcript's, the clock's and the inputs' steps, each step checked against
/// `turn_tracker_model` (the turn records it wrote are exactly the model path's opens and closes),
/// and every case of the source driven.
#[test]
fn turn_tracker_conforms_to_the_stream_source() {
    let m = turn_tracker_model();
    let mut ran = BTreeSet::new();
    for seed in 1..=100u64 {
        let mut rng = XorShift::new(seed);
        let mut rig = Rig::new();
        for _ in 0..300 {
            let prev = project(&rig, None);
            let step = match rng.below(26) {
                0..=2 => Step::Half,
                3 => Step::Elapse,
                4 => Step::Wake,
                5 => Step::Ctl,
                6 | 7 => Step::TxEnd,
                8 => Step::TxRefute,
                9 if prev["pd"] == 0 => {
                    let by = [Actor::Ctl, Actor::Harness][rng.index(2)];
                    let act = [None, Some(ActWord::Approve), Some(ActWord::Deny)][rng.index(3)];
                    Step::Admit(by, act, None)
                }
                9 | 10 => Step::Accept,
                11 => Step::Person(None),
                // The number the stream named last, one below it, or one above it.
                12..=14 => Step::Stream(StreamEdge::Started(
                    near(&rig, &mut rng),
                    BY[rng.index(BY.len())],
                )),
                15 | 16 => Step::Stream(StreamEdge::Ended(
                    near(&rig, &mut rng),
                    ENDS[rng.index(ENDS.len())],
                )),
                17 => Step::StreamFeeds(true),
                18 => Step::StreamFeeds(false),
                _ => {
                    let mut v = SEE[rng.index(SEE.len())];
                    if rng.below(5) == 0 {
                        let b = ["bash:not-read-only", "edit"][rng.index(2)];
                        v = Verdict::Prompt(BoxWord::from_detail(b));
                    }
                    Step::See(v)
                }
            };
            if let Step::See(v) = step {
                ran.insert(see_in(&rig, v));
            }
            let held = rig
                .stream_hold
                .and_then(|(_, t)| t)
                .is_some_and(|t| rig.tracker.turn() == Some(t));
            let says = match step {
                Step::Stream(edge) => Some(rig.stream_says(edge)),
                _ => None,
            };
            apply(&m, &mut rig, step);
            let next = project(&rig, None);
            for case in stream_cases(says, held, step, &prev, &next) {
                ran.insert(case);
            }
        }
        assert!(rig.log.open_under(rig.run, Lvl::Turn).count() <= 1);
        assert!(
            !rig.stream_moved,
            "seed {seed}: a turn the stream did not move"
        );
    }
    for case in [
        "SeeOtherWall",
        "a started opens a turn",
        "a started of a new number supersedes the stream's turn",
        "a started takes or repeats the turn",
        "an end closes the turn",
        "the end keeps the box on the run",
        "the end keeps the dialog on the run",
        "the end keeps the wall on the run",
        "the end keeps the stall on the run",
        "the end ends the survey",
        "an end that ends nothing",
        "a started of a number the stream named writes nothing",
        "an end of a number the stream named closes no other turn",
        "an end of a newer number closes a turn the stream did not hold",
        "the stand-down's end arms a wall's settle",
        "the stand-down's end arms a rest's settle",
        "the stand-down's end leaves the turn open",
        "the screen's busy opens no turn under the stream",
        "the screen's rest arms nothing under the stream",
        "a dialog hangs on the run under the stream",
        "the program's end ends the stream's turn",
        "the transcript's end closes nothing under the stream",
        "the screen's rest closes a turn the stream left",
    ] {
        assert!(ran.contains(case), "the walks never drove `{case}`");
    }
}

/// A turn number for a walk's stream step: the newest the stream named, one below it or one
/// above it (at least 1).
fn near(rig: &Rig, rng: &mut XorShift) -> u64 {
    let named = rig.stream_hold.map_or(0, |(n, _)| n);
    (named + rng.below(3)).saturating_sub(1).max(1)
}

fn with(s: &State, kv: &[(&'static str, i64)]) -> State {
    let mut b = s.clone();
    for (k, v) in kv {
        b.insert(k, *v);
    }
    b
}

/// NEGATIVE CONTROLS OF THE STREAM SOURCE: from a real state under the stream, each `Buggy = 3`
/// mutant's successor, hand-built, is one the model refuses, the mutant admits, and its law
/// breaks; the real tracker takes the model's step from the same state instead.
#[test]
fn the_stream_source_conformance_rejects_turns_the_stream_did_not_move() {
    let m = turn_tracker_model();
    let buggy = with_buggy(&m, 3);
    let refused = |from: &State, to: &State, mutant: &str, law: &str| {
        assert!(
            admits(&m, from, to).is_none(),
            "{mutant}: the model admits it"
        );
        assert!(
            buggy.successors(mutant, from).contains(to),
            "{mutant}: not the mutant's successor"
        );
        assert!(!m.check_invariant(law, to), "{mutant}: {law} holds");
    };

    // A rest under the stream arms no settle, held far past it.
    let mut rig = drive(&[
        Step::Stream(StreamEdge::Started(1, "person")),
        Step::See(Verdict::Busy),
        Step::See(Verdict::Idle),
    ]);
    let resting = project(&rig, None);
    assert_eq!(
        (
            resting["sf"],
            resting["turn"],
            resting["pend"],
            resting["word"]
        ),
        (1, 1, 0, 2)
    );
    checked(&mut rig, Step::Wait(10 * SETTLE_MS));
    assert!(rig.tracker.turn().is_some(), "the screen's rest closed it");
    let armed = with(&resting, &[("pend", 1), ("ticks", 0)]);
    refused(&resting, &armed, "StreamRestArms", "StreamTurnsNeverSettle");

    // The screen's busy opens no turn under the stream.
    let mut rig = drive(&[
        Step::Stream(StreamEdge::Started(1, "person")),
        Step::Stream(StreamEdge::Ended(1, TurnOut::Done)),
    ]);
    let ended = project(&rig, None);
    assert_eq!((ended["sf"], ended["turn"], ended["cw"]), (1, 0, 13));
    checked(&mut rig, Step::See(Verdict::Busy));
    assert_eq!(rig.tracker.turn(), None, "the screen opened a turn");
    let opened = with(&ended, &[("turn", 1), ("sg", 1)]);
    refused(
        &ended,
        &opened,
        "ScreenOpensUnderStream",
        "OnlyTheStreamMovesItsTurns",
    );

    // The transcript's end closes no turn under the stream.
    let mut rig = drive(&[Step::Stream(StreamEdge::Started(1, "harness"))]);
    let open = project(&rig, None);
    checked(&mut rig, Step::TxEnd);
    assert!(rig.tracker.turn().is_some(), "the transcript closed it");
    let closed = with(&open, &[("turn", 0), ("cw", 12), ("cticks", 0), ("sg", 1)]);
    refused(
        &open,
        &closed,
        "TxClosesUnderStream",
        "OnlyTheStreamMovesItsTurns",
    );

    // The stream's end keeps the screen's wall, on the run.
    let mut rig = drive(&[
        Step::Stream(StreamEdge::Started(1, "harness")),
        Step::See(Verdict::Wall(WallKind::UsageSession)),
    ]);
    let walled = project(&rig, None);
    assert_eq!((walled["turn"], walled["step"], walled["sturn"]), (1, 3, 1));
    checked(
        &mut rig,
        Step::Stream(StreamEdge::Ended(1, TurnOut::Wall(None))),
    );
    let kept = project(&rig, None);
    assert_eq!(
        (kept["turn"], kept["step"], kept["sturn"], kept["wg"]),
        (0, 3, 0, 0)
    );
    let taken = with(
        &walled,
        &[
            ("turn", 0),
            ("cw", 13),
            ("cticks", 0),
            ("step", 0),
            ("sturn", 0),
            ("pend", 0),
            ("ticks", 0),
            ("wg", 1),
        ],
    );
    refused(
        &walled,
        &taken,
        "StreamEndTakesWall",
        "WallNeedOutlivesItsTurn",
    );
}
