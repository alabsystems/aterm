// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal reader's view (design §5.3, §5.4, phase 2B), over chains a
//! simulated writer wrote with the host writer's own policy
//! ([`journal::take`], [`journal::take_frames`]) and read back through the
//! codec:
//! - DIFFERENTIAL: seeded traffic on a small ring, journaled at random
//!   intervals (so the writer meets evictions): the view's records past the
//!   first loss, its loss count and its open set equal the live log's;
//! - each `(sid, base, seq)` once, however many writers journaled it;
//! - a dead chain's view closes what was open, innermost first, after `gap
//!   why=crash after=<head>`; an ended chain's and a live one's add nothing;
//! - a close the writer never saw reads `out=unknown` at the later snap;
//! - pruned and unread prefixes say so; an update's shortfall reads
//!   `why=handoff` with its count;
//! - ONE TAKE PER PASS (the host's own pace, a writer behind its log): no
//!   range is said pruned when nothing was, a crash gap says the writer's
//!   cursor, each seq once, every loss at or below the cursor counted;
//! - a reader holds every record it read: 10⁵ records in, none said lost;
//! - the ring evicts what the writer holds before what it has not reached;
//! - the records a reply derives are numbered past the journal's head in
//!   the journal's epoch and derived again by every read: a reply hands out
//!   the journal's cursor, so a reader following one reply into the next
//!   never skips a record the journal holds, and a cursor the journal never
//!   numbered (another space's, a successor's the journal lags) is reset,
//!   said;
//! - a restatement past its snap's head accounts for nothing.

use aterm_slog::journal::{
    ChainRead, EndWhy, FORMAT, Frame, Inst, Liveness, SegmentRead, View, ViewRules, complete_lines,
    decode_line, encode_frame, take, take_frames, view_of,
};
use aterm_slog::{
    Actor, CarryAdopt, CarryBudget, Close, Conf, Cursor, Fields, GapWhy, IdCounters, Key, Kind,
    Limits, Log, Lvl, MAX_BYTES, MAX_RECORDS, Mark, Node, Op, Open, Out, Rec, RunOut, ShortId, Src,
    TurnOut,
};
use aterm_spec::rng::XorShift;

use crate::support::at;

pub(crate) fn sid() -> ShortId {
    ShortId::new("s-9f3c0a").expect("a sid")
}

/// A simulated journal writer for one instance: its segments as bytes, a
/// cursor per session, and the snap each session owes per segment.
pub(crate) struct Writer {
    pub(crate) inst: Inst,
    pub(crate) segs: Vec<Vec<u8>>,
    pub(crate) cursor: u64,
    pub(crate) snapped: bool,
    pub(crate) t: u64,
}

impl Writer {
    pub(crate) fn new(inst: &str, start: u64) -> Writer {
        let mut w = Writer {
            inst: Inst::new(inst).expect("an instance"),
            segs: Vec::new(),
            cursor: 0,
            snapped: false,
            t: start,
        };
        w.rotate();
        w
    }

    pub(crate) fn line(&mut self, f: &Frame) {
        let line = encode_frame(f).expect("encodes");
        let seg = self.segs.last_mut().expect("a segment");
        seg.extend_from_slice(line.as_bytes());
        seg.push(b'\n');
    }

    pub(crate) fn rotate(&mut self) {
        self.segs.push(Vec::new());
        let n = self.segs.len() as u32;
        self.t += 1;
        self.line(&Frame::Seg {
            v: FORMAT,
            inst: self.inst.clone(),
            n,
            t: self.t,
            build: "test".to_string(),
        });
        self.snapped = false;
    }

    /// One take of at most `max` records, as the host's pass does for one
    /// track (the host takes once per pass and owes the rest), telling the
    /// log how far it read when `mark`. `present`: the writer before phase
    /// 2B's fix, whose snaps restated the open path as the log stood (a
    /// negative control).
    pub(crate) fn pass_once(&mut self, log: &mut Log, max: usize, mark: bool, present: bool) {
        let snap = !self.snapped;
        let tk = take(log, self.cursor, max, snap);
        if mark {
            log.note_journaled(tk.next);
        }
        self.t += 1;
        let frames = if present {
            present_frames(&tk, snap, self.t)
        } else {
            take_frames(sid(), &tk, snap, self.t)
        };
        for f in frames {
            self.line(&f);
        }
        self.snapped = true;
        self.cursor = tk.next;
    }

    /// One writer pass over `log`: what the host's pass does for one track.
    pub(crate) fn pass(&mut self, log: &Log, max: usize) {
        loop {
            let snap = !self.snapped;
            let tk = take(log, self.cursor, max, snap);
            self.t += 1;
            for f in take_frames(sid(), &tk, snap, self.t) {
                self.line(&f);
            }
            self.snapped = true;
            self.cursor = tk.next;
            if !tk.more {
                return;
            }
        }
    }

    pub(crate) fn fin(&mut self, why: EndWhy) {
        self.t += 1;
        let t = self.t;
        self.line(&Frame::End { why, t });
    }

    pub(crate) fn read(&self, live: Liveness) -> ChainRead {
        let segments: Vec<SegmentRead> = self
            .segs
            .iter()
            .enumerate()
            .map(|(i, bytes)| SegmentRead {
                n: i as u32 + 1,
                frames: complete_lines(bytes)
                    .filter_map(|l| decode_line(l).ok())
                    .collect(),
            })
            .collect();
        let ended = segments
            .last()
            .is_some_and(|s| s.frames.iter().any(|f| matches!(f, Frame::End { .. })));
        ChainRead {
            inst: self.inst.clone(),
            segments,
            ended,
            live,
            start_t: self.segs.first().map_or(0, |_| 1),
        }
    }
}

/// A take's lines as the writer before phase 2B's fix wrote them: each snap
/// restating the open path as the log stood at the take, past the snap's
/// head (NEGATIVE CONTROL).
fn present_frames(tk: &aterm_slog::journal::Take, snap: bool, t: u64) -> Vec<Frame> {
    let mut out = take_frames(sid(), tk, false, t);
    let restate = |head: u64| {
        let opens = tk.opens.clone().unwrap_or_default();
        let mut f = vec![Frame::Snap {
            sid: sid(),
            head,
            ep: tk.ep,
            base: tk.base,
            first: tk.first,
            open: opens.len() as u32,
            t,
        }];
        f.extend(opens.into_iter().map(|rec| Frame::Rec {
            sid: sid(),
            rec,
            restated: true,
        }));
        f
    };
    // take_frames without a leading snap ends with the post-loss snap (as of
    // its head); swap it for the present one.
    if let Some(i) = out.iter().rposition(|f| matches!(f, Frame::Snap { .. })) {
        out.truncate(i);
        out.extend(restate(tk.next));
    }
    if snap {
        let mut lead = restate(tk.cursor);
        lead.extend(out);
        out = lead;
    }
    out
}

pub(crate) fn view(chains: &[ChainRead]) -> View {
    view_of(&sid(), chains, false, ViewRules::READER).expect("the session is in the journal")
}

/// Every record a full read of `log` returns.
pub(crate) fn all(log: &Log) -> Vec<Rec> {
    log.read(Cursor::new(log.ep(), 0), usize::MAX)
        .recs
        .into_iter()
        .cloned()
        .collect()
}

fn host(r: &Rec) -> Rec {
    aterm_slog::decode(&aterm_slog::encode(r, aterm_slog::Tier::Host).expect("fits"))
        .expect("decodes")
}

fn open_session(log: &mut Log, ms: u64) {
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Nobody,
        Fields::new(),
        at(ms),
    )
    .expect("the session opens");
}

fn cmd(log: &mut Log, ms: u64) -> Node {
    log.open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
        .expect("a run opens")
}

/// Random traffic: commands that open and close, marks, and turns under an
/// agent run, on `log`.
fn traffic(log: &mut Log, rng: &mut XorShift, ms: &mut u64) {
    *ms += 7;
    match rng.below(6) {
        0 => {
            let _ = cmd(log, *ms);
        }
        1 => {
            if let Some(n) = log.deepest_open(Lvl::Run) {
                let _ = log.close(
                    n,
                    Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                    at(*ms),
                );
            }
        }
        2 => {
            if let Some(r) = log.deepest_open(Lvl::Run) {
                let _ = log.open(
                    Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(r),
                    at(*ms),
                );
            }
        }
        3 => {
            if let Some(t) = log.deepest_open(Lvl::Turn) {
                let _ = log.close(
                    t,
                    Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                    at(*ms),
                );
            }
        }
        _ => {
            let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(*ms));
        }
    }
}

#[test]
fn the_view_of_a_journaled_log_is_the_live_log() {
    for seed in 1..=40u64 {
        let mut rng = XorShift::new(seed);
        let limits = Limits::new(24, 1 << 20).expect("a small ring");
        let mut log = Log::with_limits(1, limits);
        open_session(&mut log, 0);
        let mut w = Writer::new("p1-a1", 0);
        let mut ms = 0;
        for step in 0..400 {
            traffic(&mut log, &mut rng, &mut ms);
            if rng.below(9) == 0 {
                w.pass(&log, 1 + rng.below(16) as usize);
            }
            if step % 150 == 149 {
                w.rotate();
            }
        }
        w.pass(&log, 512);
        let v = view(&[w.read(Liveness::Live)]);
        assert!(!v.crashed, "a live chain is never crashed");
        // The records the view holds are exactly the live log's that the
        // journal holds, and a full read of the view is told every loss.
        let live: Vec<Rec> = all(&log).iter().map(host).collect();
        let shown = all(&v.log);
        let from = shown.first().map_or(0, |r| r.seq);
        for r in &live {
            if r.seq >= from && v.unknown_closed == 0 {
                assert!(shown.contains(r), "seed {seed}: {r:?} is not in the view");
            }
        }
        let mut seqs: Vec<u64> = shown.iter().map(|r| r.seq).collect();
        seqs.dedup();
        assert_eq!(seqs.len(), shown.len(), "seed {seed}: a seq twice");
        let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
        let missing = (1..=log.last_seq())
            .filter(|s| !shown.iter().any(|r| r.seq == *s))
            .count() as u64;
        assert_eq!(
            read.gap.map_or(0, |g| g.lost),
            missing,
            "seed {seed}: the view's loss is the journal's"
        );
        // The open set is the live log's (a close the ring evicted before the
        // writer reached it reads `out=unknown`, never open).
        let open = |l: &Log| l.open_nodes().iter().map(|e| e.node).collect::<Vec<_>>();
        assert_eq!(open(&v.log), open(&log), "seed {seed}: the open set");
        assert_eq!(v.log.is_open(Node::SESSION), log.is_open(Node::SESSION));
    }
}

#[test]
fn each_seq_is_shown_once_however_often_it_was_journaled() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let r = cmd(&mut log, 1);
    log.close(
        r,
        Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact),
        at(2),
    )
    .expect("closes");
    // Two writers over one log (a stopped writer's successor journaled from
    // 0), and one writer rotating twice (each segment restates the session).
    let mut a = Writer::new("p1-a1", 0);
    a.pass(&log, 512);
    a.rotate();
    a.pass(&log, 512);
    a.rotate();
    let mut b = Writer::new("p2-b2", 10);
    b.pass(&log, 512);
    let chains = [a.read(Liveness::Live), b.read(Liveness::Live)];
    let v = view(&chains);
    let seqs: Vec<u64> = all(&v.log).iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3], "each seq once");
    // NEGATIVE CONTROL: without the dedupe the second writer's lines show
    // again.
    let dup = view_of(
        &sid(),
        &chains,
        false,
        ViewRules {
            dedupe: false,
            ..ViewRules::READER
        },
    )
    .expect("a view");
    assert!(all(&dup.log).len() > 3);
}

#[test]
fn a_dead_chain_reads_crash_closes_innermost_first() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let r = cmd(&mut log, 1);
    let t = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(r),
            at(2),
        )
        .expect("a turn");
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    let head = log.last_seq();
    for (live, ended, crashed) in [
        (Liveness::Dead, false, true),
        (Liveness::Live, false, false),
        (Liveness::Unknown, false, false),
        (Liveness::Dead, true, false),
    ] {
        let mut chain = w.read(live);
        chain.ended = ended;
        let v = view(&[chain]);
        assert_eq!(v.crashed, crashed, "{live:?} ended={ended}");
        let recs = all(&v.log);
        if !crashed {
            assert_eq!(recs.len() as u64, head);
            continue;
        }
        let tail: Vec<String> = recs[head as usize..]
            .iter()
            .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).expect("fits"))
            .collect();
        assert!(
            tail[0].starts_with(&format!("{} gap - gap why=crash after={head}", head + 1)),
            "{tail:?}"
        );
        // Innermost first: the turn, the run, then the session.
        let closes: Vec<Node> = recs[head as usize + 1..].iter().map(|r| r.node).collect();
        assert_eq!(closes, vec![t, r, Node::SESSION]);
        for (r, line) in recs[head as usize + 1..].iter().zip(&tail[1..]) {
            assert_eq!(r.op, Op::Close);
            assert!(
                line.contains("out=gone") && line.contains("by=aterm"),
                "{line}"
            );
        }
        assert!(!v.log.is_open(Node::SESSION));
        assert_eq!(v.head, head);
    }
    // NEGATIVE CONTROL: without the rule a dead chain's tree stays open.
    let open = view_of(
        &sid(),
        &[w.read(Liveness::Dead)],
        false,
        ViewRules {
            crash_closes: false,
            ..ViewRules::READER
        },
    )
    .expect("a view");
    assert!(open.log.is_open(t));
}

#[test]
fn a_chain_ended_by_a_quit_or_a_handoff_reads_no_crash() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let _ = cmd(&mut log, 1);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    // The quit's own closes, then `#fin`.
    let closes = log.quit_closes(at(5));
    assert!(
        log.is_open(Node::SESSION),
        "quit_closes leaves the log as it was"
    );
    for r in &closes {
        w.line(&Frame::Rec {
            sid: sid(),
            rec: r.clone(),
            restated: false,
        });
    }
    w.fin(EndWhy::Quit);
    let v = view(&[w.read(Liveness::Dead)]);
    assert!(!v.crashed);
    let last = all(&v.log).last().cloned().expect("records");
    assert_eq!(last.out(), Some("app-quit"));
    assert!(!v.log.is_open(Node::SESSION));
    // A `#handed` for the session is no crash either, its trailer cut short.
    let mut h = Writer::new("p1-a1", 0);
    h.pass(&log, 512);
    h.line(&Frame::Handed {
        sid: sid(),
        fence: log.last_seq(),
    });
    let v = view(&[h.read(Liveness::Dead)]);
    assert!(!v.crashed && v.log.is_open(Node::SESSION));
}

#[test]
fn a_close_the_writer_never_saw_reads_unknown_at_the_later_snap() {
    // A two-record ring: the run's close is evicted before the writer runs.
    let limits = Limits::new(3, 1 << 20).expect("a tiny ring");
    let mut log = Log::with_limits(1, limits);
    open_session(&mut log, 0);
    let r = cmd(&mut log, 1);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    log.close(
        r,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
        at(2),
    )
    .expect("closes");
    for i in 0..6 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(3 + i));
    }
    assert!(
        !log.window().any(|x| x.node == r && x.op == Op::Close),
        "evicted"
    );
    w.pass(&log, 512);
    let v = view(&[w.read(Liveness::Live)]);
    assert!(!v.log.is_open(r), "a later snap does not list it");
    assert_eq!(v.unknown_closed, 1);
    let close = all(&v.log)
        .into_iter()
        .find(|x| x.node == r && x.op == Op::Close)
        .expect("a synthetic close");
    assert_eq!(close.out(), Some("unknown"));
    assert_eq!(close.by, Actor::Aterm);
}

#[test]
fn a_pruned_prefix_and_an_unread_one_say_so() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    for i in 0..5 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(1 + i));
    }
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    w.rotate();
    let _ = cmd(&mut log, 9);
    w.pass(&log, 512);
    let mut chain = w.read(Liveness::Live);
    chain.segments.remove(0);
    for (unread, why) in [(false, GapWhy::Pruned), (true, GapWhy::Unread)] {
        let v = view_of(
            &sid(),
            std::slice::from_ref(&chain),
            unread,
            ViewRules::READER,
        )
        .expect("a view");
        let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
        assert_eq!(read.gap.map(|g| g.why), Some(why), "{read:?}");
        // The restated session open and the newer run are there.
        let recs = all(&v.log);
        assert_eq!(recs.first().map(|r| r.seq), Some(1));
        assert!(recs.iter().any(|r| r.k == Kind::Cmd));
    }
}

#[test]
fn an_update_reads_once_across_both_chains_and_says_a_shortfall() {
    let mut a_log = Log::new(1);
    open_session(&mut a_log, 0);
    let r = cmd(&mut a_log, 1);
    for i in 0..4 {
        let _ = a_log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(2 + i));
    }
    for cut in [None, Some(2usize)] {
        let mut a = Writer::new("p1-a1", 0);
        match cut {
            None => a.pass(&a_log, 512),
            Some(k) => {
                // The drain cut short: A journaled all but the last k.
                let tk = take(&a_log, 0, (a_log.last_seq() as usize) - k, true);
                for f in take_frames(sid(), &tk, true, 1) {
                    a.line(&f);
                }
            }
        }
        // The carry, fenced at A's head; the successor continues it.
        let mut src = a_log.clone();
        let snap = src.carry_snapshot();
        src.fence_reads();
        let carry = snap
            .prepare()
            .encode(CarryBudget::SESSION, &[])
            .expect("a carry");
        let h = a_log.last_seq();
        if cut.is_none() {
            a.line(&Frame::Handed {
                sid: sid(),
                fence: h,
            });
        }
        a.fin(EndWhy::Handoff);
        let (mut b_log, _, stats) = Log::from_carry(
            &carry.lines,
            CarryAdopt {
                ep: 2,
                next_seq: h + 65536 + 1,
                ids: IdCounters::default(),
                limits: Limits::SESSION,
                at: at(50),
            },
        )
        .expect("carried");
        assert_eq!(stats.carried, h);
        b_log
            .close(
                r,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                at(60),
            )
            .expect("closes in B");
        let mut b = Writer::new("p2-b2", 100);
        b.cursor = h;
        b.pass(&b_log, 512);
        let v = view(&[a.read(Liveness::Dead), b.read(Liveness::Live)]);
        assert!(!v.crashed);
        let recs = all(&v.log);
        let mut seqs: Vec<u64> = recs.iter().map(|r| r.seq).collect();
        seqs.dedup();
        assert_eq!(seqs.len(), recs.len(), "each seq once across both chains");
        let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
        let g = read.gap.expect("the overlap is said");
        assert_eq!(g.why, GapWhy::Handoff);
        assert_eq!(g.lost, cut.unwrap_or(0) as u64, "the shortfall counted");
        assert!(!v.log.is_open(r), "B's close of the carried run");
    }
}

/// A READER HOLDS EVERY RECORD IT READ: 10⁵ journaled records read back are
/// all in the view and none is said lost (a reply's ring is sized to its
/// read, which the read's own budget bounds), and `n=`-style paging reaches
/// the oldest.
#[test]
fn a_reader_holds_every_record_it_read() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let mut w = Writer::new("p1-a1", 0);
    for i in 0..100_000u64 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(i));
        if i % 1500 == 0 {
            w.pass(&log, 512);
        }
    }
    w.pass(&log, 512);
    let v = view(&[w.read(Liveness::Live)]);
    assert_eq!(v.log.last_seq(), log.last_seq());
    let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
    assert!(
        read.gap.is_none(),
        "nothing on disk said lost: {:?}",
        read.gap
    );
    assert_eq!(read.recs.len() as u64, log.last_seq(), "every seq held");
    // NEGATIVE CONTROL: a session's ring holds a fraction of them.
    assert!(v.log.len() > MAX_RECORDS && v.log.bytes() > MAX_BYTES);
    let _ = Key::N;
}

/// ONE TAKE PER PASS: the host's writer takes at most `max` records of a
/// track per pass and owes the rest, so it is often behind its log when it
/// starts a segment or snaps after a loss. Over seeded traffic on a small
/// ring (with and without the writer telling the ring how far it read, as a
/// stopped writer does not), random rotations, and a dead or live chain at
/// the end: no range is said pruned (nothing was), a dead chain's crash gap
/// says `after=` the writer's cursor, each seq is shown once, and every seq
/// at or below the cursor the view does not hold is counted lost.
/// NEGATIVE CONTROL: the writer and reader before phase 2B's fix (a snap
/// restating the open path past its head, counted toward the head) break
/// the first two on some seed.
#[test]
fn one_take_per_pass_never_invents_a_loss() {
    let walk = |seed: u64, mark: bool, before_fix: bool| -> Vec<String> {
        let mut rng = XorShift::new(seed);
        let limits = Limits::new(32, 1 << 20).expect("a small ring");
        let mut log = Log::with_limits(1, limits);
        open_session(&mut log, 0);
        let mut w = Writer::new("p1-a1", 0);
        let mut ms = 0;
        for _ in 0..300 {
            traffic(&mut log, &mut rng, &mut ms);
            if rng.below(4) == 0 {
                w.pass_once(&mut log, 1 + rng.below(16) as usize, mark, before_fix);
            }
            if rng.below(60) == 0 {
                w.rotate();
            }
        }
        let live = if rng.below(2) == 0 {
            Liveness::Dead
        } else {
            Liveness::Live
        };
        let rules = if before_fix {
            ViewRules {
                restated_heads: false,
                ..ViewRules::READER
            }
        } else {
            ViewRules::READER
        };
        let v = view_of(&sid(), &[w.read(live)], false, rules).expect("a view");
        let mut wrong = Vec::new();
        let pruned: Vec<(u64, u64)> = v
            .log
            .holes_why()
            .filter(|h| h.2 == GapWhy::Pruned)
            .map(|h| (h.0, h.1))
            .collect();
        if !pruned.is_empty() {
            wrong.push(format!("pruned {pruned:?} (cursor {})", w.cursor));
        }
        let shown = all(&v.log);
        let crash_after = shown
            .iter()
            .find(|r| r.op == Op::Gap && r.f.get_word(Key::Why) == Some("crash"))
            .and_then(|r| match r.f.get(Key::After) {
                Some(aterm_slog::Val::U(n)) => Some(*n),
                _ => None,
            });
        if let Some(after) = crash_after
            && after != w.cursor
        {
            wrong.push(format!("crash after={after}, the cursor {}", w.cursor));
        }
        let mut seqs: Vec<u64> = shown.iter().map(|r| r.seq).collect();
        seqs.dedup();
        if seqs.len() != shown.len() {
            wrong.push("a seq twice".to_string());
        }
        let held = shown.iter().filter(|r| r.seq <= w.cursor).count() as u64;
        let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
        if !before_fix && read.gap.map_or(0, |g| g.lost) != w.cursor - held {
            wrong.push(format!(
                "lost {:?}, the cursor {} holds {held}",
                read.gap, w.cursor
            ));
        }
        wrong
    };
    let walks: Vec<(u64, bool)> = (1..=300u64)
        .flat_map(|seed| [(seed, true), (seed, false)])
        .collect();
    let wrongs = aterm_spec::check::each(&walks, |&(seed, mark)| walk(seed, mark, false));
    for (&(seed, mark), wrong) in walks.iter().zip(wrongs) {
        assert!(wrong.is_empty(), "seed {seed} mark={mark}: {wrong:?}");
    }
    // Caught first on seed 4, and on 15 of the 300 (2026-10-17).
    assert!(
        (1..=300u64).any(|s| !walk(s, false, true).is_empty()),
        "the writer and reader before the fix are caught"
    );
}

/// THE RING AND THE WRITER: a ring told how far the writer read evicts what
/// the writer holds before what it has not reached, so a burst of commands
/// between two passes, smaller than the ring, loses nothing the writer can
/// still take; the ring keeps its bounds. NEGATIVE CONTROL: the same burst
/// on a ring the writer never told evicts records it had not reached.
#[test]
fn a_burst_between_passes_loses_nothing_the_writer_could_write() {
    let burst = |mark: bool| -> (u64, usize) {
        let limits = Limits::new(64, 1 << 20).expect("a ring");
        let mut log = Log::with_limits(1, limits);
        open_session(&mut log, 0);
        let mut ms = 1;
        let run = |log: &mut Log, ms: &mut u64| {
            *ms += 2;
            let r = cmd(log, *ms);
            log.close(
                r,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                at(*ms + 1),
            )
            .expect("closes");
        };
        for _ in 0..40 {
            run(&mut log, &mut ms);
        }
        let mut w = Writer::new("p1-a1", 0);
        w.pass_once(&mut log, 512, mark, false);
        // A burst: 25 commands, 50 records, before the next pass.
        for _ in 0..25 {
            run(&mut log, &mut ms);
        }
        assert!(log.len() <= 64, "the ring keeps its bounds");
        let tk = take(&log, w.cursor, 512, false);
        (tk.gap.map_or(0, |g| g.lost), log.len())
    };
    let (lost, _) = burst(true);
    assert_eq!(lost, 0, "nothing the writer could take was evicted");
    let (lost, _) = burst(false);
    assert!(
        lost > 0,
        "NEGATIVE CONTROL: without the mark the burst loses"
    );
}

/// A TAKE IS DONE only when the session's close is among what it took: a
/// session that closes during an update's overlap is fenced past its close,
/// and its writer must keep it to write the close after a rollback.
#[test]
fn a_take_is_done_only_with_the_close_taken() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let _ = cmd(&mut log, 1);
    let first = take(&log, 0, 512, true);
    log.fence_reads();
    log.close_session(
        Close::new(
            Out::Session(aterm_slog::SessionOut::CtlClose),
            Src::Aterm,
            Conf::Exact,
        ),
        at(2),
    )
    .expect("closes");
    let fenced = take(&log, first.next, 512, false);
    assert!(fenced.recs.is_empty(), "the fence hides the close");
    assert!(!fenced.done, "not done while its close is fenced off");
    log.lift_read_fence();
    let after = take(&log, first.next, 512, false);
    assert!(after.done && after.recs.iter().any(|r| r.op == Op::Close));
}

/// THE RECORDS A REPLY DERIVES (a crash gap and its closes) are numbered
/// past the journal's head in the journal's own epoch, in the same seq
/// space, and the reply hands out the journal's own cursor, its epoch and
/// head: following it shows what the reply derived again, and nothing else,
/// never a reset; a cursor of the dead process at or below the head reads
/// on; one past it is reset.
#[test]
fn the_reply_hands_out_the_journals_cursor_never_one_into_what_it_derived() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let _ = cmd(&mut log, 1);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    let v = view(&[w.read(Liveness::Dead)]);
    assert!(v.crashed);
    assert_eq!(
        (v.ep, v.log.ep()),
        (log.ep(), log.ep()),
        "the journal's epoch"
    );
    assert_eq!(v.log.state().ep, log.ep(), "a brief names it too");
    assert_eq!(v.reply_cursor(), Cursor::new(log.ep(), v.head));
    assert!(v.log.last_seq() > v.head, "the reply derived records");
    let again = v.log.read(v.cursor(v.reply_cursor()), usize::MAX);
    assert!(again.gap.is_none(), "{again:?}");
    assert!(
        !again.recs.is_empty() && again.recs.iter().all(|r| r.seq > v.head),
        "what it derived, again: {again:?}"
    );
    let old = v
        .log
        .read(v.cursor(Cursor::new(log.ep(), v.head - 1)), usize::MAX);
    assert!(old.gap.is_none(), "{old:?}");
    assert_eq!(old.recs.len() as u64, v.log.last_seq() - v.head + 1);
    let past = v
        .log
        .read(v.cursor(Cursor::new(log.ep(), v.head + 1)), usize::MAX);
    assert_eq!(past.gap.map(|g| g.why), Some(GapWhy::Reset), "{past:?}");
    // A live chain's view derives nothing.
    let live = view(&[w.read(Liveness::Live)]);
    assert_eq!(live.log.last_seq(), live.head);
}

/// A READ THAT DID NOT REACH ALL OF THE JOURNAL SAYS SO: what it missed past
/// its head (a newer segment it could not list) reads as a `gap why=unread`
/// record of the reply's, and the chain is never read as a crash.
#[test]
fn a_read_that_missed_newer_lines_says_unread() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    let mut chain = w.read(Liveness::Unknown);
    chain.ended = false;
    let v = view_of(&sid(), &[chain.clone()], true, ViewRules::READER).expect("a view");
    let recs = all(&v.log);
    assert!(
        recs.iter()
            .any(|r| r.op == Op::Gap && r.f.get_word(Key::Why) == Some("unread")),
        "{recs:?}"
    );
    assert!(!v.crashed);
    // NEGATIVE CONTROL: a whole read adds nothing.
    let whole = view_of(&sid(), &[chain], false, ViewRules::READER).expect("a view");
    assert_eq!(all(&whole.log).len(), 1);
}

/// A SESSION THE WRITER LET GO (its backstop) is said in its space: a snap
/// first, then `#gap why=dropped` over what it had not written.
#[test]
fn a_dropped_session_is_said_in_its_space() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    for i in 0..5 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(1 + i));
    }
    w.rotate();
    let probe = take(&log, w.cursor, 0, true);
    let through = log.last_seq();
    for f in aterm_slog::journal::dropped_frames(sid(), &probe, through, 5, true, 9) {
        w.line(&f);
    }
    let v = view(&[w.read(Liveness::Live)]);
    let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
    assert_eq!(read.gap.map(|g| g.lost), Some(5), "{read:?}");
    assert_eq!(v.log.last_seq(), through);
}

/// THE WRITER'S WAKE (ruling R6) lives in the log: every append (each record
/// of a cascade), every eviction, an overlap and a fence change ring the
/// host's notifier; a log without one rings nothing, and a copy kept for the
/// closed index rings nothing either.
#[test]
fn every_append_and_eviction_rings_the_journal_wake() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let rings = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&rings);
    let mut log = Log::new(1);
    log.set_notify(Some(aterm_slog::Notify::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    })));
    let n = || rings.load(Ordering::Relaxed);
    open_session(&mut log, 0);
    assert_eq!(n(), 1, "the session's open");
    let r = cmd(&mut log, 1);
    let _ = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(r),
            at(2),
        )
        .expect("a turn");
    assert_eq!(n(), 3);
    log.close(
        r,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
        at(3),
    )
    .expect("closes");
    assert_eq!(n(), 5, "the cascade's two closes");
    for i in 0..20 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(4 + i));
    }
    let before = n();
    log.set_byte_budget(1);
    assert!(n() > before, "the sweep's eviction rings");
    let before = n();
    log.skip_to(log.last_seq() + 10);
    assert_eq!(n(), before + 1, "an overlap");
    log.fence_reads();
    log.lift_read_fence();
    assert_eq!(n(), before + 3, "the fence and its lift");
    let kept = log.kept_within(1024);
    assert!(
        kept.notify().is_none(),
        "the closed index's copy rings nothing"
    );
    let mut quiet = Log::new(1);
    open_session(&mut quiet, 0);
    assert!(quiet.notify().is_none());
    let _ = IdCounters::default();
}

/// The cursor a reply hands a reader: the rule's ([`View::reply_cursor`]),
/// or, as a NEGATIVE CONTROL, the reply's own last record's (`Read::next`),
/// which names a record the reply derived.
#[derive(Clone, Copy)]
enum Hand {
    Rule,
    OwnLast,
}

/// One reply a reader following cursors gets from `v`: its cursor placed by
/// [`View::cursor`] (none: the whole view), the records read on from it,
/// and the cursor the reply hands back (`hand`).
fn follow(v: &View, c: Option<Cursor>, hand: Hand) -> (Vec<Rec>, Cursor) {
    let from = c.map_or(Cursor::new(v.log.ep(), 0), |c| v.cursor(c));
    let read = v.log.read(from, usize::MAX);
    let next = match hand {
        Hand::Rule => v.reply_cursor(),
        Hand::OwnLast => read.next,
    };
    let recs: Vec<Rec> = read.recs.into_iter().cloned().collect();
    (recs, next)
}

/// What a reader following cursors across `views` (one reply each) never
/// saw of the last view's journaled records, and the seqs any one reply
/// showed twice.
fn followed(views: &[View], hand: Hand) -> (Vec<u64>, Vec<u64>) {
    let mut seen = std::collections::BTreeSet::new();
    let mut twice = Vec::new();
    let mut c = None;
    for v in views {
        let (recs, next) = follow(v, c, hand);
        let mut mine = std::collections::BTreeSet::new();
        for r in &recs {
            if !mine.insert(r.seq) {
                twice.push(r.seq);
            }
            // Only the journal's own records are kept: the reply's are
            // derived anew by each reply.
            if r.seq <= v.head {
                seen.insert(r.seq);
            }
        }
        c = Some(next);
    }
    let last = views.last().expect("a view");
    let missed = all(&last.log)
        .iter()
        .filter(|r| r.seq <= last.head && !seen.contains(&r.seq))
        .map(|r| r.seq)
        .collect();
    (missed, twice)
}

/// A READER FOLLOWING ONE REPLY'S CURSOR INTO THE NEXT NEVER SKIPS A RECORD
/// THE JOURNAL HOLDS: a cut read says `gap why=unread` by a record it derives
/// past its head; the whole read that follows holds journaled records at
/// those seqs and derives a crash. The first reply handed out its head, so
/// every journaled record is shown, none twice in a reply; the second
/// reply's cursor then reads only what it derives, again. NEGATIVE CONTROL:
/// replies handing out their own last record's cursor skip the record the
/// cut read's derived one stood for.
#[test]
fn a_cut_read_then_a_whole_one_skips_no_journaled_record() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    for i in 0..3 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(1 + i));
    }
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    w.rotate();
    for i in 0..3 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(5 + i));
    }
    w.pass(&log, 512);
    let views = |rules: ViewRules| -> Vec<View> {
        let mut cut = w.read(Liveness::Unknown);
        cut.segments.truncate(1);
        let first = view_of(&sid(), &[cut], true, rules).expect("a view");
        let whole = view_of(&sid(), &[w.read(Liveness::Dead)], false, rules).expect("a view");
        vec![first, whole]
    };
    let vs = views(ViewRules::READER);
    assert!(vs[0].head < vs[1].head && vs[1].crashed);
    let (missed, twice) = followed(&vs, Hand::Rule);
    assert!(
        missed.is_empty() && twice.is_empty(),
        "{missed:?} {twice:?}"
    );
    // The second reply's cursor reads only what the reply derives, again.
    let (_, next) = follow(&vs[1], None, Hand::Rule);
    let (again, _) = follow(&vs[1], Some(next), Hand::Rule);
    assert!(
        !again.is_empty() && again.iter().all(|r| r.seq > vs[1].head),
        "{again:?}"
    );
    // NEGATIVE CONTROL.
    let (missed, _) = followed(&vs, Hand::OwnLast);
    assert!(!missed.is_empty(), "the reply's own cursor skips a record");
}

/// THE SAME ON A LIVE CHAIN THAT GROWS between two replies, each deriving an
/// `out=unknown` close past its head (a run whose close the writer never
/// saw): the first reply handed out its head, so the records journaled since
/// are shown, and the unknown close is shown again, past the new head.
/// NEGATIVE CONTROL: the reply's own last record's cursor reads on past a
/// journaled record.
#[test]
fn a_growing_live_chain_read_twice_skips_no_journaled_record() {
    let limits = Limits::new(3, 1 << 20).expect("a tiny ring");
    let mut log = Log::with_limits(1, limits);
    open_session(&mut log, 0);
    let r = cmd(&mut log, 1);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    log.close(
        r,
        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
        at(2),
    )
    .expect("closes");
    for i in 0..6 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(3 + i));
    }
    w.pass(&log, 512);
    let before = w.read(Liveness::Live);
    for i in 0..2 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(20 + i));
    }
    w.pass(&log, 512);
    let after = w.read(Liveness::Live);
    let views = |rules: ViewRules| -> Vec<View> {
        [&before, &after]
            .into_iter()
            .map(|c| view_of(&sid(), std::slice::from_ref(c), false, rules).expect("a view"))
            .collect()
    };
    let vs = views(ViewRules::READER);
    assert_eq!(vs[0].unknown_closed, 1);
    assert_eq!(vs[1].unknown_closed, 1);
    assert!(vs[0].head < vs[1].head);
    let (missed, twice) = followed(&vs, Hand::Rule);
    assert!(
        missed.is_empty() && twice.is_empty(),
        "{missed:?} {twice:?}"
    );
    let (missed, _) = followed(&vs, Hand::OwnLast);
    assert!(!missed.is_empty(), "the reply's own cursor skips a record");
}

/// THE CURSOR RULE'S EDGES: the dead process's cursor past the head (a
/// reply's over more of the journal than this read reached among them), an
/// epoch past the journal's (the largest too) and one before its space's
/// base are reset; one of the journal's at or below the head reads on.
#[test]
fn a_cursor_the_view_cannot_place_is_reset() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let _ = cmd(&mut log, 1);
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    let v = view(&[w.read(Liveness::Dead)]);
    let reset = |c: Cursor| {
        v.log
            .read(v.cursor(c), usize::MAX)
            .gap
            .is_some_and(|g| g.why == GapWhy::Reset)
    };
    assert!(reset(Cursor::new(v.ep, v.head + 1)), "past the head");
    assert!(reset(Cursor::new(v.ep, v.head + 5)), "a reply's over more");
    assert!(!reset(Cursor::new(v.ep, v.head)));
    assert!(!reset(Cursor::new(v.ep, 0)));
    assert!(reset(Cursor::new(v.ep + 1, 1)), "the next epoch");
    assert!(reset(Cursor::new(u32::MAX, 1)), "the largest epoch");
    let mut later = Log::new(5);
    open_session(&mut later, 0);
    let mut wl = Writer::new("p2-a1", 0);
    wl.pass(&later, 512);
    let vl = view(&[wl.read(Liveness::Dead)]);
    assert_eq!(vl.log.base_ep(), 5);
    assert!(
        vl.log
            .read(vl.cursor(Cursor::new(4, 1)), usize::MAX)
            .gap
            .is_some_and(|g| g.why == GapWhy::Reset),
        "before the space's base"
    );
}

/// A RESTATEMENT PAST ITS SNAP'S HEAD ACCOUNTS FOR NOTHING (the reader's
/// half of the restatement rule, over hand-built frames as a writer before
/// phase 2B's fix or a corrupt segment writes them, whatever this build's
/// writer does): a dead chain journaled through seq 3 whose newest snap, at
/// head 3, restates a run opened at seq 6 reads no pruned range, its crash
/// gap says `after=3`, its head is 3, and the run is never shown.
/// NEGATIVE CONTROL: a reader that takes the restatement (`restated_heads`
/// off) breaks every one of those.
#[test]
fn a_restatement_past_its_snaps_head_accounts_for_nothing() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    for i in 0..4 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(1 + i));
    }
    let r = cmd(&mut log, 9);
    let recs = all(&log);
    assert_eq!(recs.last().map(|x| (x.seq, x.node)), Some((6, r)));
    let inst = Inst::new("p1-a1").expect("an instance");
    let rec = |seq: u64, restated: bool| Frame::Rec {
        sid: sid(),
        rec: recs.iter().find(|x| x.seq == seq).cloned().expect("held"),
        restated,
    };
    let snap = |head: u64, open: u32, t: u64| Frame::Snap {
        sid: sid(),
        head,
        ep: log.ep(),
        base: log.base_ep(),
        first: log.first_seq(),
        open,
        t,
    };
    let seg = |n: u32, body: Vec<Frame>| {
        let mut frames = vec![Frame::Seg {
            v: FORMAT,
            inst: inst.clone(),
            n,
            t: u64::from(n),
            build: "test".to_string(),
        }];
        frames.extend(body);
        SegmentRead { n, frames }
    };
    let chain = ChainRead {
        inst: inst.clone(),
        segments: vec![
            seg(
                1,
                vec![snap(0, 0, 1), rec(1, false), rec(2, false), rec(3, false)],
            ),
            seg(2, vec![snap(3, 2, 2), rec(1, true), rec(6, true)]),
        ],
        ended: false,
        live: Liveness::Dead,
        start_t: 1,
    };
    let wrong = |rules: ViewRules| -> Vec<String> {
        let Some(v) = view_of(&sid(), std::slice::from_ref(&chain), false, rules) else {
            return vec!["no view".to_string()];
        };
        let mut wrong = Vec::new();
        let pruned: Vec<(u64, u64)> = v
            .log
            .holes_why()
            .filter(|h| h.2 == GapWhy::Pruned)
            .map(|h| (h.0, h.1))
            .collect();
        if !pruned.is_empty() {
            wrong.push(format!("pruned {pruned:?}"));
        }
        if v.head != 3 {
            wrong.push(format!("head {}", v.head));
        }
        let shown = all(&v.log);
        let after = shown
            .iter()
            .find(|x| x.op == Op::Gap && x.f.get_word(Key::Why) == Some("crash"))
            .and_then(|x| match x.f.get(Key::After) {
                Some(aterm_slog::Val::U(n)) => Some(*n),
                _ => None,
            });
        if after != Some(3) {
            wrong.push(format!("crash after={after:?}"));
        }
        if shown.iter().any(|x| x.node == r) {
            wrong.push("the run is shown".to_string());
        }
        wrong
    };
    let reader = wrong(ViewRules::READER);
    assert!(reader.is_empty(), "{reader:?}");
    let before = wrong(ViewRules {
        restated_heads: false,
        ..ViewRules::READER
    });
    assert_eq!(before.len(), 4, "{before:?}");
}

/// A crash recovery's log of the session (its epoch `ep`, the Unix seconds
/// `crash_epoch` gives one): its crash gap, its open and `n` marks,
/// journaled whole by `inst`'s writer.
fn recovered_space(ep: u32, n: u64, inst: &str, ms: u64) -> Writer {
    let mut log = Log::new(ep);
    let _ = log.gap(GapWhy::Crash, 0, at(ms));
    open_session(&mut log, ms);
    for i in 0..n {
        let _ = log.mark(
            Mark::new(Kind::Human, Src::Input, Conf::Exact),
            at(ms + 1 + i),
        );
    }
    let mut w = Writer::new(inst, ms);
    w.pass(&log, 512);
    w
}

/// Where `v` reads a reader's cursor `c` on from, placed by `place`: the
/// first seq it shows and the gap it says.
fn placed_by(
    v: &View,
    c: Cursor,
    place: fn(&View, Cursor) -> Cursor,
) -> (Option<u64>, Option<GapWhy>) {
    let read = v.log.read(place(v, c), usize::MAX);
    (read.recs.first().map(|r| r.seq), read.gap.map(|g| g.why))
}

/// [`placed_by`] the rule ([`View::cursor`]).
fn placed(v: &View, c: Cursor) -> (Option<u64>, Option<GapWhy>) {
    placed_by(v, c, View::cursor)
}

/// NEGATIVE CONTROL: the placement the rule replaced, which read any epoch
/// above the journal's as a reply's built over head `(ep - journal ep - 1) /
/// 2` and moved the cursor into the journal's epoch at or below that head.
fn keyed_placement(v: &View, c: Cursor) -> Cursor {
    let reset = Cursor::new(v.ep, u64::MAX);
    if c.ep <= v.ep {
        return if c.seq > v.head { reset } else { c };
    }
    let over = u64::from(c.ep - v.ep - 1) / 2;
    if c.ep == u32::MAX || over > v.head {
        reset
    } else {
        Cursor::new(v.ep, c.seq.min(over))
    }
}

/// A CURSOR OF ANOTHER SEQ SPACE IS RESET, SAID, in both directions. Two
/// crash recoveries of one shell ten minutes apart each start a space of
/// their own, their epochs Unix seconds (`crash_epoch`), and each holds 402
/// records, so a reply epoch over the earlier space under the keyed rule
/// (`ep + 1 + 2·head`) lands past the later space's epoch. Read in the later
/// space (the newest answers): the earlier process's live cursor and the
/// cursors replies over the earlier space handed out. Read in a journal
/// that holds only the earlier space (the later writer had not written):
/// the later process's live cursor and its replies'. Every one is reset and
/// the reset said: none is placed inside a space it was not read in, where
/// it would skip that space's records, its crash gap and open among them,
/// unsaid. NEGATIVE CONTROL: the keyed placement does skip them.
#[test]
fn a_cursor_of_another_space_is_reset_said() {
    let s: u32 = 1_792_100_000;
    let wa = recovered_space(s, 400, "p1-a1", 0);
    let wb = recovered_space(s + 600, 400, "p2-a1", 600_000);
    let va = view(&[wa.read(Liveness::Dead)]);
    let vb = view(&[wa.read(Liveness::Dead), wb.read(Liveness::Dead)]);
    assert!(va.crashed && vb.crashed);
    assert_eq!((va.ep, vb.ep), (s, s + 600), "the newest space answers");
    assert!(
        s + 1 + 2 * va.head as u32 > vb.ep,
        "the keyed epoch lands past"
    );
    // What a reader of a space can hold: its process's live cursor, and the
    // cursors replies over it handed out (this rule's; the keyed rule's; the
    // first reader's, one epoch past the journal's).
    let held = |v: &View| {
        let own = v.log.last_seq();
        vec![
            Cursor::new(v.ep, 350),
            v.reply_cursor(),
            Cursor::new(v.ep + 1 + 2 * v.head as u32, own),
            Cursor::new(v.ep + 1, own),
        ]
    };
    let mut skipped = 0;
    for (from, to) in [(&va, &vb), (&vb, &va)] {
        for c in held(from) {
            assert_eq!(
                placed(to, c),
                (Some(1), Some(GapWhy::Reset)),
                "{c:?} read in the space of epoch {}",
                to.ep
            );
            if let (Some(first), None) = placed_by(to, c, keyed_placement) {
                skipped += usize::from(first > 1);
            }
        }
    }
    assert!(skipped >= 2, "the keyed placement skips unsaid in both");
}

/// A SUCCESSOR'S CURSOR AGAINST A JOURNAL THAT LAGS IS RESET, SAID: a
/// carried update's log continues the space in the next epoch (the
/// predecessor's plus one), and its writer had not written when the journal
/// was read. Its cursor, past the journal's head, at it or below it, names
/// an epoch the journal never numbered: it is reset and the reset said, on
/// a chain read live (the reply derives nothing) and on one read dead (it
/// derives a crash). NEGATIVE CONTROL: the keyed placement reads each from
/// the start, no reset said.
#[test]
fn a_successors_cursor_against_a_lagging_journal_is_reset_said() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    for i in 0..50 {
        let _ = log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(1 + i));
    }
    let mut w = Writer::new("p1-a1", 0);
    w.pass(&log, 512);
    for live in [Liveness::Live, Liveness::Dead] {
        let v = view(&[w.read(live)]);
        assert_eq!((v.ep, v.head), (1, 51));
        for seq in [v.head + 20, v.head, v.head - 10] {
            let c = Cursor::new(2, seq);
            assert_eq!(
                placed(&v, c),
                (Some(1), Some(GapWhy::Reset)),
                "{c:?} {live:?}"
            );
            assert_eq!(placed_by(&v, c, keyed_placement), (Some(1), None));
        }
    }
}

/// Decoded journal seqs are bounded before the view does range arithmetic.
/// The inclusive limit is a real sparse view, not a loop through its seqs.
#[test]
fn a_decoded_journal_seq_outside_the_logs_bound_is_refused_before_ranges() {
    let mut log = Log::new(1);
    open_session(&mut log, 0);
    let mut ordinary = Writer::new("p1-a1", 0);
    ordinary.pass(&log, 8);
    assert!(
        view(&[ordinary.read(Liveness::Live)])
            .log
            .is_open(Node::SESSION)
    );
    let opened = all(&log).into_iter().next().expect("session open");
    let (ep, base) = (log.ep(), log.base_ep());
    let read = |head: u64, seq: u64, first: u64| {
        let mut rec = opened.clone();
        rec.seq = seq;
        let mut w = Writer::new("p1-a1", 0);
        w.line(&Frame::Snap {
            sid: sid(),
            head,
            ep,
            base,
            first,
            open: 1,
            t: 1,
        });
        w.line(&Frame::Rec {
            sid: sid(),
            rec,
            restated: true,
        });
        let chain = w.read(Liveness::Live);
        assert_eq!(chain.segments[0].frames.len(), 3, "all frames decoded");
        view_of(&sid(), &[chain], false, ViewRules::READER)
    };
    let limit = aterm_slog::SEQ_LIMIT;
    let boundary = read(limit, limit, 1).expect("the inclusive limit is valid");
    assert_eq!(boundary.head, limit);
    assert!(boundary.log.is_open(Node::SESSION));
    let refused: Vec<bool> = [
        (limit + 1, limit + 1, 1),
        (u64::MAX, u64::MAX, 1),
        (1, 1, u64::MAX),
    ]
    .into_iter()
    .map(|(head, seq, first)| {
        // Observe every malformed input even when the old reader panics on MAX.
        matches!(
            std::panic::catch_unwind(|| read(head, seq, first)),
            Ok(None)
        )
    })
    .collect();
    assert_eq!(
        refused,
        vec![true; 3],
        "each malformed view must refuse without panic"
    );
}
