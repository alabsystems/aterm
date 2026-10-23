// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that each record retains its monotonic process-clock reading in RAM without encoding it.

use aterm_slog::{
    Actor, Amend, CarryAdopt, CarryBudget, Close, Conf, Fields, GapWhy, JournalImage, Key, Kind,
    Limits, Log, Mark, Node, Open, Out, Rec, RunOut, SessionOut, Src, Stamp, Tier, TurnOut, encode,
};

use crate::support::WALL0;

const MONO0: u64 = 7_777_000;

fn stamp(i: u64, shift: u64) -> Stamp {
    Stamp::new(WALL0 + 1_000 * i, MONO0 + shift + 37 * i * i + 3 * i)
}

fn opened(log: &mut Log, shift: u64) -> Node {
    let s = |i| stamp(i, shift);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        s(1),
    )
    .expect("the session opens");
    let run = log
        .open(
            Open::new(Kind::Cmd, Src::Shell, Conf::Strong)
                .fields(Fields::new().u(Key::Blk, 4).expect("a block id")),
            s(2),
        )
        .expect("a run opens");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            s(3),
        )
        .expect("a turn opens");
    log.open(
        Open::new(Kind::Tool, Src::Transcript, Conf::Exact).under(turn),
        s(4),
    )
    .expect("a step opens");
    log.mark(
        Mark::new(Kind::Bell, Src::Engine, Conf::Exact)
            .at(Node::SESSION)
            .fields(Fields::new().u(Key::N, 2).expect("a count")),
        s(5),
    )
    .expect("a mark");
    log.amend(
        turn,
        Amend::new(
            Kind::Turn,
            Src::Transcript,
            Conf::Exact,
            Fields::new().u(Key::TokIn, 9).expect("a count"),
        ),
        s(6),
    )
    .expect("an amend");
    turn
}

fn script(log: &mut Log, shift: u64) -> Vec<Stamp> {
    let s = |i| stamp(i, shift);
    let turn = opened(log, shift);
    let run = log.parent_of(turn).expect("the turn's run");
    log.close(
        turn,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        s(7),
    )
    .expect("the turn closes");
    let _ = log.gap(GapWhy::Evicted, 3, s(8));
    log.close(
        run,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Strong),
        s(9),
    )
    .expect("the run closes");
    log.close_session(
        Close::new(Out::Session(SessionOut::AppQuit), Src::Aterm, Conf::Exact).by(Actor::Aterm),
        s(10),
    )
    .expect("the session closes");
    [1, 2, 3, 4, 5, 6, 7, 7, 8, 9, 10].map(s).to_vec()
}

const HOST_LINES: [&str; 11] = [
    "1 open - spawn by=aterm src=aterm conf=exact t=1760000001000",
    "2 open r1 cmd par=- blk=4 src=shell conf=strong t=1760000002000",
    "3 open t1 turn par=r1 src=screen conf=strong t=1760000003000",
    "4 open p1 tool par=t1 src=transcript conf=exact t=1760000004000",
    "5 mark - bell n=2 src=engine conf=exact t=1760000005000",
    "6 amend t1 turn tok_in=9 src=transcript conf=exact t=1760000006000",
    "7 close p1 tool out=gone dur_ms=1230 src=screen conf=strong t=1760000007000",
    "8 close t1 turn out=done dur_ms=1492 steps=1 src=screen conf=strong t=1760000007000",
    "9 gap - gap why=evicted lost=3 by=aterm src=aterm conf=exact t=1760000008000",
    "10 close r1 cmd out=ok dur_ms=2870 steps=1 src=shell conf=strong t=1760000009000",
    "11 close - spawn out=app-quit dur_ms=3690 steps=1 by=aterm src=aterm conf=exact t=1760000010000",
];

#[test]
fn each_op_keeps_its_stamps_mono_and_no_tier_encodes_it() {
    let mut log = Log::new(3);
    let stamps = script(&mut log, 0);
    let kept: Vec<(u64, u64)> = log.window_mono().map(|(r, m)| (r.seq, m)).collect();
    let want: Vec<(u64, u64)> = stamps
        .iter()
        .zip(1..)
        .map(|(s, seq)| (seq, s.mono_ms))
        .collect();
    assert_eq!(kept, want, "each record its own append's mono");
    for (seq, mono) in &want {
        assert_eq!(log.mono_of(*seq), Some(*mono), "mono_of({seq})");
    }
    assert_eq!(log.mono_of(99), None, "no record, no reading");
    let host: Vec<String> = log
        .window()
        .map(|r| encode(r, Tier::Host).expect("fits"))
        .collect();
    assert_eq!(host, HOST_LINES, "the host tier as committed");

    let mut later = Log::new(3);
    let _ = script(&mut later, 1_800_000);
    assert!(
        later
            .window_mono()
            .zip(log.window_mono())
            .all(|((_, b), (_, a))| b == a + 1_800_000),
        "the later clock's readings"
    );
    for tier in [Tier::Wire, Tier::WireMeta, Tier::Host, Tier::Content] {
        let of = |l: &Log| -> Vec<String> {
            l.window().map(|r| encode(r, tier).expect("fits")).collect()
        };
        let (a, b) = (of(&log), of(&later));
        assert_eq!(a, b, "{tier:?}: a process clock changes no byte");
        for line in &a {
            assert!(!line.contains("777"), "{tier:?}: a clock reading in {line}");
        }
    }
}

#[test]
fn a_record_from_another_process_reads_its_wall_on_this_clock_or_nothing() {
    let mut log = Log::new(3);
    let _ = opened(&mut log, 0);
    let stamps: Vec<Stamp> = (1..=6).map(|i| stamp(i, 0)).collect();
    let out = log
        .carry(CarryBudget::SESSION, &[])
        .expect("the carry fits");
    for line in &out.lines {
        assert!(!line.contains("777"), "the carry names a clock: {line}");
    }
    let now = Stamp::new(WALL0 + 30_000, 50_000_000);
    let (back, _, _) = Log::from_carry(
        &out.lines,
        CarryAdopt {
            ep: 4,
            next_seq: log.last_seq() + 100,
            ids: log.id_counters(),
            limits: Limits::SESSION,
            at: now,
        },
    )
    .expect("the carry reads");
    let mut crossed = 0;
    for (r, mono) in back.window_mono() {
        assert_eq!(mono, 50_000_000 - (now.wall_ms - r.t), "seq {}", r.seq);
        if let Some(s) = usize::try_from(r.seq - 1).ok().and_then(|i| stamps.get(i)) {
            assert_ne!(mono, s.mono_ms, "seq {}: the old clock's reading", r.seq);
            crossed += 1;
        }
    }
    assert!(crossed >= 4, "the open path crossed: {crossed}");

    let mut log = Log::new(3);
    let _ = script(&mut log, 0);
    let recs: Vec<Rec> = log.window().cloned().collect();
    let journal = Log::from_journal(&JournalImage {
        ep: 3,
        base: 3,
        first: 1,
        head: log.last_seq(),
        recs,
        holes: Vec::new(),
    })
    .expect("the journal image reads");
    assert_eq!(journal.len(), log.len());
    assert!(journal.window_mono().all(|(_, m)| m == 0), "journal: none");

    let summary = Log::from_closed_summary(&log.closed_summary()).expect("the summary reads");
    assert!(!summary.is_empty(), "the summary keeps records");
    assert!(summary.window_mono().all(|(_, m)| m == 0), "summary: none");
}
