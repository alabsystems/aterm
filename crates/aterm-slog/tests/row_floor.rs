// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TIMELINE'S ROWS: a host's grammar
//! ([`Log::keep_rows`]) flags the records its timeline prints, each takes
//! the next dense row ids, and the newest of them, up to [`row_floor`],
//! outlive every other record an eviction batch may take.
//!
//! The flood (10^4 status changes, 600 directory moves, 300 program flaps
//! and 2000 runs, the last 400 runs after every row) keeps the newest rows
//! within the floor, their ids the ones they took (the program marks aside,
//! under their share); every record is live or counted lost
//! (`NoSilentLoss`), and the fold is what a refold of every append makes
//! (`ProjectionIndependentOfEviction`). The ids: dense in append order, a
//! session close two; a carried update's successor and an uncarried one's
//! continue past the ceiling their host carried, and a crash adoption's are
//! named by its new epoch, never by when it started.
//!
//! NEGATIVE CONTROLS: the same flood under no grammar loses the rows the
//! floor keeps (in the test); a successor whose counter is not carried
//! reuses the predecessor's ids (in the test); and, scratch at 6G-1's
//! commit, the floor's deferral taken out of `Log::evict_batch` fails the
//! flood.

use std::collections::{BTreeMap, BTreeSet};

use aterm_slog::{
    Actor, CarryAdopt, CarryBudget, Close, Conf, Fields, Fold, GapWhy, Key, Kind, Limits, Log,
    Mark, Node, Op, Open, Out, PROGRAM_SHARE_MAX, Rec, RunOut, SEQ_LIMIT, SessionOut, Src, Tier,
    encode, row_floor,
};
use aterm_spec::rng::XorShift;

use crate::support::at;

/// The overlap reserve a host carries its count with (aterm-gui's
/// `HANDOFF_ID_RESERVE`).
const RESERVE: u64 = 1 << 16;

/// A timeline grammar shaped as the host's: the session's open and its
/// handoff and crash gaps a row each, its close two (the reason's and the
/// last state's), and the marks a timeline prints one each.
fn grammar(r: &Rec) -> u8 {
    match (r.op, r.node == Node::SESSION, r.k) {
        (Op::Open, true, _) => 1,
        (Op::Close, true, _) => 2,
        (Op::Gap, true, _) => u8::from(matches!(r.f.get_word(Key::Why), Some("handoff" | "crash"))),
        (Op::Mark, _, Kind::Status | Kind::Cwd | Kind::Program | Kind::Title | Kind::Handoff) => 1,
        _ => 0,
    }
}

fn open_session(log: &mut Log, ms: u64) {
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(ms),
    )
    .expect("the session opens");
}

fn status(log: &mut Log, phase: &str, ms: u64) -> u64 {
    log.mark(
        Mark::new(Kind::Status, Src::Status, Conf::Strong)
            .fields(Fields::new().word(Key::Phase, phase).expect("a phase")),
        at(ms),
    )
    .expect("a status mark")
}

fn cwd(log: &mut Log, n: u64, ms: u64) -> u64 {
    log.mark(
        Mark::new(Kind::Cwd, Src::Shell, Conf::Exact).fields(
            Fields::new()
                .text(Key::Cwd, &format!("/private/tmp/x/{n}"))
                .expect("a directory"),
        ),
        at(ms),
    )
    .expect("a cwd mark")
}

fn program(log: &mut Log, state: &str, ms: u64) -> u64 {
    log.mark(
        Mark::new(Kind::Program, Src::Engine, Conf::Claim)
            .at(Node::SESSION)
            .fields(Fields::new().word(Key::State, state).expect("a state")),
        at(ms),
    )
    .expect("a program mark")
}

fn bell(log: &mut Log, ms: u64) -> u64 {
    log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(ms))
        .expect("a bell")
}

/// The ids every live row stands for, by its seq.
fn ids_by_seq(log: &Log) -> BTreeMap<u64, (u64, u8)> {
    log.rows().map(|r| (r.rec.seq, (r.id, r.span))).collect()
}

/// Every row id a log's live rows stand for.
fn ids(log: &Log) -> BTreeSet<u64> {
    log.rows()
        .flat_map(|r| r.id..r.id + u64::from(r.span))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ev {
    Status,
    Cwd,
    Program,
    Run,
}

/// The flood's events: 10^4 status changes, 600 directory moves, 300
/// program flaps and 1600 runs shuffled, then 400 more runs.
fn schedule() -> Vec<Ev> {
    let mut evs = Vec::new();
    evs.extend(std::iter::repeat_n(Ev::Status, 10_000));
    evs.extend(std::iter::repeat_n(Ev::Cwd, 600));
    evs.extend(std::iter::repeat_n(Ev::Program, 300));
    evs.extend(std::iter::repeat_n(Ev::Run, 1_600));
    let mut rng = XorShift::new(0x6061);
    for i in (1..evs.len()).rev() {
        evs.swap(i, rng.index(i + 1));
    }
    evs.extend(std::iter::repeat_n(Ev::Run, 400));
    evs
}

/// One flood under way: the log, every record appended, the fold of every
/// append (the refold, which no eviction touches), and every row's seq,
/// ids and kind in append order.
struct Flood {
    log: Log,
    ms: u64,
    appended: Vec<Rec>,
    refold: Fold,
    rows: Vec<(u64, u64, Kind)>,
    next_id: u64,
}

impl Flood {
    fn new(ruled: bool) -> Flood {
        let mut log = Log::with_limits(2, Limits::SESSION);
        if ruled {
            log.keep_rows(grammar, 0);
        }
        let refold = Fold::new(log.ep());
        let mut f = Flood {
            log,
            ms: 0,
            appended: Vec::new(),
            refold,
            rows: Vec::new(),
            next_id: 1,
        };
        open_session(&mut f.log, 1);
        f.note();
        f
    }

    /// Note the record just appended (the newest, which no batch takes).
    fn note(&mut self) {
        let r = self.log.window().last().expect("the record just appended");
        let span = u64::from(grammar(r));
        if span > 0 {
            self.rows.push((r.seq, self.next_id, r.k));
            self.next_id += span;
        }
        self.refold.apply(r);
        self.appended.push(r.clone());
    }

    fn apply(&mut self, ev: Ev, i: u64) {
        self.ms += 1;
        let ms = self.ms;
        match ev {
            Ev::Status => {
                status(
                    &mut self.log,
                    if i.is_multiple_of(2) {
                        "running"
                    } else {
                        "idle"
                    },
                    ms,
                );
            }
            Ev::Cwd => {
                cwd(&mut self.log, i, ms);
            }
            Ev::Program => {
                program(
                    &mut self.log,
                    if i.is_multiple_of(2) {
                        "working"
                    } else {
                        "idle"
                    },
                    ms,
                );
            }
            Ev::Run => {
                let run = self
                    .log
                    .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
                    .expect("a run opens");
                self.note();
                self.log
                    .close(
                        run,
                        Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                        at(ms),
                    )
                    .expect("the run closes");
            }
        }
        self.note();
    }

    /// The floor's rows as the test counts them: the newest rows within a
    /// quarter of the ring's records and bytes, as (seq, first id, kind).
    fn floor(&self) -> Vec<(u64, u64, Kind)> {
        let q = row_floor(self.log.effective_limits());
        let size: BTreeMap<u64, usize> = self
            .appended
            .iter()
            .map(|r| (r.seq, encode(r, Tier::Content).expect("fits").len()))
            .collect();
        let mut out = Vec::new();
        let mut bytes = 0;
        for &(seq, id, k) in self.rows.iter().rev() {
            // What the ring counts a record: its content-tier line.
            bytes += size[&seq];
            if out.len() == q.records || bytes > q.bytes {
                break;
            }
            out.push((seq, id, k));
        }
        out
    }

    /// The newest `PROGRAM_SHARE_MAX` program marks' seqs: the share's.
    fn program_share(&self) -> BTreeSet<u64> {
        self.rows
            .iter()
            .rev()
            .filter(|r| r.2 == Kind::Program)
            .take(PROGRAM_SHARE_MAX)
            .map(|r| r.0)
            .collect()
    }
}

/// THE FLOOR UNDER A FLOOD: after every 97th event and at the end, each of
/// the newest rows within the floor is live with the ids it took (a program
/// mark past its node's share aside), every record is live or counted lost,
/// and the fold is the refold of every append so far. The floor is the
/// record quarter (512), its bytes well within theirs. CONTROL: the same
/// flood under no grammar keeps almost none of those records, so the floor,
/// not the flood's shape, is what kept them.
#[test]
fn a_flood_keeps_the_newest_rows_within_the_floor() {
    let evs = schedule();
    assert_eq!(evs.len(), 12_900);
    assert_eq!(evs.iter().filter(|e| **e == Ev::Run).count(), 2_000);
    let q = row_floor(Limits::SESSION);
    assert_eq!(q.records, 512, "a session ring's floor: the timeline's cap");
    let mut f = Flood::new(true);
    let check = |f: &Flood, at_event: usize| {
        let live = ids_by_seq(&f.log);
        let share = f.program_share();
        let floor = f.floor();
        for &(seq, id, k) in &floor {
            if k == Kind::Program && !share.contains(&seq) {
                continue;
            }
            assert_eq!(
                live.get(&seq).map(|v| v.0),
                Some(id),
                "event {at_event}: the floor's row at seq {seq} ({k:?}) is gone or renumbered"
            );
        }
        assert_eq!(
            f.log.lost() + f.log.len() as u64,
            f.log.last_seq(),
            "event {at_event}: NoSilentLoss"
        );
        assert_eq!(
            f.refold.state(),
            f.log.fold().state(),
            "event {at_event}: ProjectionIndependentOfEviction"
        );
        floor.len()
    };
    for (i, ev) in evs.iter().enumerate() {
        f.apply(*ev, i as u64);
        if i % 97 == 0 {
            check(&f, i);
        }
    }
    let held = check(&f, evs.len());
    assert_eq!(held, q.records, "the floor is the record quarter");
    assert!(f.log.lost() > 12_000, "the flood evicted: {}", f.log.lost());
    assert_eq!(
        f.log.row_head(),
        f.next_id - 1,
        "every row took the next id, densely"
    );

    // CONTROL: no grammar, no floor: the batch takes those same records.
    let mut bare = Flood::new(false);
    for (i, ev) in evs.iter().enumerate() {
        bare.apply(*ev, i as u64);
    }
    let floor_seqs: BTreeSet<u64> = f.floor().iter().map(|r| r.0).collect();
    let kept = bare
        .log
        .window()
        .filter(|r| floor_seqs.contains(&r.seq))
        .count();
    assert!(
        kept < floor_seqs.len() / 4,
        "CONTROL: without the floor the flood keeps {kept} of the {} rows",
        floor_seqs.len()
    );
}

/// DENSE IDS: each row takes the next ids in append order (a session close
/// two), a record the grammar does not print takes none, and a row the ring
/// no longer holds is a hole [`Log::rows_lost_between`] counts. On a ring of
/// eight (a floor of two) a run of bells takes the older rows outside the
/// floor and leaves the two newest.
#[test]
fn row_ids_are_dense_in_append_order_and_a_session_close_takes_two() {
    let limits = Limits::new(8, 1 << 20).expect("a small ring");
    assert_eq!(row_floor(limits).records, 2);
    let mut log = Log::with_limits(3, limits);
    log.keep_rows(grammar, 0);
    open_session(&mut log, 1);
    let c1 = cwd(&mut log, 1, 2);
    bell(&mut log, 3);
    let s1 = status(&mut log, "running", 4);
    let c2 = cwd(&mut log, 2, 5);
    let session_open = log.window().next().expect("the open").seq;
    assert_eq!(
        ids_by_seq(&log).into_iter().collect::<Vec<_>>(),
        [
            (session_open, (1, 1)),
            (c1, (2, 1)),
            (s1, (3, 1)),
            (c2, (4, 1))
        ],
        "a bell is no row"
    );
    for ms in 6..12 {
        bell(&mut log, ms);
    }
    assert_eq!(log.len(), 7, "the batch ran: {}", log.len());
    let live = ids_by_seq(&log);
    assert!(
        live.contains_key(&s1) && live.contains_key(&c2),
        "the floor's two rows stay: {live:?}"
    );
    assert!(!live.contains_key(&c1), "the row outside the floor went");
    assert_eq!(log.rows_lost_between(0, 4), 1, "id 2 is the one hole");
    assert_eq!(log.rows_lost_between(2, 4), 0);
    assert_eq!(log.rows_lost_between(1, 3), 1);
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact),
        at(20),
    )
    .expect("the session closes");
    let close = log.last_seq();
    assert_eq!(ids_by_seq(&log).get(&close), Some(&(5, 2)));
    assert_eq!(
        (log.row_first(), log.row_head(), log.rows_issued()),
        (1, 6, 6)
    );
    assert_eq!(
        log.rows_lost_between(4, 6),
        0,
        "both of the close's ids are held"
    );
}

/// NO ID IS REUSED ACROSS AN UPDATE: a carried update's successor and an
/// uncarried one's continue past the ceiling the host carried (the head
/// plus the overlap's reserve, which the outgoing process keeps under), the
/// carried records are no rows of the successor, and its first record is.
/// A crash adoption's ids start at 1 at its new epoch: as numbers they meet
/// the predecessor's, as (epoch, id) never, though both started in one wall
/// second. NEGATIVE CONTROL: a successor whose counter is not carried reuses
/// the predecessor's ids.
#[test]
fn no_row_id_is_reused_across_a_carried_update_or_a_crash_adoption() {
    let mut pred = Log::with_limits(4, Limits::SESSION);
    pred.keep_rows(grammar, 0);
    open_session(&mut pred, 1);
    cwd(&mut pred, 1, 2);
    status(&mut pred, "running", 3);
    bell(&mut pred, 4);
    let ceiling = pred.row_head() + RESERVE;
    // The overlap: the outgoing process writes on, under its ceiling.
    status(&mut pred, "idle", 5);
    assert!(pred.row_head() <= ceiling);
    let pred_ids = ids(&pred);
    assert_eq!(pred_ids, (1..=4).collect());
    let adopt = CarryAdopt {
        ep: pred.ep() + 1,
        next_seq: pred.last_seq() + 1_000,
        ids: pred.id_counters(),
        limits: Limits::SESSION,
        at: at(10),
    };
    let lines = pred
        .carry(CarryBudget::SESSION, &[])
        .expect("the carry fits")
        .lines;

    let (mut next, _, stats) = Log::from_carry(&lines, adopt).expect("the carry reads");
    assert!(stats.carried >= 4);
    next.keep_rows(grammar, ceiling);
    let rows: Vec<(u64, Kind)> = next.rows().map(|r| (r.id, r.rec.k)).collect();
    assert_eq!(
        rows,
        [(ceiling + 1, Kind::Handoff)],
        "the carried records are no rows; the successor's handoff mark is its first"
    );
    cwd(&mut next, 2, 11);
    assert_eq!(next.row_head(), ceiling + 2);
    assert!(ids(&next).is_disjoint(&pred_ids));
    assert_eq!(
        next.rows_lost_between(0, ceiling + 2),
        0,
        "no id below its own"
    );

    let mut gap = Log::after_handoff(
        pred.ep() + 1,
        Limits::SESSION,
        pred.id_counters(),
        pred.last_seq() + 1_000,
        at(10),
    );
    gap.keep_rows(grammar, ceiling);
    assert_eq!(
        gap.rows().map(|r| r.id).collect::<Vec<_>>(),
        [ceiling + 1],
        "an uncarried successor's gap is its first row, past the ceiling"
    );

    // NEGATIVE CONTROL: the counter not carried: the successor's first row
    // takes an id the predecessor's rows hold.
    let (mut uncounted, _, _) = Log::from_carry(&lines, adopt).expect("the carry reads");
    uncounted.keep_rows(grammar, 0);
    assert!(
        !ids(&uncounted).is_disjoint(&pred_ids),
        "CONTROL: a counter not carried reuses ids"
    );

    // A crash adoption, in the same wall second as the predecessor began:
    // its epoch (the keeper tag's, above every earlier one) names its ids.
    // The epoch is picked here; the host's own pick (`Epochs::crash`,
    // through the production adoption) is aterm-gui's
    // `a_keeper_recovery_names_its_rows_apart_by_epoch`.
    let mut crash = Log::with_limits(pred.ep() + 7, Limits::SESSION);
    crash.gap(GapWhy::Crash, 0, at(1));
    crash.keep_rows(grammar, 0);
    assert_eq!(crash.rows().map(|r| r.id).collect::<Vec<_>>(), [1]);
    let keyed = |log: &Log| -> BTreeSet<(u32, u64)> {
        ids(log).into_iter().map(|id| (log.ep(), id)).collect()
    };
    assert!(keyed(&pred).is_disjoint(&keyed(&crash)));
    assert!(
        !ids(&pred).is_disjoint(&ids(&crash)),
        "as bare numbers they meet: the epoch is what names a row"
    );
}

/// The count moves only forward: a lower count, or one past
/// [`SEQ_LIMIT`] (no host's), is none; a count continued past the head
/// skips the ids between, which a reader is told of as holes, and the ids
/// below the first are another process's, no loss of this log's (a host
/// tells an update's jump past them itself). Installing the grammar again
/// flags nothing twice.
#[test]
fn the_count_moves_only_forward() {
    let mut log = Log::with_limits(1, Limits::SESSION);
    log.keep_rows(grammar, 10);
    open_session(&mut log, 1);
    assert_eq!((log.row_first(), log.row_head()), (11, 11));
    log.continue_rows(5);
    log.continue_rows(SEQ_LIMIT + 1);
    assert_eq!(log.row_head(), 11);
    log.keep_rows(grammar, 0);
    assert_eq!(log.row_head(), 11, "nothing flagged twice");
    log.continue_rows(20);
    let c = cwd(&mut log, 1, 2);
    assert_eq!(ids_by_seq(&log).get(&c), Some(&(21, 1)));
    assert_eq!(
        log.rows_lost_between(11, 21),
        9,
        "the skipped ids are holes"
    );
    assert_eq!(
        log.rows_lost_between(0, 10),
        0,
        "below the first: another's"
    );

    // The last encodable id is usable; the next record has no row rather
    // than wrapping its id to zero or advancing the exhausted counter.
    let mut last = Log::with_limits(2, Limits::SESSION);
    last.keep_rows(grammar, SEQ_LIMIT - 2);
    open_session(&mut last, 1);
    assert_eq!(ids(&last), BTreeSet::from([SEQ_LIMIT - 1]));
    status(&mut last, "running", 2);
    assert_eq!(ids(&last), BTreeSet::from([SEQ_LIMIT - 1]));
    assert_eq!(last.row_head(), SEQ_LIMIT - 1);

    // A two-row close must fit as a whole: one remaining id cannot take
    // it. With two ids remaining, both rows still take those final ids.
    for (carried, expected) in [
        (SEQ_LIMIT - 3, vec![SEQ_LIMIT - 2]),
        (
            SEQ_LIMIT - 4,
            vec![SEQ_LIMIT - 3, SEQ_LIMIT - 2, SEQ_LIMIT - 1],
        ),
    ] {
        let mut close = Log::with_limits(3, Limits::SESSION);
        close.keep_rows(grammar, carried);
        open_session(&mut close, 1);
        close
            .close_session(
                Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact),
                at(2),
            )
            .expect("the record closes even when its rows do not fit");
        assert_eq!(
            ids(&close),
            expected.iter().copied().collect::<BTreeSet<_>>()
        );
        assert_eq!(close.row_head(), *expected.last().unwrap());
    }
}
