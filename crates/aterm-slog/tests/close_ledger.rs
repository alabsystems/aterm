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

/// A turn opened by `by` (screen, no ledger) under `run`, at `ms`.
fn turn_by(log: &mut Log, run: Node, by: Actor, ms: u64) -> Node {
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong)
            .under(run)
            .by(by),
        at(ms),
    )
    .expect("a turn")
}

/// A TURN'S CLOSE RESTATES WHO OPENED IT (Phase 6 step 6C-2, ruling U6):
/// `op_by=<opener>` on the close of a turn the person, `ctl`, the harness
/// or an edge opened, whoever closes it (the screen `by=-`, the transcript
/// `by=vendor`, a cascade), as a host-tier id. NEGATIVE CONTROLS, each
/// naming none: a turn nobody opened that the transcript closes `by=vendor`
/// (the closer is never taken for the opener), a step's and a run's close.
/// A caller never writes it, on a close or on any other record.
#[test]
fn a_turn_close_restates_who_opened_it() {
    let edge = Actor::sid(aterm_slog::ShortId::new("s-edge42").expect("a sid"));
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let vendor = || done().by(Actor::Vendor);
    let mut cases = Vec::new();
    for (i, (by, close)) in [
        (Actor::Human, done()),
        (Actor::Ctl, done()),
        (Actor::Harness, vendor()),
        (edge, done().by(Actor::Ctl)),
        (Actor::Nobody, vendor()),
    ]
    .into_iter()
    .enumerate()
    {
        let ms = 10 + 10 * i as u64;
        let t = turn_by(&mut log, run, by, ms);
        log.close(t, close, at(ms + 5)).expect("closes");
        cases.push((t, by));
    }
    let opener = |log: &Log, n: Node| {
        close_of(log, n).and_then(|r| match r.f.get(Key::OpBy) {
            Some(aterm_slog::Val::Id(id)) => Some(id.as_str().to_string()),
            _ => None,
        })
    };
    for (t, by) in &cases {
        let want = (*by != Actor::Nobody).then(|| by.word().to_string());
        assert_eq!(opener(&log, *t), want, "{t} opened by {}", by.word());
    }
    let edge_close = close_of(&log, cases[3].0).expect("the edge's turn closed");
    let host = encode(&edge_close, Tier::Host).expect("fits");
    assert!(host.contains(" op_by=s-edge42 by=ctl "), "{host}");
    for tier in [Tier::Wire, Tier::WireMeta] {
        let line = encode(&edge_close, tier).expect("fits");
        assert!(
            !line.contains("op_by=") && !line.contains("s-edge42"),
            "{line}"
        );
    }
    // A cascade's `out=gone` close, and the levels that name none.
    let t = turn_by(&mut log, run, Actor::Harness, 100);
    let step = log
        .open(
            Open::new(Kind::Tool, Src::Screen, Conf::Strong)
                .under(t)
                .by(Actor::Harness),
            at(105),
        )
        .expect("a step");
    log.close(
        run,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
        at(110),
    )
    .expect("the run closes, the turn and step with it");
    assert_eq!(
        opener(&log, t).as_deref(),
        Some("harness"),
        "a cascade's close"
    );
    assert_eq!(opener(&log, step), None, "NEGATIVE: a step's close");
    assert_eq!(opener(&log, run), None, "NEGATIVE: a run's close");
    // A caller's `op_by=` is refused on every record it could ride.
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let planted = || {
        Fields::new()
            .with(
                Key::OpBy,
                aterm_slog::Val::Id(aterm_slog::ShortId::new("harness").expect("an id")),
            )
            .expect("an id field")
    };
    let owned = |r: Result<u64, aterm_slog::Refused>| {
        matches!(
            r,
            Err(aterm_slog::Refused::Field(aterm_slog::FieldError::Shape(
                Key::OpBy
            )))
        )
    };
    let t = turn_by(&mut log, run, Actor::Ctl, 10);
    assert!(
        owned(log.close(t, done().fields(planted()), at(20))),
        "a close"
    );
    let opened = log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong)
            .under(run)
            .fields(planted()),
        at(21),
    );
    assert!(
        matches!(
            opened,
            Err(aterm_slog::Refused::Field(aterm_slog::FieldError::Shape(
                Key::OpBy
            )))
        ),
        "an open: {opened:?}"
    );
    assert!(
        owned(log.mark(
            aterm_slog::Mark::new(Kind::Note, Src::Mark, Conf::Claim).fields(planted()),
            at(22)
        )),
        "a mark"
    );
    assert!(
        owned(log.amend(
            t,
            aterm_slog::Amend::new(Kind::Turn, Src::Transcript, Conf::Exact, planted()),
            at(23)
        )),
        "an amend"
    );
    log.close(t, done(), at(30)).expect("the turn closes");
    assert_eq!(opener(&log, t).as_deref(), Some("ctl"), "the log's own");
}

/// A CLOSE AT THE SIZE CAP STILL FITS (6C-2): the opener is the last thing
/// the close adds, so the size guard drops it first. Over every length of
/// a caller's text on a harness turn's close (`ledger=7` on its open), each
/// close the log takes fits the record cap; some keep `ledger=` but not
/// `op_by=`, and none keeps `op_by=` without `ledger=`. A close whose field
/// slots are full goes without both.
#[test]
fn a_close_at_the_size_cap_still_fits_without_its_opener() {
    let mut dropped_first = 0;
    let mut both = 0;
    for n in 1..=160usize {
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        let t = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                    .under(run)
                    .by(Actor::Harness)
                    .fields(Fields::new().u(Key::Ledger, 7).expect("a ledger")),
                at(10),
            )
            .expect("a turn");
        let f = Fields::new()
            .text(Key::Cwd, &"c".repeat(256))
            .and_then(|f| f.text(Key::Text, &"x".repeat(n)))
            .expect("host text");
        if log.close(t, done().fields(f), at(20)).is_err() {
            continue;
        }
        let close = close_of(&log, t).expect("closed");
        let line = encode(&close, Tier::Content).expect("a taken close encodes");
        assert!(line.len() <= aterm_slog::MAX_RECORD_BYTES, "{n}: {line}");
        match (
            close.f.get(Key::Ledger).is_some(),
            close.f.get(Key::OpBy).is_some(),
        ) {
            (true, false) => dropped_first += 1,
            (true, true) => both += 1,
            (false, true) => panic!("{n}: the opener outlived the ledger: {line}"),
            (false, false) => {}
        }
    }
    assert!(
        dropped_first > 0,
        "no close at the cap dropped its opener first"
    );
    assert!(both > 0, "PRECONDITION: a short close keeps both");

    // Every field slot taken: the close goes without its restatements.
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let t = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                .under(run)
                .by(Actor::Harness)
                .fields(Fields::new().u(Key::Ledger, 7).expect("a ledger")),
            at(10),
        )
        .expect("a turn");
    let mut f = Fields::new();
    for key in [
        Key::Code,
        Key::Blk,
        Key::Rev,
        Key::N,
        Key::Pct,
        Key::After,
        Key::Epoch,
        Key::Carried,
        Key::Shed,
        Key::Ttl,
    ] {
        let _ = f
            .set(key, aterm_slog::Val::U(1))
            .or_else(|_| f.set(key, aterm_slog::Val::I(1)));
    }
    log.close(t, done().fields(f), at(20))
        .expect("a full close is taken");
    let close = close_of(&log, t).expect("closed");
    assert_eq!(close.f.len(), aterm_slog::MAX_FIELDS, "every slot taken");
    assert!(close.f.get(Key::OpBy).is_none(), "no slot for the opener");
}
