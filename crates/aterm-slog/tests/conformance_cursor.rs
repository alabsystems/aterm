// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: [`Log::read`] against `aterm_models_session::session_log_cursor_model`.

use aterm_models_session::session_log_cursor_model;
use aterm_slog::{
    Close, Conf, Cursor, GapWhy, Kind, Limits, Log, Mark, Node, Open, Out, RunOut, Src,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use aterm_spec::rng::XorShift;

use crate::support::at;

const SLOTS: u64 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reader {
    cur: u64,
    got: u64,
    rlost: u64,
    told: bool,
}

struct World {
    log: Log,
    run: Option<Node>,
    reader: Reader,
    clock: u64,
    /// Active carry snapshot fencing outgoing readers.
    carry: Option<aterm_slog::CarrySnapshot>,
    /// Whether the update was committed (the successor's log).
    committed: bool,
}

impl World {
    fn new() -> World {
        World {
            log: Log::with_limits(0, Limits::new(2, 1 << 20).expect("a two-record ring")),
            run: None,
            reader: Reader::default(),
            clock: 0,
            carry: None,
            committed: false,
        }
    }

    /// Seq `i` state: 0 unassigned, 1 live, 2 evicted, 3 hole, 4 post-carry overlap.
    fn standing(&self, i: u64) -> i64 {
        if i > self.log.last_seq() {
            0
        } else if self.carry.as_ref().is_some_and(|c| i > c.last_seq()) {
            4
        } else if self.log.window().any(|r| r.seq == i) {
            1
        } else if self.log.holes().any(|(a, b)| a <= i && i <= b) {
            3
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
    s.insert("told", i64::from(r.told));
    s.insert("ov", 0);
    s.insert("fence", w.carry.as_ref().map_or(0, |c| c.last_seq() as i64));
    s.insert("ep", i64::from(w.committed));
    s
}

/// Assert `next` is reachable from `prev` via `action` followed by `evicted` steps.
fn admitted(m: &Model, action: &str, evicted: &[u64], prev: &State, next: &State) {
    admitted_chain(m, &[action], evicted, prev, next);
}

fn admitted_chain(m: &Model, actions: &[&str], evicted: &[u64], prev: &State, next: &State) {
    let mut frontier = vec![prev.clone()];
    for action in actions {
        frontier = frontier
            .iter()
            .flat_map(|s| m.successors(action, s))
            .collect();
    }
    let action = actions.join("+");
    for _ in evicted {
        let mut more = Vec::new();
        for s in &frontier {
            for i in evicted {
                for t in m.successors(&format!("Evict{i}"), s) {
                    if !more.contains(&t) {
                        more.push(t);
                    }
                }
            }
        }
        frontier = more;
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

fn append(w: &mut World, what: &str) -> (&'static str, Vec<u64>) {
    w.clock += 5;
    let stamp = at(w.clock);
    let live_before: Vec<u64> = w.log.window().map(|r| r.seq).collect();
    let action = match what {
        "hole" => {
            w.log.skip_to(w.log.last_seq() + 2);
            "Hole"
        }
        "open" => {
            w.run = Some(
                w.log
                    .open(Open::new(Kind::Prog, Src::Status, Conf::Strong), stamp)
                    .expect("a run opens"),
            );
            "Append"
        }
        "close" => {
            let run = w.run.take().expect("a run is open");
            w.log
                .close(
                    run,
                    Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
                    stamp,
                )
                .expect("the run closes");
            "Append"
        }
        _ => {
            w.log
                .mark(
                    Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(Node::SESSION),
                    stamp,
                )
                .expect("a mark");
            "Append"
        }
    };
    let evicted = live_before
        .into_iter()
        .filter(|seq| !w.log.window().any(|r| r.seq == *seq))
        .collect();
    (action, evicted)
}

/// Run an update step (`export`, `overlap`, `stand`, or `commit`), returning model actions and
/// evictions.
fn update(w: &mut World, what: &str) -> (Vec<&'static str>, Vec<u64>) {
    w.clock += 5;
    let stamp = at(w.clock);
    let live_before: Vec<u64> = w.log.window().map(|r| r.seq).collect();
    let actions = match what {
        "export" => {
            w.carry = Some(w.log.carry_snapshot());
            w.log.fence_reads();
            vec!["Export"]
        }
        "overlap" => {
            w.log
                .mark(
                    Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(Node::SESSION),
                    stamp,
                )
                .expect("a mark");
            vec!["OverlapAppend"]
        }
        "stand" => {
            let fence = w.carry.take().map_or(0, |c| c.last_seq());
            w.log.lift_read_fence();
            let gone: Vec<u64> = (fence + 1..=w.log.last_seq())
                .filter(|seq| !w.log.window().any(|r| r.seq == *seq))
                .collect();
            return (vec!["StandDown"], gone);
        }
        _ => {
            let snap = w.carry.take().expect("a carry stands");
            let out = snap
                .encode(aterm_slog::CarryBudget::SESSION, &[])
                .expect("carried");
            let a = aterm_slog::CarryAdopt {
                ep: w.log.ep() + 1,
                next_seq: w.log.last_seq() + 1,
                ids: w.log.id_counters(),
                limits: w.log.limits(),
                at: stamp,
            };
            w.log = Log::from_carry(&out.lines, a).expect("the carry reads").0;
            w.run = w.log.open_nodes().first().map(|e| e.node);
            w.committed = true;
            vec!["Commit", "Append"]
        }
    };
    let fenced = w.carry.as_ref().map(aterm_slog::CarrySnapshot::last_seq);
    let evicted = live_before
        .into_iter()
        .filter(|seq| fenced.is_none_or(|f| *seq <= f))
        .filter(|seq| {
            !w.log.window().any(|r| r.seq == *seq)
                && !w.log.holes().any(|(a, b)| a <= *seq && *seq <= b)
        })
        .collect();
    (actions, evicted)
}

fn read(w: &mut World, to: Option<u64>) -> &'static str {
    let c = w.reader.cur;
    let max = match to {
        None => usize::MAX,
        Some(k) => w.log.window().filter(|r| r.seq > c && r.seq <= k).count(),
    };
    let got = w.log.read(Cursor::new(w.log.ep(), c), max);
    let next = got.next.seq;
    assert_eq!(
        next,
        to.unwrap_or(w.log.reader_last_seq()),
        "where the read ends"
    );
    assert!(next <= w.log.reader_last_seq(), "never past the fence");
    for r in &got.recs {
        assert!(r.seq > c && r.seq <= next, "a record outside the read");
    }
    if let Some(g) = got.gap {
        assert_eq!(
            g.resync_to,
            got.recs.first().map_or(next, |r| r.seq - 1),
            "the records begin past resync_to"
        );
        w.reader.rlost += g.lost;
        w.reader.told |= g.why == GapWhy::Handoff;
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
    for _ in 0..24 {
        let prev = w.project();
        let can_append = w.log.last_seq() < SLOTS;
        let pick = rng.below(8);
        if pick >= 6 && !w.committed {
            let what = match (&w.carry, pick, can_append) {
                (None, _, _) if w.log.last_seq() > 0 => Some("export"),
                (Some(_), 6, true) => Some("overlap"),
                (Some(_), _, false) => Some("stand"),
                (Some(_), _, _) if rng.below(4) == 0 => Some("stand"),
                (Some(_), _, _) => Some("commit"),
                _ => None,
            };
            if let Some(what) = what {
                let (actions, evicted) = update(&mut w, what);
                admitted_chain(m, &actions, &evicted, &prev, &w.project());
                ran.extend(actions);
            }
        } else if can_append && pick < 4 && w.carry.is_none() {
            let what = match (pick, w.run.is_some()) {
                (0, _) => "hole",
                (1, false) => "open",
                (1, true) => "close",
                _ => "mark",
            };
            let (action, evicted) = append(&mut w, what);
            admitted(m, action, &evicted, &prev, &w.project());
            ran.insert(action);
            if !evicted.is_empty() {
                ran.insert("(evicted)");
            }
        } else if w.log.reader_last_seq() > w.reader.cur {
            let (c, head) = (w.reader.cur, w.log.reader_last_seq());
            let live: Vec<u64> = w
                .log
                .window()
                .map(|r| r.seq)
                .filter(|s| *s > c && *s <= head)
                .collect();
            let to = (!live.is_empty() && rng.below(2) == 0).then(|| live[rng.index(live.len())]);
            let action = read(&mut w, to);
            admitted(m, action, &[], &prev, &w.project());
            ran.insert(action);
        }
    }
    ran
}

#[test]
fn slog_cursor_conforms_to_derived_model() {
    let m = session_log_cursor_model();
    let mut ran = std::collections::BTreeSet::new();
    for seed in 1..=300u64 {
        ran.extend(walk(&m, seed));
    }
    for a in [
        "Append",
        "Hole",
        "(evicted)",
        "ReadAll",
        "ReadTo1",
        "ReadTo2",
        "ReadTo3",
        "ReadTo4",
        "Export",
        "OverlapAppend",
        "Commit",
        "StandDown",
    ] {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
}

/// Negative controls: counting a hole as loss or dropping a mid-window eviction is rejected.
#[test]
fn slog_cursor_conformance_rejects_what_a_read_never_says() {
    let m = session_log_cursor_model();
    let mut w = World::new();
    for what in ["open", "mark"] {
        let prev = w.project();
        let (action, evicted) = append(&mut w, what);
        admitted(&m, action, &evicted, &prev, &w.project());
    }
    let prev = w.project();
    let (action, evicted) = append(&mut w, "hole");
    admitted(&m, action, &evicted, &prev, &w.project());
    let prev = w.project();
    let (action, evicted) = append(&mut w, "mark");
    assert_eq!(evicted, vec![2], "the batch took the mark in the middle");
    admitted(&m, action, &evicted, &prev, &w.project());
    assert_eq!(
        (1..=4).map(|i| w.standing(i)).collect::<Vec<_>>(),
        vec![1, 2, 3, 1],
        "live, evicted, never held, live"
    );

    let before = w.project();
    let honest = w.reader;
    let _ = read(&mut w, None);
    let read_to_head = w.project();
    admitted(&m, "ReadAll", &[], &before, &read_to_head);
    assert_eq!(
        (w.reader.got, w.reader.rlost, w.reader.told),
        (2, 1, true),
        "two records, the middle eviction counted, the hole said, not counted"
    );

    let mut counted = w.reader;
    counted.rlost += 1;
    let bad = project_reader(&w, counted);
    assert!(!m.check_invariant("HoleNeverCountsAsLoss", &bad));
    assert!(!m.successors("ReadAll", &before).contains(&bad));

    let mut blind = w.reader;
    blind.rlost -= 1;
    let bad = project_reader(&w, blind);
    assert!(!m.check_invariant("NoSilentLoss", &bad));
    assert!(!m.successors("ReadAll", &before).contains(&bad));

    let mut deaf = w.reader;
    deaf.told = honest.told;
    let bad = project_reader(&w, deaf);
    assert!(!m.check_invariant("HandoffSaid", &bad));

    let mut f = World::new();
    for what in ["open", "mark"] {
        let prev = f.project();
        let (action, evicted) = append(&mut f, what);
        admitted(&m, action, &evicted, &prev, &f.project());
    }
    let (actions, _) = update(&mut f, "export");
    assert_eq!(actions, vec!["Export"]);
    let (actions, _) = update(&mut f, "overlap");
    assert_eq!(actions, vec!["OverlapAppend"]);
    let before = f.project();
    let _ = read(&mut f, None);
    let fenced = f.project();
    admitted(&m, "ReadAll", &[], &before, &fenced);
    assert_eq!(f.reader.cur, 2, "the read stopped at the fence");
    let mut shown = fenced.clone();
    shown.insert("cur", 3);
    shown.insert("got", fenced["got"] + 1);
    shown.insert("ov", 1);
    assert!(!m.check_invariant("ReaderNeverSeesOverlap", &shown));
    assert!(!m.successors("ReadAll", &before).contains(&shown));
}
