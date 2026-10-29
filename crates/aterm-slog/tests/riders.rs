// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AMEND THAT RIDES ITS OPEN (fix unit H4, finding GATE3-7): an amend that writes what its
//! node is lives and dies with the node's open, in a batch, a carry's cut and a journal snap.

use std::collections::BTreeSet;

use aterm_slog::journal::Liveness;
use aterm_slog::{
    Actor, Amend, CarryBudget, Close, Conf, EvictClass, Fields, Key, Kind, Limits, Log, Mark, Node,
    Op, Open, Out, Rec, RunOut, Src, TurnOut, low_water, row_floor,
};

use crate::journal_view::{Writer, view};
use crate::support::{agent_session, at};

fn session(log: &mut Log) {
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("the session opens");
}

/// The session a restore's new shell stands in for, named after its open (`note_restored`).
fn restored(log: &mut Log) -> u64 {
    let f = Fields::new()
        .id(Key::Prev, "s-7a1b2c")
        .and_then(|f| f.id(Key::Lineage, "s-0d0e0f"))
        .expect("a restore's ids");
    log.amend(
        Node::SESSION,
        Amend::new(Kind::Restore, Src::Aterm, Conf::Exact, f),
        at(1),
    )
    .expect("the session amends")
}

/// A `cmd` run opened without `detail=`, named by an amend once its program is confirmed (G1).
fn named_cmd(log: &mut Log, fields: Fields, ms: u64) -> (Node, u64) {
    let node = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
        .expect("the cmd opens");
    let seq = log
        .amend(
            node,
            Amend::new(Kind::Cmd, Src::Status, Conf::Strong, fields),
            at(ms + 1),
        )
        .expect("the detail amends");
    (node, seq)
}

fn detail(name: &str) -> Fields {
    Fields::new().tok(Key::Detail, name).expect("a detail")
}

fn end(log: &mut Log, node: Node, ms: u64) {
    log.close(
        node,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
        at(ms),
    )
    .expect("the command ends");
}

fn bell(log: &mut Log, ms: u64) -> u64 {
    log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(ms))
        .expect("a mark")
}

fn held(log: &Log, node: Node, op: Op) -> bool {
    log.window().any(|r| r.node == node && r.op == op)
}

fn adopt(log: &Log, ms: u64) -> aterm_slog::CarryAdopt {
    aterm_slog::CarryAdopt {
        ep: log.ep() + 1,
        next_seq: log.last_seq() + 1000,
        ids: log.id_counters(),
        limits: Limits::SESSION,
        at: at(ms),
    }
}

/// The nodes in `nodes` whose open `log` holds and differ from the ones whose amend it holds.
fn unpaired(log: &Log, nodes: &[Node]) -> Vec<Node> {
    nodes
        .iter()
        .copied()
        .filter(|&n| held(log, n, Op::Open) != held(log, n, Op::Amend))
        .collect()
}

/// The fold keeps a run's `model=` by the confidence law (an exact transcript's over a later,
/// strong screen's): the amend it keeps is the one that rides, never merely the newest.
#[test]
fn the_amend_the_fold_keeps_rides_not_the_newest() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let model = |m: &str| Fields::new().tok(Key::Model, m).expect("a model");
    let exact = log
        .amend(
            run,
            Amend::new(Kind::Agent, Src::Transcript, Conf::Exact, model("opus")),
            at(5),
        )
        .expect("the transcript names the model");
    let strong = log
        .amend(
            run,
            Amend::new(Kind::Agent, Src::Screen, Conf::Strong, model("sonnet")),
            at(6),
        )
        .expect("the card names another");
    assert_eq!(
        log.state().run().and_then(|r| r.f.str(Key::Model)),
        Some("opus"),
        "PRECONDITION: the fold keeps the exact model"
    );
    let class = |log: &Log, seq: u64| log.window().find(|r| r.seq == seq).map(|r| log.class_of(r));
    assert_eq!(class(&log, exact), Some(EvictClass::Pinned));
    assert_eq!(class(&log, strong), Some(EvictClass::OpenDetail));
    let mut ms = 10;
    while log.lost() < 4_000 {
        ms += 10;
        let t = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(ms),
            )
            .expect("a turn");
        bell(&mut log, ms + 1);
        log.close(
            t,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(ms + 2),
        )
        .expect("the turn closes");
    }
    let models: Vec<Option<&str>> = log
        .window()
        .filter(|r| r.node == run && r.op == Op::Amend)
        .map(|r| r.f.get_str(Key::Model))
        .collect();
    assert_eq!(
        models,
        [Some("opus")],
        "the ring holds the model the fold shows, and the refused one went as detail"
    );
    assert_eq!(
        log.state().run().and_then(|r| r.f.str(Key::Model)),
        Some("opus")
    );
}

/// A closed command named after its close, its amend the newest record when the ring evicts: the
/// newest record stays, and so does the open it rides; a carry at a tight budget carries both.
#[test]
fn a_command_named_after_its_close_keeps_its_name_while_it_is_the_newest() {
    let mut log = Log::new(1);
    session(&mut log);
    let c = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(10))
        .expect("the cmd opens");
    end(&mut log, c, 11);
    log.open(Open::new(Kind::Turn, Src::Screen, Conf::Strong), at(12))
        .expect("an open turn");
    let mut ms = 20;
    while log.len() < aterm_slog::MAX_RECORDS {
        ms += 1;
        bell(&mut log, ms);
    }
    assert_eq!(log.lost(), 0, "PRECONDITION: nothing evicted yet");
    let seq = log
        .amend(
            c,
            Amend::new(Kind::Cmd, Src::Status, Conf::Strong, detail("make")),
            at(ms + 1),
        )
        .expect("named late");
    assert!(log.lost() > 0, "PRECONDITION: the amend's append evicted");
    let newest = log.window().last().expect("the newest record");
    assert_eq!(newest.seq, seq, "the newest record stays");
    assert!(held(&log, c, Op::Open), "and the open it rides");
    assert_eq!(log.class_of(newest), EvictClass::ClosedOpen);

    let tight = CarryBudget {
        records: 8,
        ..CarryBudget::SESSION
    };
    let cut = log.carry(tight, &[]).expect("carried");
    assert!(cut.shed > 0, "PRECONDITION: the cut shed");
    let (new, _, _) = Log::from_carry(&cut.lines, adopt(&log, 99_000)).expect("reads");
    let crossed = new
        .window()
        .find(|r| r.seq == seq)
        .expect("the newest record crosses");
    assert_eq!(crossed.f.get_str(Key::Detail), Some("make"));
    assert!(held(&new, c, Op::Open), "with the open it rides");
}

/// The journal holds the open of a closed command and the marks after it, but not the amend
/// that names it: a batch spends journaled records first and keeps the unwritten amend, with
/// its open.
#[test]
fn an_amend_the_journal_has_not_written_waits_with_its_open() {
    let mut log = Log::new(1);
    session(&mut log);
    let c = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(10))
        .expect("the cmd opens");
    end(&mut log, c, 11);
    log.open(Open::new(Kind::Turn, Src::Screen, Conf::Strong), at(12))
        .expect("an open turn");
    let mut ms = 20;
    while log.len() < 1_000 {
        ms += 1;
        bell(&mut log, ms);
    }
    let rider = log
        .amend(
            c,
            Amend::new(Kind::Cmd, Src::Status, Conf::Strong, detail("make")),
            at(ms + 1),
        )
        .expect("named late");
    log.note_journaled(rider - 1);
    while log.lost() == 0 {
        ms += 1;
        bell(&mut log, ms);
    }
    assert!(
        log.window().next().is_some_and(|r| r.seq < rider),
        "PRECONDITION: journaled records are still held"
    );
    assert!(
        held(&log, c, Op::Amend) && held(&log, c, Op::Open),
        "the unwritten amend and its open stay while journaled records go first"
    );
}

/// A batch counts an open and the amend that rides it as the records and bytes they are: it
/// stops at the low-water mark within one group, by records and by bytes.
#[test]
fn a_batch_counts_a_rider_and_its_open_by_records_and_bytes() {
    let wide = || {
        detail("make-with-a-long-name-of-forty-eight-characters")
            .tok(Key::Prog, "a-program-named-at-length-to-weigh-forty-eight")
            .and_then(|f| f.tok(Key::Model, "a-model-of-thirty-two-characters"))
            .expect("identity fields")
    };
    for (limits, fields) in [
        (
            Limits::new(64, 1 << 20).expect("a records ring"),
            detail("make"),
        ),
        (Limits::new(10_000, 8_192).expect("a bytes ring"), wide()),
    ] {
        let low = low_water(limits);
        let mut log = Log::with_limits(1, limits);
        session(&mut log);
        let (mut ms, mut cmds) = (10, Vec::new());
        let mut group_bytes = 0usize;
        while log.lost() < 2_000 {
            ms += 10;
            let before = log.bytes();
            let (c, _) = named_cmd(&mut log, fields.clone(), ms);
            if log.bytes() > before {
                group_bytes = group_bytes.max(log.bytes() - before);
            }
            end(&mut log, c, ms + 2);
            cmds.push(c);
            assert_eq!(unpaired(&log, &cmds), [], "{limits:?} at {ms}");
            if log.lost() > 0 {
                assert!(
                    log.len() + 1 >= low.records || log.bytes() + group_bytes > low.bytes,
                    "{limits:?} at {ms}: a batch stops within one group of low water: \
                     {} records, {} bytes, low {low:?}, a group {group_bytes} bytes",
                    log.len(),
                    log.bytes(),
                );
            }
        }
    }
}

/// An open run pins the amend that rides it; once the run closes, the amend is no more pinned
/// than its open.
#[test]
fn pinned_bytes_counts_a_rider_only_while_its_run_is_open() {
    let mut named = Log::new(1);
    let mut bare = Log::new(1);
    let mut nodes = Vec::new();
    for log in [&mut named, &mut bare] {
        session(log);
        nodes.push(
            log.open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(10))
                .expect("the cmd opens"),
        );
    }
    let before = named.bytes();
    named
        .amend(
            nodes[0],
            Amend::new(Kind::Cmd, Src::Status, Conf::Strong, detail("make")),
            at(11),
        )
        .expect("named");
    let rider = named.bytes() - before;
    assert_eq!(named.pinned_bytes(), bare.pinned_bytes() + rider, "pinned");
    end(&mut named, nodes[0], 12);
    end(&mut bare, nodes[1], 12);
    assert_eq!(
        named.pinned_bytes(),
        bare.pinned_bytes(),
        "a closed run pins neither its open nor its rider"
    );
}

/// A row-floor rider (a rule that keeps a command's naming amend as a row) holds its open back
/// with it: the floor's rows outlast the records outside it.
#[test]
fn a_floor_rider_keeps_its_open_out_of_the_first_pass() {
    fn rule(r: &Rec) -> u8 {
        u8::from(r.op == Op::Amend && r.k == Kind::Cmd)
    }
    let limits = Limits::new(64, 1 << 20).expect("a small ring");
    let q = row_floor(limits);
    let mut log = Log::with_limits(1, limits);
    log.keep_rows(rule, 0);
    session(&mut log);
    let mut riders = Vec::new();
    for i in 0..20u64 {
        let (c, seq) = named_cmd(&mut log, detail("make"), 10 + 10 * i);
        end(&mut log, c, 12 + 10 * i);
        riders.push((c, seq));
    }
    let floor = &riders[riders.len() - q.records..];
    let mut ms = 1_000;
    for _ in 0..200 {
        ms += 1;
        bell(&mut log, ms);
        for &(c, seq) in floor {
            assert!(
                log.window().any(|r| r.seq == seq) && held(&log, c, Op::Open),
                "at {ms}: the floor's row {seq} and its open stay while marks outside it go"
            );
        }
    }
    assert!(log.lost() > 100, "PRECONDITION: the marks evicted");
}

/// A carry cut by its byte budget sheds a rider with its open, never one without the other.
#[test]
fn a_carry_cut_by_bytes_sheds_a_rider_with_its_open() {
    let mut log = Log::new(1);
    session(&mut log);
    let mut ms = 10;
    let mut cmds = Vec::new();
    for _ in 0..120 {
        ms += 10;
        let (c, _) = named_cmd(&mut log, detail("make"), ms);
        bell(&mut log, ms + 2);
        end(&mut log, c, ms + 3);
        cmds.push(c);
    }
    let all = log.carry(CarryBudget::SESSION, &[]).expect("carried");
    assert_eq!(all.shed, 0, "PRECONDITION: room for everything");
    let (mut some, mut none) = (false, false);
    for total in (all.skeleton_bytes..all.bytes).step_by(11) {
        let tight = CarryBudget {
            total,
            ..CarryBudget::SESSION
        };
        let Some(cut) = log.carry(tight, &[]) else {
            continue;
        };
        let (new, _, _) = Log::from_carry(&cut.lines, adopt(&log, 99_000)).expect("reads");
        assert_eq!(unpaired(&new, &cmds), [], "a cut at {total} bytes");
        some |= cmds.iter().any(|&c| held(&new, c, Op::Open));
        none |= cmds.iter().any(|&c| !held(&new, c, Op::Open));
    }
    assert!(
        some && none,
        "PRECONDITION: the cuts kept some commands and shed others"
    );
}

/// A journal reader that starts in a later segment reads the open path its snap restates: each
/// open with the amends that name it, the session's restore among them.
#[test]
fn a_snap_restates_an_open_with_the_amends_that_name_it() {
    let mut log = Log::new(1);
    session(&mut log);
    let restore = restored(&mut log);
    let (run, seq) = named_cmd(&mut log, detail("claude"), 10);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    w.rotate();
    for i in 0..5 {
        bell(&mut log, 20 + i);
    }
    w.pass(&log, 512);
    let mut chain = w.read(Liveness::Live);
    chain.segments.remove(0);
    let v = view(std::slice::from_ref(&chain));
    let seqs: BTreeSet<u64> = v.log.window().map(|r| r.seq).collect();
    assert!(v.log.is_open(run), "the restated run is open");
    assert!(
        seqs.contains(&seq) && seqs.contains(&restore),
        "the snap restates the amends that name the run and the session: {seqs:?}"
    );
    assert_eq!(
        v.log.state().run().and_then(|r| r.f.str(Key::Detail)),
        Some("claude"),
        "the reader's fold names the run"
    );
}

/// A later snap counts the opens it restates, never the amends that ride them: a command whose
/// close the writer never saw is still read closed `out=unknown` beside a restated rider.
#[test]
fn a_later_snap_counts_its_opens_not_the_amends_that_ride_them() {
    let limits = Limits::new(4, 1 << 20).expect("a tiny ring");
    let mut log = Log::with_limits(1, limits);
    session(&mut log);
    restored(&mut log);
    let r = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(2))
        .expect("a run opens");
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    end(&mut log, r, 3);
    for i in 0..6 {
        log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(4 + i))
            .expect("a mark");
    }
    assert!(!held(&log, r, Op::Close), "PRECONDITION: evicted unwritten");
    w.pass(&log, 512);
    let v = view(&[w.read(Liveness::Live)]);
    assert!(!v.log.is_open(r), "the later snap does not list it");
    assert_eq!(v.unknown_closed, 1, "it reads closed out=unknown");
}

/// A restored session's `prev=`/`lineage=` amend rides the session's open: no flood of closed
/// runs beneath it evicts it, and a tight carry carries it.
#[test]
fn a_restored_session_keeps_what_it_was_restored_from() {
    let mut log = Log::new(1);
    session(&mut log);
    let restore = restored(&mut log);
    let mut ms = 10;
    while log.lost() < 3_000 {
        ms += 10;
        let (c, _) = named_cmd(&mut log, detail("make"), ms);
        end(&mut log, c, ms + 2);
    }
    let amend = log
        .window()
        .find(|r| r.seq == restore)
        .expect("the restore amend stays");
    assert_eq!(log.class_of(amend), EvictClass::Pinned);
    let tight = CarryBudget {
        records: 8,
        ..CarryBudget::SESSION
    };
    let cut = log.carry(tight, &[]).expect("carried");
    let (new, _, _) = Log::from_carry(&cut.lines, adopt(&log, 99_000)).expect("reads");
    let crossed = new
        .window()
        .find(|r| r.seq == restore)
        .expect("the restore amend crosses");
    assert_eq!(crossed.f.get_str(Key::Prev), Some("s-7a1b2c"));
}
