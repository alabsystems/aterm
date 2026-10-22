// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ring's bounds and cursor contract (§5.1): both bounds hold over far
//! more appends than the ring keeps, every eviction is counted exactly, and
//! a reader is told exactly what it lost.

use aterm_slog::{
    Conf, Cursor, Fields, Gap, GapWhy, Key, Kind, Limits, Log, MAX_BYTES, MAX_RECORDS, Mark, Read,
    SharedRead, Src,
};
use aterm_spec::rng::XorShift;

use crate::support::at;

/// 30,000 appends of varying sizes, in six alternating runs of small and
/// large: after every one the ring is within 2048 records AND 256 KiB, `lost`
/// is exactly the records no longer held, and a reader from the start is told
/// that number.
#[test]
fn many_appends_stay_within_both_bounds_and_count_every_eviction() {
    let mut log = Log::new(0);
    let mut rng = XorShift::new(7);
    let mut bytes_bound_hit = false;
    let mut count_bound_hit = false;
    for i in 0..30_000u64 {
        // Mostly small marks, with runs of large ones so the byte bound,
        // not the count, is what evicts for a while.
        let big = (i / 5_000) % 2 == 1;
        let n = if big {
            150 + rng.below(100)
        } else {
            rng.below(8)
        };
        let f = Fields::new()
            .text(Key::Text, &"n".repeat(n as usize))
            .unwrap_or_default();
        // Observations (src=meta), not claims: a claim's share of the ring
        // is its quarter (`tests/mark_quota.rs`), and this is the ring's
        // own bound.
        log.mark(
            Mark::new(Kind::Note, Src::Meta, Conf::Strong).fields(f),
            at(i),
        )
        .expect("a note fits");
        assert!(log.len() <= MAX_RECORDS);
        assert!(log.bytes() <= MAX_BYTES);
        assert_eq!(log.lost(), log.last_seq() - log.len() as u64, "at {i}");
        assert_eq!(log.oldest_seq(), log.lost() + 1);
        bytes_bound_hit |= log.len() < MAX_RECORDS && log.lost() > 0;
        count_bound_hit |= log.len() == MAX_RECORDS;
    }
    assert!(bytes_bound_hit && count_bound_hit, "both bounds evicted");
    let read = log.read(Cursor::new(0, 0), 10);
    let gap = read.gap.expect("a reader from the start is far behind");
    assert_eq!(gap.why, GapWhy::Evicted);
    assert_eq!(gap.lost, log.lost());
    assert_eq!(read.recs.first().map(|r| r.seq), Some(log.oldest_seq()));
}

/// `since=` is exclusive, seqs are dense and increasing, and pulling in any
/// page size yields every live record once.
#[test]
fn cursors_are_exclusive_monotone_and_resumable() {
    let mut log = Log::with_limits(3, Limits::new(64, 1 << 20).expect("limits"));
    for i in 0..40 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(i))
            .expect("fits");
    }
    for page in [1usize, 3, 7, 64] {
        let mut cur = Cursor::new(3, 0);
        let mut seen = Vec::new();
        loop {
            let read = log.read(cur, page);
            assert!(read.gap.is_none());
            if read.recs.is_empty() {
                assert_eq!(read.next, cur, "an empty read holds its place");
                break;
            }
            for r in &read.recs {
                assert!(r.seq > cur.seq, "since= is exclusive");
            }
            seen.extend(read.recs.iter().map(|r| r.seq));
            cur = read.next;
        }
        assert_eq!(seen, (1..=40).collect::<Vec<u64>>(), "page {page}");
    }
    // Repeating a read is safe: the same cursor, the same records.
    let a = log.read(Cursor::new(3, 10), 5);
    let b = log.read(Cursor::new(3, 10), 5);
    assert_eq!(a, b);
}

/// A seamless update from a producer that carried no seq (an older build):
/// the successor's log starts epoch `ep+1` with `gap why=handoff`, seqs
/// restart, and an old cursor is reset rather than read as current.
#[test]
fn a_handoff_resets_old_cursors() {
    let mut old = Log::new(4);
    for i in 0..10 {
        old.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(i))
            .expect("fits");
    }
    let cursor = old.read(Cursor::new(4, 0), 100).next;
    assert_eq!(cursor, Cursor::new(4, 10));

    let mut new = Log::after_handoff(5, Limits::SESSION, old.id_counters(), 1, at(20));
    for i in 0..12 {
        new.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(21 + i))
            .expect("fits");
    }
    // seq 10 exists in the new log, but the cursor is of epoch 4.
    let read = new.read(cursor, 100);
    assert_eq!(read.gap.map(|g| g.why), Some(GapWhy::Reset));
    let first = read.recs.first().expect("the new epoch from its start");
    assert_eq!((first.seq, first.k), (1, Kind::Gap));
    assert_eq!(first.f.get_word(Key::Why), Some("handoff"));
    assert_eq!(first.f.get_u(Key::Epoch), Some(5));
    assert_eq!(read.next, Cursor::new(5, 13));
    assert_eq!(new.state().ep, 5);
    // A cursor ahead of the head is foreign too.
    assert_eq!(
        new.read(Cursor::new(5, 99), 1).gap.map(|g| g.why),
        Some(GapWhy::Reset)
    );
}

/// What a read answered: its gap, its records' seqs, its next cursor.
fn answer(gap: Option<Gap>, seqs: impl Iterator<Item = u64>, next: Cursor) -> String {
    format!("{gap:?} {:?} {next:?}", seqs.collect::<Vec<u64>>())
}

fn borrowed(r: &Read<'_>) -> String {
    answer(r.gap, r.recs.iter().map(|r| r.seq), r.next)
}

fn shared(r: &SharedRead<'_>) -> String {
    answer(r.gap, r.recs.iter().map(|r| r.seq), r.next)
}

/// `read_shared` IS `read` HANDING OUT THE RING'S OWN RECORDS: on an empty
/// ring and on one that evicted, for cursors at the start, behind the ring,
/// on its oldest record, inside it, at its head, ahead of it and of another
/// epoch, at page sizes 0, 1, 3 and unbounded, both reads answer the same
/// gap, records and next cursor, and each shared record is the very record
/// the borrowed read points at. NEGATIVE CONTROL: an evicting append
/// between the two reads makes them differ, so each reads the live ring.
#[test]
fn read_shared_equals_read() {
    let empty = Log::with_limits(2, Limits::new(8, 1 << 20).expect("limits"));
    let mut log = Log::with_limits(2, Limits::new(8, 1 << 20).expect("limits"));
    for i in 0..20 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(i))
            .expect("fits");
    }
    assert_eq!((log.oldest_seq(), log.last_seq()), (13, 20));
    for ring in [&empty, &log] {
        for seq in [0, 5, 11, 12, 13, 16, 19, 20, 21, 99] {
            for ep in [2, 3] {
                for max in [0, 1, 3, usize::MAX] {
                    let cursor = Cursor::new(ep, seq);
                    let (a, b) = (ring.read(cursor, max), ring.read_shared(cursor, max));
                    assert_eq!(borrowed(&a), shared(&b), "{cursor:?} max {max}");
                    assert!(
                        a.recs
                            .iter()
                            .zip(&b.recs)
                            .all(|(x, y)| std::ptr::eq(*x, &***y)),
                        "{cursor:?} max {max}: the ring's own records"
                    );
                }
            }
        }
    }

    // NEGATIVE CONTROL.
    let cursor = Cursor::new(2, 12);
    let before = borrowed(&log.read(cursor, usize::MAX));
    log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(20))
        .expect("fits");
    assert_ne!(before, shared(&log.read_shared(cursor, usize::MAX)));
}

/// An update that carries no records but carries the seq count (design
/// §2.2, `log_seq`): the successor's seqs start past the predecessor's last
/// plus the overlap's reserve, above every seq the predecessor wrote while it
/// went on to its Commit, and a cursor of the old epoch is reset (what it
/// held past its cursor is gone, uncounted). A reader of the new epoch from 0
/// is told no loss: the log accounts for nothing before its first seq.
/// NEGATIVE CONTROL: with nothing carried (`next_seq` 1) the seqs collide.
#[test]
fn an_uncarried_update_continues_seq_and_resets_older_cursors() {
    let mut old = Log::new(4);
    for i in 0..10 {
        old.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(i))
            .expect("fits");
    }
    let reserve = 100;
    let carried = old.last_seq() + reserve;
    // The predecessor goes on writing until its Commit, within the reserve.
    for i in 0..5 {
        old.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(10 + i))
            .expect("fits");
    }
    assert!(old.last_seq() <= carried);
    let cursor = Cursor::new(4, old.last_seq());
    let mut new = Log::after_handoff(5, Limits::SESSION, old.id_counters(), carried + 1, at(20));
    new.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(21))
        .expect("fits");
    let first = new.window().next().expect("the gap");
    assert_eq!((first.seq, first.k), (carried + 1, Kind::Gap));
    assert!(
        new.window().all(|r| r.seq > old.last_seq()),
        "no seq is used twice"
    );
    assert_eq!(new.first_seq(), carried + 1);
    let read = new.read(cursor, 100);
    assert_eq!(read.gap.map(|g| (g.why, g.lost)), Some((GapWhy::Reset, 0)));
    let fresh = new.read(Cursor::new(5, 0), 100);
    assert_eq!(
        fresh.gap, None,
        "nothing before the first seq is this log's loss"
    );
    assert_eq!(fresh.recs.len(), 2);
    assert_eq!(new.complete_from(), carried + 1);

    let bare = Log::after_handoff(5, Limits::SESSION, old.id_counters(), 1, at(20));
    assert_eq!(
        bare.window().next().map(|r| r.seq),
        Some(1),
        "NEGATIVE CONTROL: uncarried, seq 1 again"
    );
}

// ---- level-aware eviction (§5.1, phase 2A) ---------------------------------

use aterm_slog::{
    Actor, Amend, Close, EvictClass, Node, Open, Out, RunOut, SessionOut, StepOut, TurnOut,
    low_water,
};

use crate::support::agent_session;

/// A verdict mark on `run` (what the host writes for every published move).
fn verdict(log: &mut Log, run: Node, word: &str, ms: u64) {
    let f = Fields::new().word(Key::Word, word).expect("a verdict word");
    log.mark(
        Mark::new(Kind::Agent, Src::Screen, Conf::Strong)
            .at(run)
            .fields(f),
        at(ms),
    )
    .expect("a verdict mark");
}

/// THE CRITIC'S CASE: an agent run open for 1000 turns of five verdict marks
/// each (the marks hang from the RUN, not the turn). The marks of a closed
/// turn are history (older than its close), so they go first; the newest
/// turns keep both their open records (with `by=` and `ledger=`) and their
/// closes. NEGATIVE CONTROL: the newest marks, written after the last close,
/// are current and stay too.
#[test]
fn an_agents_verdict_marks_go_before_its_newest_turns() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut ms = 10;
    let mut turns = Vec::new();
    for _ in 0..1000 {
        ms += 10;
        let t = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                    .under(run)
                    .by(Actor::Human)
                    .fields(Fields::new().u(Key::Ledger, ms).expect("a ledger")),
                at(ms),
            )
            .expect("a turn");
        for w in ["busy", "prompt", "busy", "busy", "idle"] {
            ms += 10;
            verdict(&mut log, run, w, ms);
        }
        ms += 10;
        log.close(
            t,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(ms),
        )
        .expect("the turn closes");
        turns.push(t);
    }
    // Two marks after the last close: the present.
    verdict(&mut log, run, "busy", ms + 10);
    verdict(&mut log, run, "idle", ms + 20);
    assert!(log.lost() > 0, "the ring evicted");
    let live = |pred: &dyn Fn(&aterm_slog::Rec) -> bool| log.window().filter(|r| pred(r)).count();
    // Hundreds of the newest turns keep both records: under the run's marks'
    // weight a ring that kept "detail of an open node" would hold marks and
    // no turns.
    let whole = turns
        .iter()
        .rev()
        .take_while(|t| live(&|r| r.node == **t) == 2)
        .count();
    assert!(
        whole >= 500,
        "only the newest {whole} turns kept their open and close"
    );
    assert_eq!(
        log.window()
            .rev()
            .take(2)
            .filter(|r| r.k == Kind::Agent)
            .count(),
        2,
        "NEGATIVE CONTROL: the marks after the last close are current and stay"
    );
    assert!(
        live(&|r| r.k == Kind::Agent) < live(&|r| r.node.lvl == aterm_slog::Lvl::Turn),
        "the closed turns' marks went first"
    );
    // The run's open record is pinned.
    assert!(
        log.window()
            .any(|r| r.node == run && r.op == aterm_slog::Op::Open)
    );
}

/// 10^5 appends of a realistic mix (turns with steps and verdict marks, shell
/// commands, person marks) stay within both bounds, count every eviction,
/// and never evict an open node's open record: the ring turns over some fifty
/// times.
#[test]
fn level_aware_eviction_keeps_the_open_path_and_every_rollup() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut rng = XorShift::new(11);
    let mut ms = 0u64;
    let mut turn: Option<Node> = None;
    let mut step: Option<Node> = None;
    while log.last_seq() < 100_000 {
        ms += 1;
        match rng.below(10) {
            0 if turn.is_none() => {
                turn = log
                    .open(
                        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                        at(ms),
                    )
                    .ok();
            }
            1 if turn.is_some() && step.is_none() => {
                step = log
                    .open(
                        Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                            .under(turn.expect("open"))
                            .fields(Fields::new().word(Key::Box, "bash").expect("box")),
                        at(ms),
                    )
                    .ok();
            }
            2 if step.is_some() => {
                let _ = log.close(
                    step.take().expect("open"),
                    Close::new(Out::Step(StepOut::Approved), Src::Screen, Conf::Strong),
                    at(ms),
                );
            }
            3 if turn.is_some() => {
                step = None;
                let _ = log.close(
                    turn.take().expect("open"),
                    Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                    at(ms),
                );
            }
            4 => {
                let c = log
                    .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
                    .expect("a command");
                let _ = log.close(
                    c,
                    Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                    at(ms),
                );
            }
            5 => {
                let _ = log.mark(
                    Mark::new(Kind::Human, Src::Input, Conf::Strong)
                        .at(Node::SESSION)
                        .by(Actor::Human),
                    at(ms),
                );
            }
            _ => verdict(&mut log, run, "busy", ms),
        }
        assert!(
            log.len() <= MAX_RECORDS && log.bytes() <= MAX_BYTES,
            "at {ms}"
        );
        assert_eq!(log.lost(), log.last_seq() - log.len() as u64, "at {ms}");
        if ms.is_multiple_of(997) {
            for e in log.open_nodes() {
                assert!(
                    log.window()
                        .any(|r| r.node == e.node && r.op == aterm_slog::Op::Open),
                    "{} lost its open record at {ms}",
                    e.node
                );
            }
        }
    }
    // A reader from the start is told exactly what it will never see.
    let read = log.read(Cursor::new(1, 0), usize::MAX);
    assert_eq!(read.gap.map(|g| g.lost), Some(log.lost()));
    assert_eq!(read.recs.len(), log.len());
}

/// THE POLICY ORDER, by what a ring keeps. Closes under half: as a busy
/// open turn's marks push the ring past its bound, the batch takes the closed
/// turn's detail, then the opens of closed commands and turns, then the open
/// turn's OLDEST detail, and every close stays. NEGATIVE CONTROL: with the
/// closes over half the record bound, the batch takes closes before any of
/// the open turn's detail, down to half.
#[test]
fn eviction_takes_classes_in_policy_order() {
    let build = |cmds: u64| {
        let mut log = Log::with_limits(1, Limits::new(64, 1 << 20).expect("limits"));
        let run = agent_session(&mut log, 0);
        for i in 0..cmds {
            let c = log
                .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(1 + i))
                .expect("cmd");
            log.close(
                c,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                at(1 + i),
            )
            .expect("cmd closes");
        }
        let t1 = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(100),
            )
            .expect("t1");
        log.mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(t1),
            at(101),
        )
        .expect("t1 detail");
        log.close(
            t1,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(102),
        )
        .expect("t1 closes");
        let t2 = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(103),
            )
            .expect("t2");
        (log, t1, t2)
    };
    let count = |log: &Log, c: EvictClass| log.window().filter(|r| log.class_of(r) == c).count();
    let closes = |log: &Log| {
        log.window()
            .filter(|r| r.op == aterm_slog::Op::Close && r.node.lvl != aterm_slog::Lvl::Session)
            .count()
    };

    // Ten commands: eleven closes, under half of 64.
    let (mut log, t1, t2) = build(10);
    assert_eq!(log.lost(), 0, "PRECONDITION: all of it fits");
    let first_t2 = log
        .mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(t2),
            at(200),
        )
        .expect("t2 detail");
    for i in 0..200 {
        log.mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(t2),
            at(201 + i),
        )
        .expect("t2 detail");
        if !log.window().any(|r| r.seq == first_t2) {
            break;
        }
    }
    assert!(
        !log.window().any(|r| r.seq == first_t2),
        "PRECONDITION: the open turn's oldest detail went"
    );
    assert!(
        !log.window()
            .any(|r| r.node == t1 && r.op == aterm_slog::Op::Mark),
        "by then the closed turn's detail had gone"
    );
    assert_eq!(
        count(&log, EvictClass::ClosedOpen),
        0,
        "and every closed open"
    );
    assert_eq!(closes(&log), 11, "and every close stayed, under half");
    assert_eq!(
        count(&log, EvictClass::Pinned),
        3,
        "the session, r1 and t2 opens stay"
    );

    // NEGATIVE CONTROL: forty commands, their closes over half of 64.
    let (mut log, _, t2) = build(40);
    assert!(closes(&log) * 2 > 64, "PRECONDITION: closes over half");
    let first = log
        .mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(t2),
            at(300),
        )
        .expect("t2 detail");
    let mut i = 0;
    while closes(&log) * 2 > 64 {
        assert!(
            log.window().any(|r| r.seq == first),
            "the open turn's detail went while the closes were over half"
        );
        log.mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(t2),
            at(301 + i),
        )
        .expect("t2 detail");
        i += 1;
        assert!(i < 500, "the closes never came down to half");
    }
    assert!(
        closes(&log) * 2 >= 62,
        "down to half, no further: {}",
        closes(&log)
    );
}

/// Front eviction alone (marks on the session, nothing else open) reads as before:
/// one gap, `resync_to` the oldest live less one, `oldest=` the
/// oldest live.
#[test]
fn a_contiguous_loss_reads_as_slice_one_did() {
    let mut log = Log::with_limits(1, Limits::new(16, 1 << 20).expect("limits"));
    for i in 0..40 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(i))
            .expect("fits");
    }
    let oldest = log.oldest_seq();
    assert_eq!(log.complete_from(), oldest);
    let read = log.read(Cursor::new(1, 3), usize::MAX);
    let g = read.gap.expect("behind the ring");
    assert_eq!(
        (g.why, g.lost, g.resync_to),
        (GapWhy::Evicted, oldest - 4, oldest - 1)
    );
    assert_eq!(read.recs.first().map(|r| r.seq), Some(oldest));
}

/// Interleaved evictions and update overlaps: a reader is told exactly the
/// records this log held and lost in what it read, the overlaps said as
/// `why=handoff` and never counted. CONTROL: counting the overlaps would
/// change every count the reader is told.
#[test]
fn a_reader_across_interleaved_holes_is_told_exactly_what_it_lost() {
    let mut log = Log::with_limits(1, Limits::new(8, 1 << 20).expect("limits"));
    let run = log
        .open(Open::new(Kind::Prog, Src::Status, Conf::Strong), at(0))
        .expect("a run");
    let mut rng = XorShift::new(5);
    let mut holes_total = 0;
    for i in 0..200 {
        if rng.below(9) == 0 {
            let n = 1 + rng.below(5);
            log.skip_to(log.last_seq() + 1 + n);
            holes_total += n;
        } else {
            log.mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(run),
                at(i),
            )
            .expect("fits");
        }
    }
    assert!(holes_total > 0);
    let held = log.last_seq() - holes_total;
    assert_eq!(
        log.lost(),
        held - log.len() as u64,
        "every loss counted once"
    );
    // Pull a page at a time from the start: the told losses add up exactly.
    for page in [1usize, 2, 3, 100] {
        let (mut cur, mut told, mut got, mut handoffs) = (Cursor::new(1, 0), 0u64, 0u64, 0);
        loop {
            let r = log.read(cur, page);
            if let Some(g) = r.gap {
                told += g.lost;
                handoffs += usize::from(g.why == GapWhy::Handoff);
            }
            got += r.recs.len() as u64;
            if r.next == cur {
                break;
            }
            cur = r.next;
            if cur.seq == log.last_seq() {
                break;
            }
        }
        assert_eq!(told, log.lost(), "page {page}: told exactly");
        assert_eq!(got, log.len() as u64, "page {page}: every live record once");
        assert!(handoffs > 0, "page {page}: the overlaps are said");
        assert_ne!(
            told + holes_total,
            log.lost(),
            "CONTROL: counting the overlaps as lost would tell another number"
        );
    }
    assert_eq!(log.held_between(0, log.last_seq()), held);
}

/// A ring whose open path alone fills it holds the open path and the record
/// just appended, nothing else; a closed node frees its open record to go.
#[test]
fn the_open_path_alone_may_pass_a_tiny_ring_and_nothing_else_does() {
    let mut log = Log::with_limits(1, Limits::new(2, 1 << 20).expect("limits"));
    let run = log
        .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), at(0))
        .expect("run");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    let step = log
        .open(
            Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(turn),
            at(2),
        )
        .expect("step");
    assert_eq!(log.len(), 3, "three pinned opens in a two-record ring");
    for i in 0..20 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(3 + i))
            .expect("fits");
        assert_eq!(log.len(), 4, "the open path and the newest");
        assert_eq!(log.window().last().map(|r| r.seq), Some(log.last_seq()));
    }
    log.close(
        step,
        Close::new(Out::Step(StepOut::Passed), Src::Screen, Conf::Strong),
        at(30),
    )
    .expect("closes");
    assert!(
        log.len() <= 3,
        "the step's records are no longer pinned: {}",
        log.len()
    );
}

/// An evicted record's loss lands in its nearest OPEN node's rollup: a mark
/// on an open turn in the turn's, the closed step's records in the turn's
/// too (the step's rollup was written at its close).
#[test]
fn an_evicted_record_counts_into_its_nearest_open_node() {
    let mut log = Log::with_limits(1, Limits::new(6, 1 << 20).expect("limits"));
    let run = log
        .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), at(0))
        .expect("run");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    let step = log
        .open(
            Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(turn),
            at(2),
        )
        .expect("step");
    log.close(
        step,
        Close::new(Out::Step(StepOut::Passed), Src::Screen, Conf::Strong),
        at(3),
    )
    .expect("step closes");
    for i in 0..10 {
        log.mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(turn),
            at(4 + i),
        )
        .expect("fits");
    }
    let lost = log.lost();
    assert!(lost >= 3);
    let roll = log.open_entry(turn).expect("the turn is open").roll;
    assert_eq!(
        u64::from(roll.lost),
        lost,
        "every loss is under the open turn"
    );
    let seq = log
        .close(
            turn,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(20),
        )
        .expect("the turn closes");
    let close = log.window().find(|r| r.seq == seq).expect("its close");
    assert_eq!(close.f.get_u(Key::Lost), Some(lost), "its close says so");
}

/// The host's budget lowers what one ring holds, level-aware, and never
/// below its open path; raised again, the ring keeps what it had and grows.
/// The floor, `pinned_bytes`, is the bytes of exactly the records no eviction
/// takes: all of a ring of only opens, or of a session's open and close.
#[test]
fn set_byte_budget_evicts_level_aware_and_never_below_the_open_path() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    assert_eq!(
        log.pinned_bytes(),
        log.bytes(),
        "the session's open and the run's: nothing but pinned records"
    );
    let mut closed = Log::new(1);
    closed
        .open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Aterm,
            Fields::new(),
            at(0),
        )
        .expect("the session opens");
    closed
        .close_session(
            Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact),
            at(1),
        )
        .expect("the session closes");
    assert_eq!(
        closed.pinned_bytes(),
        closed.bytes(),
        "the session's open and close: nothing but pinned records"
    );
    for i in 0..500 {
        verdict(&mut log, run, "busy", 10 + i);
    }
    let pinned = log.pinned_bytes();
    assert!(pinned > 0);
    log.set_byte_budget(0);
    assert_eq!(log.budget_bytes(), aterm_slog::MAX_RECORD_BYTES, "clamped");
    assert!(log.bytes() <= pinned + aterm_slog::MAX_RECORD_BYTES);
    assert!(
        log.window()
            .any(|r| r.node == run && r.op == aterm_slog::Op::Open)
    );
    log.set_byte_budget(usize::MAX);
    assert_eq!(log.budget_bytes(), MAX_BYTES);
    let before = log.len();
    verdict(&mut log, run, "idle", 999);
    assert_eq!(log.len(), before + 1, "room again");
}

/// The record just appended is never the one evicted, even when its class is
/// the first to go (an amend of a closed node's step), and the close above it
/// stays while it lives.
#[test]
fn the_newest_record_and_the_close_above_it_stay() {
    let mut log = Log::with_limits(1, Limits::new(3, 1 << 20).expect("limits"));
    let run = log
        .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), at(0))
        .expect("run");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    let close = log
        .close(
            turn,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(2),
        )
        .expect("closes");
    let f = Fields::new().u(Key::TokIn, 7).expect("a count");
    let seq = log
        .amend(
            turn,
            Amend::new(Kind::Turn, Src::Transcript, Conf::Exact, f),
            at(3),
        )
        .expect("an amend of the closed turn");
    assert_eq!(
        log.class_of(log.window().last().expect("newest")),
        EvictClass::ClosedDetail
    );
    assert!(log.window().any(|r| r.seq == seq), "the newest stays");
    assert!(
        log.window().any(|r| r.seq == close),
        "the close above it stays"
    );
}

/// 65 carried updates (one overlap each) with the session's open record
/// pinned at seq 1: past [`aterm_slog::MAX_HOLES`] the log forgets the oldest
/// overlap and accounts only for seqs after it. No count ever runs negative:
/// a reader of an earlier epoch behind that point is reset, and a reader of
/// the current one is told only what the log accounts for.
#[test]
fn sixty_five_overlaps_never_miscount() {
    let mut log = Log::with_limits(1, Limits::new(16, 1 << 20).expect("limits"));
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Nobody,
        Fields::new(),
        at(0),
    )
    .expect("opens");
    for u in 0..65u64 {
        for i in 0..4 {
            log.mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact),
                at(u * 10 + i),
            )
            .expect("fits");
        }
        log.skip_to(log.last_seq() + 100);
    }
    assert_eq!(log.holes().count(), aterm_slog::MAX_HOLES);
    assert!(log.first_seq() > 1, "the oldest overlap is forgotten");
    assert!(
        log.window().any(|r| r.seq == 1),
        "the pinned open lives below it"
    );
    let read = log.read(Cursor::new(1, 0), usize::MAX);
    let g = read.gap.expect("losses and overlaps");
    assert!(g.lost <= log.lost(), "never more than this log lost");
    assert!(
        read.recs.iter().any(|r| r.seq == 1),
        "the pinned open is returned"
    );
    // A cursor of an earlier epoch behind the accounting floor is reset.
    let mut later = log.clone();
    later.skip_to(later.last_seq() + 2);
    let read = log.read(Cursor::new(0, 2), 4);
    assert_eq!(
        read.gap.map(|g| g.why),
        Some(GapWhy::Reset),
        "another epoch's"
    );
    let _ = (later, low_water(Limits::SESSION));
}

/// One step `p` opened and closed under `turn` at `ms`.
fn passed_step(log: &mut Log, turn: Node, ms: u64) -> Node {
    let p = log
        .open(
            Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(turn),
            at(ms),
        )
        .expect("a step opens");
    log.close(
        p,
        Close::new(Out::Step(StepOut::Passed), Src::Screen, Conf::Strong),
        at(ms + 1),
    )
    .expect("the step closes");
    p
}

/// AN AMEND HANGS WHERE ITS NODE DID, OR NOWHERE: a closed step whose
/// records the ring has evicted is put back under its turn (still open) from
/// the last closes the log remembers, so the amend is current detail and its
/// loss counts into the turn; a step closed long ago (65 closes back), its
/// records gone too, is placed by nothing the log still holds, and its amend
/// is refused rather than indexed under an unknown parent (it was classed
/// closed detail, its loss the session's, its turn's close unsheltered).
/// NEGATIVE CONTROL: a never-evicting log, driven alike, still indexes the
/// old step, so the refused amend is the one an evicting log could only
/// have guessed at.
#[test]
fn an_amend_of_a_closed_node_hangs_where_it_did_or_is_refused() {
    let drive = |limits: Limits| -> (Log, Node, Node, Node) {
        let mut log = Log::with_limits(1, limits);
        let run = agent_session(&mut log, 0);
        let turn = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(1),
            )
            .expect("turn");
        let old = passed_step(&mut log, turn, 10);
        for i in 0..65 {
            passed_step(&mut log, turn, 100 + 2 * i);
        }
        let recent = passed_step(&mut log, turn, 1_000);
        // Detail under the open turn pushes the steps' records out.
        for i in 0..40 {
            log.mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(turn),
                at(2_000 + i),
            )
            .expect("fits");
        }
        (log, turn, old, recent)
    };
    let amend = |log: &mut Log, node: Node, ms: u64| {
        let f = Fields::new().u(Key::TokIn, ms).expect("a count");
        log.amend(
            node,
            Amend::new(Kind::Stall, Src::Transcript, Conf::Exact, f),
            at(ms),
        )
    };
    let (mut log, turn, old, recent) = drive(Limits::new(16, 1 << 20).expect("limits"));
    assert!(
        log.node_view(recent).is_none() && log.node_view(old).is_none(),
        "PRECONDITION: both steps' records are gone"
    );
    let seq = amend(&mut log, recent, 3_000).expect("a recent close is placed");
    let rec = log
        .window()
        .find(|r| r.seq == seq)
        .expect("the amend")
        .clone();
    assert_eq!(log.parent_of(recent), Some(turn), "back under its turn");
    assert_eq!(log.class_of(&rec), EvictClass::OpenDetail, "current detail");
    let before = log.open_entry(turn).expect("open").roll.lost;
    let lost_before = log.lost();
    for i in 0..40 {
        log.mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(turn),
            at(3_100 + i),
        )
        .expect("fits");
    }
    assert!(!log.window().any(|r| r.seq == seq), "evicted in turn");
    assert_eq!(
        u64::from(log.open_entry(turn).expect("open").roll.lost - before),
        log.lost() - lost_before,
        "every loss, the amend's with them, counts into the turn"
    );
    assert_eq!(
        amend(&mut log, old, 4_000),
        Err(aterm_slog::Refused::Forgotten(old)),
        "a step the log no longer places is not guessed at"
    );

    // NEGATIVE CONTROL: without eviction the old step is still indexed, and
    // its amend hangs under its turn.
    let (mut whole, turn, old, _) = drive(Limits::new(4096, 1 << 22).expect("limits"));
    assert_eq!(whole.lost(), 0);
    assert_eq!(whole.parent_of(old), Some(turn));
    assert!(amend(&mut whole, old, 4_000).is_ok());
}

/// AN AMEND RE-INDEXES THE WHOLE CLOSED CHAIN IT HANGS FROM: a step closed
/// under a turn that closed too (both still among the last closes the log
/// remembers, the run above them open), every record of both evicted. The
/// amend puts the step back under its turn AND the turn back under its run,
/// so when the amend is evicted in turn its loss counts into the run, the
/// nearest open ancestor, and not into the session's rollup (a re-index of
/// the step alone leaves its turn unknown, which ends the walk at the
/// session). Every loss of the drive counts into the run or the open turn.
/// NEGATIVE CONTROL: a never-evicting log, driven alike, still indexes the
/// step under the turn under the run, the chain the amend rebuilt.
#[test]
fn an_amend_of_a_closed_node_reindexes_its_closed_ancestors_too() {
    let drive = |limits: Limits| -> (Log, Node, Node, Node, Node) {
        let mut log = Log::with_limits(1, limits);
        let run = agent_session(&mut log, 0);
        let first = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(1),
            )
            .expect("turn");
        let step = passed_step(&mut log, first, 2);
        log.close(
            first,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(4),
        )
        .expect("the turn closes");
        // 30 more closed turns: the step's and its turn's closes stay among
        // the last 64 closes.
        for i in 0..30 {
            let t = log
                .open(
                    Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                    at(10 + 2 * i),
                )
                .expect("turn");
            log.close(
                t,
                Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                at(11 + 2 * i),
            )
            .expect("it closes");
        }
        let now = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(500),
            )
            .expect("the open turn");
        // Detail under the open turn pushes the old records out.
        for i in 0..40 {
            log.mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(now),
                at(1_000 + i),
            )
            .expect("fits");
        }
        (log, run, first, step, now)
    };
    let (mut log, run, first, step, now) = drive(Limits::new(16, 1 << 20).expect("limits"));
    assert!(
        log.node_view(step).is_none() && log.node_view(first).is_none(),
        "PRECONDITION: the step's and its turn's records are gone"
    );
    let lost = |log: &Log, n: Node| log.open_entry(n).expect("open").roll.lost;
    let (run_before, now_before, lost_before) = (lost(&log, run), lost(&log, now), log.lost());
    let f = Fields::new().u(Key::TokIn, 7).expect("a count");
    let seq = log
        .amend(
            step,
            Amend::new(Kind::Stall, Src::Transcript, Conf::Exact, f),
            at(2_000),
        )
        .expect("a recent close is placed");
    assert_eq!(log.parent_of(step), Some(first), "back under its turn");
    assert!(log.node_view(first).is_some(), "the turn is indexed again");
    assert_eq!(log.parent_of(first), Some(run), "back under its run");
    for i in 0..40 {
        log.mark(
            Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(now),
            at(2_100 + i),
        )
        .expect("fits");
    }
    assert!(!log.window().any(|r| r.seq == seq), "the amend is evicted");
    let to_run = lost(&log, run) - run_before;
    let to_now = lost(&log, now) - now_before;
    assert!(to_run >= 1, "the amend's loss counts into the run");
    assert_eq!(
        u64::from(to_run + to_now),
        log.lost() - lost_before,
        "every loss counts into an open node, none into the session"
    );

    // NEGATIVE CONTROL: without eviction the chain is still indexed.
    let (whole, run, first, step, _) = drive(Limits::new(4096, 1 << 22).expect("limits"));
    assert_eq!(whole.lost(), 0);
    assert!(whole.node_view(step).is_some() && whole.node_view(first).is_some());
    assert_eq!(whole.parent_of(step), Some(first));
    assert_eq!(whole.parent_of(first), Some(run));
}

/// AN OPEN UNDER A CLOSED NODE HANGS THE SAME WHATEVER WAS EVICTED: `under=`
/// a turn closed long ago (its run closed too) resolves its nearest open
/// ancestor from the open path and the last closes alone (rule),
/// never from the node index, which keeps the old turn only while a record
/// of it lives. Two logs driven alike, one evicting and one not, write the
/// same open. NEGATIVE CONTROL: their indexes differ about the old turn
/// (the never-evicting one still walks it up to the session, the evicting
/// one cannot), so a rule that read the index placed the step apart: under
/// the session in one, under the open step in the other.
#[test]
fn an_open_under_a_closed_node_hangs_the_same_whatever_was_evicted() {
    let drive = |limits: Limits| -> (Log, Node) {
        let mut log = Log::with_limits(1, limits);
        let old_run = agent_session(&mut log, 0);
        let first = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(old_run),
                at(1),
            )
            .expect("turn");
        log.close(
            old_run,
            Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
            at(2),
        )
        .expect("the run and its turn close");
        // 65 commands after them: their closes push both out of the last
        // closes the log remembers.
        for i in 0..65 {
            let r = log
                .open(
                    Open::new(Kind::Cmd, Src::Shell, Conf::Strong),
                    at(10 + 2 * i),
                )
                .expect("a command");
            log.close(
                r,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Strong),
                at(11 + 2 * i),
            )
            .expect("it ends");
        }
        // Another agent, mid-turn, a step open.
        let run = log
            .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), at(500))
            .expect("run");
        let turn = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(501),
            )
            .expect("turn");
        log.open(
            Open::new(Kind::Approval, Src::Screen, Conf::Strong).under(turn),
            at(502),
        )
        .expect("a step");
        log.open(
            Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(first),
            at(1_000),
        )
        .expect("a step asked under the old turn");
        (log, first)
    };
    let (small, first) = drive(Limits::new(32, 1 << 20).expect("limits"));
    let (big, _) = drive(Limits::new(4096, 1 << 22).expect("limits"));
    let open = |log: &Log| {
        let r = log.window().last().expect("the step's open");
        aterm_slog::encode(r, aterm_slog::Tier::Content).expect("fits")
    };
    assert_eq!(open(&small), open(&big), "the same parent either way");
    // NEGATIVE CONTROL: the logs disagree about the old turn's index entry.
    assert!(small.node_view(first).is_none(), "evicted, forgotten");
    assert!(big.node_view(first).is_some(), "still indexed");
}
