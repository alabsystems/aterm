// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A codex run's claimed boxes (GAP 26, Stage A): [`TurnTracker::claim_box`] opens one
//! `src=engine conf=claim` approval or question step per box its program's OSC 7501 report
//! shows, in a slot apart from the screen's, and closes it at the box's leave or its turn's
//! close, attributed only by an input that dismissed it.

use aterm_slog::{
    ActWord, Actor, AttributionView, Binding, BoxWord, Claimed, ClaimedBox, ClaimedReport, Log,
    Node, ScreenStep, ShortId, TurnTracker, TxEvent, TxPos, TxState, Verdict,
};

use crate::support::{WALL0, agent_session, at, lines, time_order_holds};

/// A session with an agent run open, and its tracker.
fn run() -> (Log, Node, TurnTracker) {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    (log, run, TurnTracker::new(run))
}

/// What a written program mark claims: `boxed` shown since `began`.
const fn report(boxed: Option<ClaimedBox>, began: u64) -> ClaimedReport {
    ClaimedReport { boxed, began }
}

/// The host's write of a program mark claiming a turn runs (`working` or `blocked`) and
/// showing `boxed`: the claim's turn first, then its box (`SessionLog::write_program`).
fn shown(log: &mut Log, tr: &mut TurnTracker, boxed: Option<ClaimedBox>, began: u64, ms: u64) {
    let _ = tr.claim(log, Claimed::Working, at(ms));
    let _ = tr.claim_box(log, report(boxed, began), at(ms));
}

/// The host's write of a program mark claiming a rest (`idle`): the turn's close, then the box.
fn rested(log: &mut Log, tr: &mut TurnTracker, began: u64, ms: u64) {
    let _ = tr.claim(log, Claimed::Rested, at(ms));
    let _ = tr.claim_box(log, report(None, began), at(ms));
}

/// The log's step and turn records, each without its `seq`, its time as `t=+<ms>`.
fn records(log: &Log) -> Vec<String> {
    lines(log)
        .into_iter()
        .filter(|l| {
            let w: Vec<&str> = l.split(' ').collect();
            matches!(w.get(1), Some(&"open" | &"close"))
                && w.get(2)
                    .is_some_and(|n| n.starts_with('p') || n.starts_with('t'))
        })
        .map(|l| {
            let body = l.split_once(' ').map_or(l.as_str(), |(_, b)| b).to_string();
            match body.rsplit_once(" t=") {
                Some((head, t)) => {
                    let ms = t.parse::<u64>().map_or(0, |t| t - WALL0);
                    format!("{head} t=+{ms}")
                }
                None => body,
            }
        })
        .collect()
}

fn ordered(log: &Log) {
    time_order_holds(&lines(log)).unwrap_or_else(|e| panic!("{e}\n{:#?}", lines(log)));
}

/// T1: a report re-sent with the same began adopts its step; a new began of the same kind (a
/// `working` between them the sweep missed) is the first box's leave and the next box's open.
#[test]
fn one_step_per_began() {
    let (mut log, _run, mut tr) = run();
    for ms in [200, 300, 400] {
        shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, ms);
    }
    assert_eq!(
        records(&log),
        [
            "open t1 turn par=r1 src=engine conf=claim t=+200",
            "open p1 approval par=t1 box=other src=engine conf=claim t=+200",
        ],
        "one step for the one began"
    );
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 9, 500);
    assert_eq!(
        records(&log)[2..],
        [
            "close p1 approval out=passed dur_ms=300 src=engine conf=claim t=+500",
            "open p2 approval par=t1 box=other src=engine conf=claim t=+500",
        ],
        "another began is another box"
    );
    ordered(&log);
}

/// T2: the leave is attributed as a screen step's close is: by the input bound to it within
/// STEP_ACT_MS (`out=passed`, since neither a person's key nor a ctl input carries a proven
/// act), else `out=passed` with no `by=`, a late act's word kept as `act=`.
#[test]
fn the_leave_closes_by_the_bound_input_else_passed() {
    let leave = |input: &dyn Fn(&mut Log, &mut TurnTracker), leave_ms: u64| -> String {
        let (mut log, _run, mut tr) = run();
        shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, 1_000);
        input(&mut log, &mut tr);
        shown(&mut log, &mut tr, None, 11, leave_ms);
        ordered(&log);
        records(&log)
            .into_iter()
            .find(|l| l.starts_with("close p1 "))
            .expect("the box's close")
    };
    let person = |log: &mut Log, tr: &mut TurnTracker| {
        assert!(matches!(tr.person(log, at(1_800)), Binding::Step(_)));
    };
    let ctl = |log: &mut Log, tr: &mut TurnTracker| {
        let adm = tr.admit(at(1_800));
        let bound = tr.accept(log, Actor::Ctl, Some(ActWord::Approve), adm, at(1_800));
        assert!(matches!(bound, Binding::Step(_)), "{bound:?}");
    };
    let harness = |log: &mut Log, tr: &mut TurnTracker| {
        let adm = tr.admit(at(1_800));
        let bound = tr.accept(log, Actor::Harness, Some(ActWord::Approve), adm, at(1_800));
        assert!(matches!(bound, Binding::Step(_)), "{bound:?}");
    };
    assert_eq!(
        leave(&person, 2_000),
        "close p1 approval out=passed dur_ms=1000 by=human src=engine conf=claim t=+2000"
    );
    assert_eq!(
        leave(&ctl, 2_000),
        "close p1 approval out=passed dur_ms=1000 by=ctl src=engine conf=claim t=+2000",
        "a ctl input's act is unproven"
    );
    assert_eq!(
        leave(&|_: &mut Log, _: &mut TurnTracker| {}, 2_000),
        "close p1 approval out=passed dur_ms=1000 src=engine conf=claim t=+2000",
        "no input dismissed it"
    );
    assert_eq!(
        leave(&ctl, 2_600),
        "close p1 approval out=passed dur_ms=1600 act=approve src=engine conf=claim t=+2600",
        "an act past STEP_ACT_MS is an orphan"
    );
    assert_eq!(
        leave(&harness, 2_000),
        "close p1 approval out=approved dur_ms=1000 by=harness src=engine conf=claim t=+2000",
        "CONTROL: a proven actor's act is the outcome, as on a screen step"
    );
}

/// T3: a screen stall over a standing box takes the screen's slot, never the claim's: the box's
/// leave closes it `out=passed` (no cascade), and the next box opens its own step while the
/// stall's step stays held.
#[test]
fn a_stall_leaves_the_claim_slot_alone() {
    let (mut log, _run, mut tr) = run();
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, 200);
    let _ = tr.observe(&mut log, Verdict::Unresponsive, None, at(300));
    let _ = tr.observe(&mut log, Verdict::Unknown, None, at(400));
    shown(&mut log, &mut tr, None, 11, 500);
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 13, 600);
    assert_eq!(
        records(&log)[1..],
        [
            "open p1 approval par=t1 box=other src=engine conf=claim t=+200",
            "open p2 stall par=t1 src=screen conf=strong t=+300",
            "close p1 approval out=passed dur_ms=300 src=engine conf=claim t=+500",
            "open p3 approval par=t1 box=other src=engine conf=claim t=+600",
        ]
    );
    let p = |n: u32| Node::parse(&format!("p{n}")).expect("a step");
    assert_eq!(
        tr.view(600).step.map(|(n, what, _)| (n, what)),
        Some((p(2), ScreenStep::Stall)),
        "the stall's step is still the screen's"
    );
    assert_eq!(tr.claim_node(), Some(p(3)));
    ordered(&log);
}

/// T4: (a) a rest with a box standing closes the box first, `out=passed` at the turn's time; (b)
/// the program gone closes it `out=gone` at `src=engine conf=claim`; (c) a box under the run (a
/// step with no turn above it, which the log marks `orphan=1`) outlives a transcript turn's close
/// and closes at its own leave.
#[test]
fn a_turn_end_is_the_leave_of_its_standing_box() {
    let (mut log, _run, mut tr) = run();
    shown(&mut log, &mut tr, Some(ClaimedBox::Question), 7, 200);
    rested(&mut log, &mut tr, 11, 900);
    assert_eq!(
        records(&log),
        [
            "open t1 turn par=r1 src=engine conf=claim t=+200",
            "open p1 question par=t1 src=engine conf=claim t=+200",
            "close p1 question out=passed dur_ms=700 src=engine conf=claim t=+900",
            "close t1 turn out=done dur_ms=700 steps=1 qs=1 src=engine conf=claim t=+900",
        ],
        "(a) the box's leave, then its turn"
    );
    ordered(&log);

    let (mut log, _run, mut tr) = run();
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, 200);
    let _ = tr.observe(&mut log, Verdict::Gone, None, at(900));
    let closes: Vec<String> = records(&log)
        .into_iter()
        .filter(|l| l.starts_with("close "))
        .collect();
    assert_eq!(
        closes,
        [
            "close p1 approval out=gone dur_ms=700 src=engine conf=claim t=+900",
            "close t1 turn out=gone dur_ms=700 steps=1 appr=1 src=screen conf=strong t=+900",
        ],
        "(b) the program went while its box stood"
    );
    assert_eq!(tr.claim_node(), None);
    ordered(&log);

    let (mut log, _run, mut tr) = run();
    let fp = 0xc0de_0000_0000_b0c5;
    let pos = |off: u64, ms: u64| TxPos {
        fp,
        id: Some((1, fp)),
        off,
        wall_ms: WALL0 + ms,
    };
    let bind = TxEvent::Bind {
        len: 0,
        floor_ms: WALL0 - 60_000,
    };
    tr.transcript(&mut log, bind, pos(0, 50), TxState::On, at(50));
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, 200);
    let vref = ShortId::new("v1").expect("an id");
    tr.transcript(
        &mut log,
        TxEvent::Prompt { vref },
        pos(200, 300),
        TxState::On,
        at(301),
    );
    let end = TxEvent::TurnEnd {
        vdur_ms: 300,
        tok_in: 1,
        tok_out: 1,
    };
    tr.transcript(&mut log, end, pos(400, 600), TxState::On, at(601));
    assert_eq!(tr.claim_node().map(|n| n.to_string()), Some("p1".into()));
    shown(&mut log, &mut tr, None, 11, 800);
    let mine: Vec<String> = records(&log)
        .into_iter()
        .filter(|l| l.contains(" p1 "))
        .collect();
    assert_eq!(
        mine,
        [
            "open p1 approval par=r1 box=other orphan=1 src=engine conf=claim t=+200",
            "close p1 approval out=passed dur_ms=600 src=engine conf=claim t=+800",
        ],
        "(c) under the run, it outlives the transcript's turn"
    );
    ordered(&log);
}

/// T5: where the screen opens the run's turns the steps are the screen's: a claim opens nothing
/// once the screen opened a turn, and a standing claimed box leaves when the screen opens one.
#[test]
fn a_screen_that_opens_turns_keeps_the_steps() {
    let (mut log, _run, mut tr) = run();
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(100));
    let _ = tr.claim_box(&mut log, report(Some(ClaimedBox::Approval), 7), at(200));
    assert!(
        !records(&log).iter().any(|l| l.contains(" approval ")),
        "{:#?}",
        records(&log)
    );

    let (mut log, _run, mut tr) = run();
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, 200);
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(300));
    let _ = tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("edit")),
        None,
        at(400),
    );
    let _ = tr.claim_box(&mut log, report(Some(ClaimedBox::Approval), 7), at(500));
    assert_eq!(
        records(&log)[1..],
        [
            "open p1 approval par=t1 box=other src=engine conf=claim t=+200",
            "close p1 approval out=passed dur_ms=100 src=engine conf=claim t=+300",
            "open p2 approval par=t1 box=edit src=screen conf=strong t=+400",
        ],
        "one approval step at a time: the screen's"
    );
    ordered(&log);
}

/// T6: inputs bind to the claimed box as to a screen box (admission, acceptance, a person, and
/// the lead), and a Claude screen box's binding is what it was.
#[test]
fn inputs_bind_to_the_claim_step() {
    let (mut log, _run, mut tr) = run();
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, 1_000);
    let p1 = tr.claim_node().expect("the box");
    let adm = tr.admit(at(1_100));
    assert_eq!(adm.step(), Some(p1));
    assert_eq!(
        tr.accept(&log, Actor::Ctl, Some(ActWord::Approve), adm, at(1_100)),
        Binding::Step(p1)
    );
    assert_eq!(tr.person(&log, at(1_200)), Binding::Step(p1));

    let (mut log, _run, mut tr) = run();
    let _ = tr.claim(&mut log, Claimed::Working, at(100));
    let adm = tr.admit(at(700));
    assert_eq!(
        tr.accept(&log, Actor::Ctl, Some(ActWord::Approve), adm, at(700)),
        Binding::Lead
    );
    let _ = tr.claim_box(&mut log, report(Some(ClaimedBox::Approval), 7), at(1_000));
    let p1 = tr.claim_node().expect("the box");
    assert_eq!(
        tr.attribution().bound,
        Some((p1, Actor::Ctl, Some(ActWord::Approve), at(700))),
        "a lead admitted 300 ms before the box binds it"
    );

    let (mut log, _run, mut tr) = run();
    let _ = tr.observe(&mut log, Verdict::Busy, None, at(100));
    let _ = tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("edit")),
        None,
        at(200),
    );
    let p1 = Node::parse("p1").expect("a step");
    assert_eq!(tr.person(&log, at(300)), Binding::Step(p1));
    assert_eq!(
        tr.attribution(),
        AttributionView {
            bound: Some((p1, Actor::Human, None, at(300))),
            lead: None,
            opens: 1,
            closes: 0,
        },
        "a Claude box's binding, as it was"
    );
    assert_eq!(tr.claim_node(), None);
}

/// T8: `brief` lists the claimed boxes as steps, closed and standing, and their turn's counts;
/// the standing box is a claim need, and the session line names no need.
#[test]
fn brief_lists_claimed_boxes() {
    let (mut log, _run, mut tr) = run();
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 7, 1_000);
    shown(&mut log, &mut tr, None, 8, 3_000);
    shown(&mut log, &mut tr, Some(ClaimedBox::Question), 9, 4_000);
    shown(&mut log, &mut tr, None, 10, 11_000);
    rested(&mut log, &mut tr, 11, 12_000);
    shown(&mut log, &mut tr, Some(ClaimedBox::Approval), 12, 14_000);
    ordered(&log);
    let text = log.brief("s-1", 1 << 16, WALL0 + 17_000).text;
    // The header's byte count is the budget's, not this law's: the body is pinned whole.
    let body: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(
        body,
        [
            "- claude r1 17s · -",
            "t2 open 3s · p3 approval other open 3s [engine?]",
            "t1 done 11s · 2 steps · appr 1 · questions 1 [engine?]",
            "p1 approval other passed 2s",
            "p2 question passed 7s",
            "claim need approval:other by=- 3s ago [?]",
            "more=-",
        ],
        "the session line names no need; the standing box is a claim need:\n{text}"
    );
    let st = log.state();
    assert_eq!((st.totals.appr, st.totals.qs), (2, 1), "the fold's totals");
}
