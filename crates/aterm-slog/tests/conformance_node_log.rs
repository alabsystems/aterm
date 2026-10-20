// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance (5J-1, ruling T27): what [`NodeLog::read`] tells a
//! reader, against the derived `SessionLogCursor` model
//! (`aterm_models_session::session_log_cursor_model`, the one
//! `conformance_cursor` holds the session ring to).
//!
//! A seeded walk drives a real two-record node log through four seqs: its
//! start, an amend of it, `link` and `config` marks, the oldest evicted
//! first. A reader pulls in between, to the head or cut at a live record.
//! After every step each seq's standing (live or evicted) and the reader's
//! tallies are projected onto the model (the node log has no hole and no
//! update: `fence`, `ov` and `ep` stay 0), and the step must be the model's:
//! an append followed by the eviction it made, or the read the reader made.
//! NEGATIVE CONTROL: a ring that drops a gap count (its read's gap thrown
//! away, or its count cut by one) is rejected.

use aterm_models_session::session_log_cursor_model;
use aterm_slog::{Actor, Conf, Cursor, Fields, GapWhy, Key, Kind, Mark, NodeLog, Read, Src};
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use aterm_spec::rng::XorShift;

use crate::support::at;

/// The model's seqs.
const SLOTS: u64 = 4;

/// The node log's epoch in these walks (any: the reader names the log's).
const EP: u32 = 1_760_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reader {
    cur: u64,
    got: u64,
    rlost: u64,
}

/// How a ring tells its reader: honestly, or a mutant that drops a count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ring {
    Honest,
    /// The read's gap thrown away.
    DropsGap,
    /// The read's gap one short.
    CutsCount,
}

struct World {
    log: NodeLog,
    reader: Reader,
    clock: u64,
}

impl World {
    fn new() -> World {
        World {
            log: NodeLog::bounded(EP, 2),
            reader: Reader::default(),
            clock: 0,
        }
    }

    /// Seq `i`'s standing: 0 not yet assigned, 1 live, 2 evicted.
    fn standing(&self, i: u64) -> i64 {
        if i > self.log.last_seq() {
            0
        } else if self.log.window().any(|r| r.seq == i) {
            1
        } else {
            2
        }
    }

    fn project(&self) -> State {
        project_reader(self, self.reader)
    }
}

fn project_reader(w: &World, r: Reader) -> State {
    let mut s: State = (1..=SLOTS)
        .map(|i| {
            (
                ["s1", "s2", "s3", "s4"][usize::try_from(i - 1).expect("small")],
                w.standing(i),
            )
        })
        .collect();
    s.insert("seq", w.log.last_seq() as i64);
    s.insert("cur", r.cur as i64);
    s.insert("got", r.got as i64);
    s.insert("rlost", r.rlost as i64);
    s.insert("told", 0);
    s.insert("ov", 0);
    s.insert("fence", 0);
    s.insert("ep", 0);
    s
}

/// The model reaches `next` from `prev` by `action` and then the evictions
/// in `evicted`, and every invariant holds there.
fn admitted(m: &Model, action: &str, evicted: &[u64], prev: &State, next: &State) {
    let mut frontier = m.successors(action, prev);
    for i in evicted {
        frontier = frontier
            .iter()
            .flat_map(|s| m.successors(&format!("Evict{i}"), s))
            .collect();
    }
    assert!(
        frontier.contains(next),
        "real `{action}` (evicting {evicted:?}) is not the model's:\n  from {prev:?}\n  to   {next:?}\n  model {frontier:?}"
    );
    for inv in &m.invariants {
        assert!(
            m.check_invariant(inv.name, next),
            "{} broken: {next:?}",
            inv.name
        );
    }
}

/// One real append (`start`, `amend`, `link` or `config`), returning the
/// seqs it evicted.
fn append(w: &mut World, what: &str) -> Vec<u64> {
    w.clock += 5;
    let stamp = at(w.clock);
    let live_before: Vec<u64> = w.log.window().map(|r| r.seq).collect();
    match what {
        "start" => {
            let f = Fields::new().word(Key::PrevEnd, "-").expect("a word");
            w.log.start(f, stamp).expect("the start is first");
        }
        "amend" => {
            let f = Fields::new()
                .word(Key::PrevEnd, "signal")
                .and_then(|f| f.i(Key::Code, 11))
                .expect("words");
            w.log.amend_start(f, stamp).expect("the start was written");
        }
        "link" => {
            let f = Fields::new()
                .word(Key::State, "connected")
                .and_then(|f| f.id(Key::Gen, "4"))
                .expect("words");
            let m = Mark::new(Kind::Link, Src::Fabric, Conf::Exact)
                .by(Actor::Bridge)
                .fields(f);
            w.log.mark(m, stamp).expect("a link mark");
        }
        _ => {
            let f = Fields::new()
                .id(Key::CfgKey, "privacy.enabled")
                .expect("a key name");
            let m = Mark::new(Kind::Config, Src::Aterm, Conf::Exact)
                .by(Actor::Aterm)
                .fields(f);
            w.log.mark(m, stamp).expect("a config mark");
        }
    }
    live_before
        .into_iter()
        .filter(|seq| !w.log.window().any(|r| r.seq == *seq))
        .collect()
}

/// What `ring` hands its reader of a real read.
fn told<'a>(ring: Ring, mut got: Read<'a>) -> Read<'a> {
    match ring {
        Ring::Honest => {}
        Ring::DropsGap => got.gap = None,
        Ring::CutsCount => {
            if let Some(g) = got.gap.as_mut() {
                g.lost = g.lost.saturating_sub(1);
            }
        }
    }
    got
}

/// One read through `ring`: to the head (`to = None`) or cut at the live
/// record `to`. Returns the model's action.
fn read(w: &mut World, ring: Ring, to: Option<u64>) -> &'static str {
    let c = w.reader.cur;
    let max = match to {
        None => usize::MAX,
        Some(k) => w.log.window().filter(|r| r.seq > c && r.seq <= k).count(),
    };
    let got = told(ring, w.log.read(Cursor::new(w.log.ep(), c), max));
    let next = got.next.seq;
    assert_eq!(next, to.unwrap_or(w.log.last_seq()), "where the read ends");
    assert_eq!(got.next.ep, EP, "the log's epoch");
    for r in &got.recs {
        assert!(r.seq > c && r.seq <= next, "a record outside the read");
    }
    if let Some(g) = got.gap {
        assert_eq!(g.why, GapWhy::Evicted, "a FIFO ring only evicts");
        assert_eq!(
            g.resync_to,
            got.recs.first().map_or(next, |r| r.seq - 1),
            "the records begin past resync_to"
        );
        w.reader.rlost += g.lost;
    }
    w.reader.got += got.recs.len() as u64;
    w.reader.cur = next;
    match to {
        None => "ReadAll",
        Some(1) => "ReadTo1",
        Some(2) => "ReadTo2",
        Some(3) => "ReadTo3",
        Some(_) => "ReadTo4",
    }
}

fn walk(m: &Model, seed: u64) -> std::collections::BTreeSet<&'static str> {
    let mut rng = XorShift::new(seed);
    let mut w = World::new();
    let mut ran = std::collections::BTreeSet::new();
    for _ in 0..16 {
        let prev = w.project();
        let can_append = w.log.last_seq() < SLOTS;
        if can_append && (w.log.last_seq() == 0 || rng.below(2) == 0) {
            let what = if w.log.last_seq() == 0 {
                "start"
            } else {
                ["amend", "link", "config"][rng.index(3)]
            };
            let evicted = append(&mut w, what);
            admitted(m, "Append", &evicted, &prev, &w.project());
            ran.insert("Append");
            if !evicted.is_empty() {
                ran.insert("(evicted)");
            }
        } else if w.log.last_seq() > w.reader.cur {
            let c = w.reader.cur;
            let live: Vec<u64> = w.log.window().map(|r| r.seq).filter(|s| *s > c).collect();
            let to = (!live.is_empty() && rng.below(2) == 0).then(|| live[rng.index(live.len())]);
            let action = read(&mut w, Ring::Honest, to);
            admitted(m, action, &[], &prev, &w.project());
            ran.insert(action);
        }
    }
    ran
}

#[test]
fn node_log_cursor_conforms_to_derived_model() {
    let m = session_log_cursor_model();
    let mut ran = std::collections::BTreeSet::new();
    for seed in 1..=300u64 {
        ran.extend(walk(&m, seed));
    }
    for a in [
        "Append",
        "(evicted)",
        "ReadAll",
        "ReadTo1",
        "ReadTo2",
        "ReadTo3",
        "ReadTo4",
    ] {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
}

/// NEGATIVE CONTROL: a ring that drops a gap count. Seqs 1..3 written to a
/// two-record ring (the start evicted), then one read from nothing: the
/// honest ring tells two records and one loss, and is the model's
/// `ReadAll`; a ring that throws the gap away, or cuts its count, leaves the
/// reader one seq short and is rejected by `NoSilentLoss`, and by the
/// model's `ReadAll`.
#[test]
fn node_log_conformance_rejects_a_ring_that_drops_a_gap_count() {
    let m = session_log_cursor_model();
    let mut w = World::new();
    for what in ["start", "link", "config"] {
        let prev = w.project();
        let evicted = append(&mut w, what);
        admitted(&m, "Append", &evicted, &prev, &w.project());
    }
    assert_eq!(
        (1..=3).map(|i| w.standing(i)).collect::<Vec<_>>(),
        vec![2, 1, 1],
        "the start evicted, the marks live"
    );
    let before = w.project();
    let start = (w.reader, w.log.clone());

    let _ = read(&mut w, Ring::Honest, None);
    admitted(&m, "ReadAll", &[], &before, &w.project());
    assert_eq!((w.reader.got, w.reader.rlost), (2, 1), "told of the start");

    for ring in [Ring::DropsGap, Ring::CutsCount] {
        let mut bad = World {
            log: start.1.clone(),
            reader: start.0,
            clock: w.clock,
        };
        let _ = read(&mut bad, ring, None);
        let s = bad.project();
        assert_eq!(bad.reader.rlost, 0, "{ring:?} told no loss");
        assert!(!m.check_invariant("NoSilentLoss", &s), "{ring:?}: {s:?}");
        assert!(
            !m.successors("ReadAll", &before).contains(&s),
            "{ring:?} is no read the model makes"
        );
    }
}
