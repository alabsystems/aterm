// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AMEND THAT RIDES ITS OPEN (fix unit H4, finding GATE3-7; a turn's, fix unit J0-2): an
//! amend that writes what its node is (a run's program or model, a turn's `vref=` or `cont=`, a
//! restored session's lineage) lives and dies with the node's open, in a batch, a carry's cut and
//! a journal snap.

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

/// A turn opened under `run`.
fn open_turn(log: &mut Log, run: Node, ms: u64) -> Node {
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
        at(ms),
    )
    .expect("a turn opens")
}

fn close_turn(log: &mut Log, turn: Node, ms: u64) {
    log.close(
        turn,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        at(ms),
    )
    .expect("the turn closes");
}

/// A turn's identity amend as `TurnTracker` writes it (fix unit J0-2): the transcript names the
/// vendor turn (`vref=`, `amend_vref`) or the earlier turn this one continues (`cont=`,
/// `ensure_turn_as`).
fn name_turn(log: &mut Log, turn: Node, key: Key, id: &str, ms: u64) -> u64 {
    let f = Fields::new().id(key, id).expect("an id");
    log.amend(
        turn,
        Amend::new(Kind::Turn, Src::Transcript, Conf::Strong, f).by(Actor::Vendor),
        at(ms),
    )
    .expect("the turn amends")
}

/// The id a turn's `key` names: a vendor message's (`vref=`, one width so every amend weighs the
/// same), or the turn before it (`cont=`).
fn turn_id(key: Key, before: Node, i: u64) -> String {
    match key {
        Key::Cont => before.to_string(),
        _ => format!("msg-{i:06}"),
    }
}

/// Whether `log` holds an amend of `node` that writes `key`.
fn named(log: &Log, node: Node, key: Key) -> bool {
    log.window()
        .any(|r| r.node == node && r.op == Op::Amend && r.f.get(key).is_some())
}

/// The turns in `turns` of which `log` holds the open or the `key` amend but not both.
fn unnamed_turns(log: &Log, turns: &[Node], key: Key) -> Vec<Node> {
    turns
        .iter()
        .copied()
        .filter(|&t| held(log, t, Op::Open) != named(log, t, key))
        .collect()
}

/// A turn named by `key`, held open while its own usage amends flood the ring: at every step the
/// ring holds its open and the amend that names it.
fn an_open_turns_name_stays_with_its_open(key: Key) {
    let mut log = Log::with_limits(1, Limits::new(64, 1 << 20).expect("a small ring"));
    let run = agent_session(&mut log, 0);
    let before = open_turn(&mut log, run, 1);
    close_turn(&mut log, before, 2);
    let open = open_turn(&mut log, run, 3);
    name_turn(&mut log, open, key, &turn_id(key, before, 0), 4);
    let mut ms = 10;
    while log.lost() < 500 {
        ms += 1;
        let f = Fields::new().u(Key::TokIn, ms).expect("a count");
        log.amend(
            open,
            Amend::new(Kind::Turn, Src::Transcript, Conf::Exact, f),
            at(ms),
        )
        .expect("usage amends");
        assert!(
            held(&log, open, Op::Open) && named(&log, open, key),
            "{key:?} at {ms}: an open turn keeps its open and the amend that names it"
        );
    }
}

/// Closed turns, each named by `key` and with a mark beneath it, until the ring evicts their
/// opens: at every step each turn's open is held iff its identity amend is.
fn a_closed_turns_name_goes_with_its_open(key: Key) {
    let mut log = Log::with_limits(1, Limits::new(64, 1 << 20).expect("a small ring"));
    let run = agent_session(&mut log, 0);
    let mut prev = open_turn(&mut log, run, 1);
    close_turn(&mut log, prev, 2);
    let mut ms = 10;
    let mut turns = Vec::new();
    let (mut some, mut none) = (false, false);
    for i in 0..400 {
        ms += 10;
        let t = open_turn(&mut log, run, ms);
        name_turn(&mut log, t, key, &turn_id(key, prev, i), ms + 1);
        bell(&mut log, ms + 2);
        close_turn(&mut log, t, ms + 3);
        turns.push(t);
        prev = t;
        assert_eq!(unnamed_turns(&log, &turns, key), [], "{key:?} at {ms}");
        some |= turns.iter().any(|&t| held(&log, t, Op::Open));
        none |= turns.iter().any(|&t| !held(&log, t, Op::Open));
    }
    assert!(
        some && none,
        "PRECONDITION: the ring evicted some closed turns' opens and kept others"
    );
}

/// A log of 120 closed turns and an open one, each named by `key`: the log, the closed turns, the
/// open turn and the seq of the amend that names it.
fn named_turns(key: Key) -> (Log, Vec<Node>, Node, u64) {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut prev = open_turn(&mut log, run, 1);
    close_turn(&mut log, prev, 2);
    let mut ms = 10;
    let mut turns = Vec::new();
    for i in 0..120 {
        ms += 10;
        let t = open_turn(&mut log, run, ms);
        name_turn(&mut log, t, key, &turn_id(key, prev, i), ms + 1);
        bell(&mut log, ms + 2);
        close_turn(&mut log, t, ms + 3);
        turns.push(t);
        prev = t;
    }
    let open = open_turn(&mut log, run, ms + 10);
    let seq = name_turn(&mut log, open, key, &turn_id(key, prev, 999), ms + 11);
    bell(&mut log, ms + 12);
    (log, turns, open, seq)
}

/// A carry cut by its byte budget sheds a closed turn's identity amend with its open, never
/// alone, and carries the open turn's with it.
fn a_tight_carry_keeps_a_turns_name_with_its_open(key: Key) {
    let (log, turns, open, seq) = named_turns(key);
    let all = log.carry(CarryBudget::SESSION, &[]).expect("carried");
    assert_eq!(all.shed, 0, "PRECONDITION: room for everything");
    let (mut some, mut none) = (false, false);
    for total in (all.skeleton_bytes..all.bytes).step_by(17) {
        let tight = CarryBudget {
            total,
            ..CarryBudget::SESSION
        };
        let Some(cut) = log.carry(tight, &[]) else {
            continue;
        };
        let (new, _, _) = Log::from_carry(&cut.lines, adopt(&log, 99_000)).expect("reads");
        assert_eq!(
            unnamed_turns(&new, &turns, key),
            [],
            "{key:?}: a cut at {total} bytes"
        );
        assert!(
            new.is_open(open) && new.window().any(|r| r.seq == seq),
            "{key:?}: a cut at {total} bytes carries the open turn with its name"
        );
        some |= turns.iter().any(|&t| held(&new, t, Op::Open));
        none |= turns.iter().any(|&t| !held(&new, t, Op::Open));
    }
    assert!(
        some && none,
        "PRECONDITION: the cuts kept some closed turns and shed others"
    );
}

/// A journal snap restates the open turn with the amend that names it, right after its open, and
/// a reader that starts in the later segment holds it and names the turn as the writer's fold does.
fn a_later_snap_restates_a_turn_with_its_name(key: Key) {
    let (mut log, _, open, seq) = named_turns(key);
    let restated: Vec<(Node, Op, u64)> = log
        .open_path_records()
        .iter()
        .map(|r| (r.node, r.op, r.seq))
        .collect();
    let at_open = restated
        .iter()
        .position(|&(n, op, _)| n == open && op == Op::Open)
        .expect("the snap restates the open turn");
    assert_eq!(
        restated.get(at_open + 1).map(|&(.., s)| s),
        Some(seq),
        "{key:?}: the amend that names the turn follows its open: {restated:?}"
    );
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    w.rotate();
    for i in 0..5 {
        bell(&mut log, 5_000 + i);
    }
    w.pass(&log, 512);
    let mut chain = w.read(Liveness::Live);
    chain.segments.remove(0);
    let v = view(std::slice::from_ref(&chain));
    assert!(v.log.is_open(open), "the restated turn is open");
    assert!(
        v.log.window().any(|r| r.seq == seq),
        "{key:?}: a reader of the later segment holds the amend that names the turn"
    );
    assert_eq!(
        v.log.state().turn().and_then(|t| t.f.str(key)),
        log.state().turn().and_then(|t| t.f.str(key)),
        "{key:?}: the reader's fold names the turn as the writer's does"
    );
}

/// A turn's `vref=` rides its open (fix unit J0-2): an open turn pins it while detail under the
/// turn floods the ring.
#[test]
fn an_open_turns_vref_stays_with_its_open() {
    an_open_turns_name_stays_with_its_open(Key::Vref);
}

/// A closed turn's `vref=` goes with its open: at every step of a flood of closed turns, the ring
/// holds a turn's open iff it holds the amend that names its vendor turn.
#[test]
fn a_closed_turns_vref_goes_with_its_open() {
    a_closed_turns_name_goes_with_its_open(Key::Vref);
}

/// A tight carry sheds a closed turn's `vref=` only with its open, and carries the open turn's.
#[test]
fn a_tight_carry_keeps_a_turns_vref_with_its_open() {
    a_tight_carry_keeps_a_turns_name_with_its_open(Key::Vref);
}

/// A later journal snap restates an open turn with its `vref=`.
#[test]
fn a_later_snap_restates_a_turn_with_its_vref() {
    a_later_snap_restates_a_turn_with_its_name(Key::Vref);
}

/// A turn's `cont=` (the earlier turn it continues) rides its open as `vref=` does: an open turn
/// pins it.
#[test]
fn an_open_turns_cont_stays_with_its_open() {
    an_open_turns_name_stays_with_its_open(Key::Cont);
}

/// A closed turn's `cont=` goes with its open through a flood of closed turns.
#[test]
fn a_closed_turns_cont_goes_with_its_open() {
    a_closed_turns_name_goes_with_its_open(Key::Cont);
}

/// A tight carry sheds a closed turn's `cont=` only with its open, and carries the open turn's.
#[test]
fn a_tight_carry_keeps_a_turns_cont_with_its_open() {
    a_tight_carry_keeps_a_turns_name_with_its_open(Key::Cont);
}

/// A later journal snap restates an open turn with its `cont=`.
#[test]
fn a_later_snap_restates_a_turn_with_its_cont() {
    a_later_snap_restates_a_turn_with_its_name(Key::Cont);
}

/// THE BOUND: a turn named more than once at one confidence (the late-match path can name a
/// closed turn again) has one rider per identity key, the amend the fold keeps (the newest): it
/// pins no more than one while the turn is open, rides the closed turn's open after, and every
/// older one is detail, evicted before the open and its rider.
#[test]
fn a_turn_named_twice_pins_only_the_amend_the_fold_keeps() {
    let mut log = Log::with_limits(1, Limits::new(64, 1 << 20).expect("a small ring"));
    let run = agent_session(&mut log, 0);
    let t = open_turn(&mut log, run, 10);
    let bare = log.pinned_bytes();
    let first = name_turn(&mut log, t, Key::Vref, "msg-000001", 11);
    let one = log.pinned_bytes() - bare;
    assert!(one > 0, "PRECONDITION: the first name rides the open turn");
    let second = name_turn(&mut log, t, Key::Vref, "msg-000002", 12);
    assert_eq!(
        log.pinned_bytes(),
        bare + one,
        "an open turn named twice pins one rider"
    );
    let class = |log: &Log, seq: u64| log.window().find(|r| r.seq == seq).map(|r| log.class_of(r));
    assert_eq!(class(&log, first), Some(EvictClass::OpenDetail));
    assert_eq!(class(&log, second), Some(EvictClass::Pinned));
    close_turn(&mut log, t, 13);
    let third = name_turn(&mut log, t, Key::Vref, "msg-000003", 14);
    let kept = name_turn(&mut log, t, Key::Vref, "msg-000004", 15);
    assert_eq!(
        log.state().last[1]
            .as_ref()
            .and_then(|c| c.f.str(Key::Vref)),
        Some("msg-000004"),
        "PRECONDITION: the fold keeps the newest name"
    );
    assert_eq!(class(&log, kept), Some(EvictClass::ClosedOpen));
    for older in [first, second, third] {
        assert_eq!(
            class(&log, older),
            Some(EvictClass::ClosedDetail),
            "the name at {older} is detail"
        );
    }
    let mut ms = 20;
    let mut detail_went_first = false;
    while held(&log, t, Op::Open) {
        ms += 10;
        let u = open_turn(&mut log, run, ms);
        bell(&mut log, ms + 1);
        close_turn(&mut log, u, ms + 2);
        let holds = |seq: u64| log.window().any(|r| r.seq == seq);
        assert_eq!(
            held(&log, t, Op::Open),
            holds(kept),
            "at {ms}: the open and the name the fold keeps go together"
        );
        detail_went_first |=
            held(&log, t, Op::Open) && ![first, second, third].iter().any(|&s| holds(s));
        assert!(ms < 1_000_000, "the flood never evicted the turn's open");
    }
    assert!(
        detail_went_first,
        "the older names went as detail while the open and its rider stayed"
    );
}
