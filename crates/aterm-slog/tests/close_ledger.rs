// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A turn's close restates its `ledger=` (plan 5F-4a, ruling T12): the log
//! copies it from the turn's open record, pinned while the turn is open, so
//! a close that outlives its evicted open still names the exchange that
//! opened the turn, and `drive ledger --source=log` joins it with no clock.

use aterm_slog::{
    Actor, Close, Conf, Fields, Key, Kind, Limits, Log, Node, Op, Open, Out, Rec, RunOut, Src,
    StepOut, Tier, TurnOut, TurnTracker, Verdict, encode,
};

use crate::support::{agent_session, at};

fn close_of(log: &Log, node: Node) -> Option<Rec> {
    log.window()
        .find(|r| r.node == node && r.op == Op::Close)
        .cloned()
}

fn turn(log: &mut Log, run: Node, ledger: Option<u64>, ms: u64) -> Node {
    let f = match ledger {
        Some(l) => Fields::new().u(Key::Ledger, l).expect("a ledger"),
        None => Fields::new(),
    };
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong)
            .under(run)
            .by(Actor::Ctl)
            .fields(f),
        at(ms),
    )
    .expect("a turn")
}

fn done() -> Close {
    Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong)
}

/// Every way a turn closes restates its open's `ledger=`: the tracker's
/// settle after a `ctl turn` exchange, a direct close, and a cascade that
/// closes it `out=gone`. A caller's own `ledger=` is kept. NEGATIVE
/// CONTROLS: a turn opened without one, a step and a run close name none —
/// the close copies, it never invents.
#[test]
fn every_turn_close_restates_its_opens_ledger() {
    // The tracker: a `ctl turn` exchange's turn, settled.
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.ctl_turn(&mut log, Actor::Ctl, 7, at(100));
    tr.observe(&mut log, Verdict::Busy, None, at(200));
    tr.observe(&mut log, Verdict::Idle, None, at(300));
    tr.tick(&mut log, at(60_000));
    let t1 = log
        .window()
        .find(|r| r.op == Op::Open && r.k == Kind::Turn)
        .map(|r| r.node)
        .expect("the turn");
    let line = encode(&close_of(&log, t1).expect("closed"), Tier::Host).expect("fits");
    assert!(line.contains(" ledger=7 "), "{line}");

    // Direct closes, a caller's own, and a cascade.
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let a = turn(&mut log, run, Some(11), 10);
    log.close(a, done(), at(20)).expect("closes");
    let b = turn(&mut log, run, None, 30);
    log.close(b, done(), at(40)).expect("closes");
    let c = turn(&mut log, run, Some(13), 50);
    log.close(
        c,
        done().fields(Fields::new().u(Key::Ledger, 99).expect("a ledger")),
        at(60),
    )
    .expect("closes");
    let d = turn(&mut log, run, Some(14), 70);
    let step = log
        .open(
            Open::new(Kind::Tool, Src::Screen, Conf::Strong).under(d),
            at(75),
        )
        .expect("a step");
    log.close(
        step,
        Close::new(Out::Step(StepOut::Ok), Src::Screen, Conf::Strong),
        at(78),
    )
    .expect("the step closes");
    log.close(
        run,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
        at(80),
    )
    .expect("the run closes, the turn with it");
    let ledger = |n: Node| close_of(&log, n).and_then(|r| r.f.get_u(Key::Ledger));
    assert_eq!(ledger(a), Some(11), "a direct close");
    assert_eq!(ledger(b), None, "NEGATIVE: opened without one");
    assert_eq!(ledger(c), Some(99), "the caller's own is kept");
    assert_eq!(ledger(d), Some(14), "a cascade's out=gone close");
    assert_eq!(
        close_of(&log, d).and_then(|r| r.f.get_word(Key::Out).map(str::to_string)),
        Some("gone".to_string())
    );
    assert_eq!(ledger(step), None, "NEGATIVE: a step's close");
    assert_eq!(ledger(run), None, "NEGATIVE: a run's close");
}

/// THE CLOSE OUTLIVES ITS OPEN: in a ring that evicts closed turns' opens
/// first (§5.1's `ClosedOpen` before `CloseRecord`), turns whose open is gone
/// still name their exchange on the close; every live close names its own.
#[test]
fn a_close_whose_open_was_evicted_still_names_its_exchange() {
    let mut log = Log::with_limits(1, Limits::new(24, 1 << 20).expect("a ring"));
    let run = agent_session(&mut log, 0);
    let mut turns = Vec::new();
    for i in 0..20u64 {
        let t = turn(&mut log, run, Some(100 + i), 10 + i * 10);
        log.close(t, done(), at(15 + i * 10)).expect("closes");
        turns.push((t, 100 + i));
    }
    assert!(log.lost() > 0, "the ring evicted");
    let live_open = |n: Node| log.window().any(|r| r.node == n && r.op == Op::Open);
    let mut orphaned = 0;
    for (t, want) in &turns {
        let Some(close) = close_of(&log, *t) else {
            continue;
        };
        assert_eq!(close.f.get_u(Key::Ledger), Some(*want), "{t:?}");
        if !live_open(*t) {
            orphaned += 1;
        }
    }
    assert!(orphaned > 0, "some closes outlived their opens");
}
