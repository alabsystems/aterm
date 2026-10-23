// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests checking [`aterm_slog::journal::view_of`] and [`aterm_slog::journal::take_frames`] against [`aterm_models_session::session_log_journal_model`].
//! Seeded walks drive real journal writes, snaps, handoffs, crashes, and reads, with negative-control mutants verifying each invariant.

use aterm_models_session::session_log_journal_model;
use aterm_slog::journal::{
    ChainRead, FORMAT, Frame, Inst, Liveness, LossWhy, SegmentRead, ViewRules, complete_lines,
    decode_line, encode_frame, take, take_frames, view_of,
};
use aterm_slog::{
    Actor, Conf, Cursor, Fields, GapWhy, Key, Kind, Log, Node, Op, Open, Rec, ShortId, Src, Val,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use aterm_spec::rng::XorShift;

use crate::support::at;

fn sid() -> ShortId {
    ShortId::new("s-conform").expect("a sid")
}

/// Negative-control mutations for the journal reader and writer.
#[derive(Clone, Copy)]
struct Mutant {
    rules: ViewRules,
    /// The reader's closes reordered parent first.
    reverse_closes: bool,
    /// Restates the open path as the log currently stands rather than at the snap's head.
    present_snap: bool,
}

const SHIPPING: Mutant = Mutant {
    rules: ViewRules::READER,
    reverse_closes: false,
    present_snap: false,
};

struct World {
    log: Log,
    bytes: Vec<u8>,
    lg: u64,
    nxt: u64,
    j: [i64; 3],
    l: [i64; 3],
    dead: bool,
    hd: bool,
    sd: bool,
    sb: u64,
    nodes: [Node; 3],
    view: State,
}

const VIEW_VARS: [&str; 12] = [
    "rd", "v1", "v2", "v3", "vlost", "vs", "vr", "vt", "cs", "cr", "ct", "ca",
];

impl World {
    fn new() -> World {
        let mut w = World {
            log: Log::new(1),
            bytes: Vec::new(),
            lg: 0,
            nxt: 0,
            j: [0; 3],
            l: [0; 3],
            dead: false,
            hd: false,
            sd: false,
            sb: 0,
            nodes: [Node::SESSION; 3],
            view: VIEW_VARS.iter().map(|v| (*v, 0)).collect(),
        };
        w.line(&Frame::Seg {
            v: FORMAT,
            inst: inst(),
            n: 1,
            t: 1,
            build: "conform".to_string(),
        });
        w.line(&Frame::Snap {
            sid: sid(),
            head: 0,
            ep: 1,
            base: 1,
            first: 1,
            open: 0,
            t: 1,
        });
        w
    }

    fn line(&mut self, f: &Frame) {
        self.bytes
            .extend_from_slice(encode_frame(f).expect("encodes").as_bytes());
        self.bytes.push(b'\n');
    }

    /// Appends slot `lg + 1`'s record (session open, run, or turn) to the log.
    fn append(&mut self) {
        let ms = 10 * (self.lg + 1);
        match self.lg {
            0 => {
                self.log
                    .open_session(
                        Kind::Spawn,
                        Src::Aterm,
                        Conf::Exact,
                        Actor::Nobody,
                        Fields::new(),
                        at(ms),
                    )
                    .expect("the session opens");
            }
            1 => {
                self.nodes[1] = self
                    .log
                    .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
                    .expect("a run");
            }
            _ => {
                self.nodes[2] = self
                    .log
                    .open(
                        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(self.nodes[1]),
                        at(ms),
                    )
                    .expect("a turn");
            }
        }
        self.lg += 1;
    }

    fn rec_of(&self, slot: usize) -> Rec {
        self.log
            .window()
            .find(|r| r.seq == slot as u64 + 1)
            .expect("the record")
            .clone()
    }

    fn chain(&self) -> ChainRead {
        ChainRead {
            inst: inst(),
            segments: vec![SegmentRead {
                n: 1,
                frames: complete_lines(&self.bytes)
                    .filter_map(|l| decode_line(l).ok())
                    .collect(),
            }],
            ended: false,
            live: if self.dead {
                Liveness::Dead
            } else {
                Liveness::Live
            },
            start_t: 1,
        }
    }

    fn project(&self) -> State {
        let mut s = self.view.clone();
        s.insert("nxt", self.nxt as i64);
        s.insert("lg", self.lg as i64);
        for i in 0..3 {
            s.insert(["j1", "j2", "j3"][i], self.j[i]);
        }
        s.insert("l2", self.l[1]);
        s.insert("l3", self.l[2]);
        s.insert("dead", i64::from(self.dead));
        s.insert("hd", i64::from(self.hd));
        s.insert("sd", i64::from(self.sd));
        s.insert("sb", self.sb as i64);
        s
    }

    /// The view's variables from a read of it.
    fn read(&mut self, m: Mutant) {
        let v = view_of(&sid(), &[self.chain()], false, m.rules).expect("a view");
        let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
        let mut recs: Vec<Rec> = read.recs.iter().map(|r| (*r).clone()).collect();
        if m.reverse_closes {
            let at: Vec<usize> = (0..recs.len())
                .filter(|i| recs[*i].op == Op::Close)
                .collect();
            let closes: Vec<Rec> = at.iter().map(|i| recs[*i].clone()).collect();
            for (i, r) in at.iter().zip(closes.into_iter().rev()) {
                recs[*i] = r;
            }
        }
        let opened = |slot: usize| {
            let node = self.nodes[slot];
            let present = self.j[slot] > 0;
            (present || slot == 0, node)
        };
        let count = |slot: usize| {
            recs.iter()
                .filter(|r| r.op == Op::Open && (self.j[slot] > 0) && r.node == self.nodes[slot])
                .count() as i64
        };
        for (i, name) in ["v1", "v2", "v3"].iter().enumerate() {
            self.view.insert(name, count(i));
        }
        self.view
            .insert("vlost", read.gap.map_or(0, |g| g.lost) as i64);
        let is_open = |slot: usize| {
            let (present, node) = opened(slot);
            i64::from(present && v.log.is_open(node))
        };
        self.view.insert("vs", is_open(0));
        self.view.insert("vr", is_open(1));
        self.view.insert("vt", is_open(2));
        let closes: Vec<Node> = recs
            .iter()
            .filter(|r| r.op == Op::Close)
            .map(|r| r.node)
            .collect();
        let place = |slot: usize| {
            let (present, node) = opened(slot);
            if !present {
                return 0;
            }
            closes
                .iter()
                .position(|n| *n == node)
                .map_or(0, |p| p as i64 + 1)
        };
        self.view.insert("cs", place(0));
        self.view.insert("cr", place(1));
        self.view.insert("ct", place(2));
        let after = recs
            .iter()
            .find(|r| {
                r.op == Op::Gap
                    && matches!(r.f.get(Key::Why), Some(Val::Word(w)) if *w == GapWhy::Crash.word())
            })
            .and_then(|r| match r.f.get(Key::After) {
                Some(Val::U(n)) => Some(*n as i64),
                _ => None,
            })
            .unwrap_or(0);
        self.view.insert("ca", after);
        self.view.insert("rd", 1);
    }

    /// The journal changed: any earlier view is stale.
    fn stale(&mut self) {
        for v in VIEW_VARS {
            self.view.insert(v, 0);
        }
    }

    /// A line of the session's after a `#handed` voids it.
    fn wrote_session_line(&mut self) {
        self.hd = false;
        self.sd = false;
        self.stale();
    }

    /// Do `action` for real.
    fn act(&mut self, action: &str, m: Mutant) {
        match action {
            "Append" => self.append(),
            "Write" => {
                let tk = take(&self.log, self.nxt, 1, false);
                assert_eq!(tk.recs.len(), 1, "one record past the cursor");
                for f in take_frames(sid(), &tk, false, 1) {
                    self.line(&f);
                }
                self.j[self.nxt as usize] = 1;
                self.nxt = tk.next;
                self.wrote_session_line();
            }
            "Lose" => {
                self.l[self.nxt as usize] = 1;
                let n = self.nxt + 1;
                self.line(&Frame::Gap {
                    sid: sid(),
                    why: LossWhy::Evicted,
                    lost: 1,
                    after: n - 1,
                    through: n,
                });
                self.nxt = n;
                self.wrote_session_line();
            }
            "Again1" | "Again2" | "Again3" => {
                let slot = usize::from(action.as_bytes()[5] - b'1');
                let r = self.rec_of(slot);
                self.j[slot] = 2;
                self.line(&Frame::Rec {
                    sid: sid(),
                    rec: r,
                    restated: false,
                });
                self.wrote_session_line();
            }
            "Snap" => {
                let tk = take(&self.log, self.nxt, 0, true);
                let frames = if m.present_snap {
                    let opens = tk.opens.clone().unwrap_or_default();
                    let mut f = vec![Frame::Snap {
                        sid: sid(),
                        head: tk.cursor,
                        ep: tk.ep,
                        base: tk.base,
                        first: tk.first,
                        open: opens.len() as u32,
                        t: 1,
                    }];
                    f.extend(opens.into_iter().map(|rec| Frame::Rec {
                        sid: sid(),
                        rec,
                        restated: true,
                    }));
                    f
                } else {
                    take_frames(sid(), &tk, true, 1)
                };
                for f in &frames {
                    self.line(f);
                }
                if self.hd {
                    self.sd = true;
                }
                self.hd = false;
                if self.lg > self.nxt {
                    self.sb = self.lg;
                }
                self.stale();
            }
            "Hand" => {
                let fence = self.nxt;
                self.line(&Frame::Handed { sid: sid(), fence });
                self.hd = true;
                self.stale();
            }
            "Crash" => {
                self.dead = true;
                self.stale();
            }
            _ => self.read(m),
        }
    }
}

fn inst() -> Inst {
    Inst::new("p1-c0ffee").expect("an instance")
}

const ACTIONS: [&str; 10] = [
    "Append", "Write", "Lose", "Again1", "Again2", "Again3", "Snap", "Hand", "Crash", "Read",
];

/// Runs a seeded walk and returns `Ok(actions)` or `Err((why, state))` on the first transition or invariant mismatch.
fn walk(m: &Model, seed: u64, mutant: Mutant) -> Result<Vec<&'static str>, (String, State)> {
    let mut rng = XorShift::new(seed);
    let mut w = World::new();
    let mut ran = Vec::new();
    for _ in 0..24 {
        let prev = w.project();
        let enabled: Vec<&'static str> = ACTIONS
            .iter()
            .copied()
            .filter(|a| m.action_enabled(a, &prev))
            .collect();
        if enabled.is_empty() {
            break;
        }
        let action = if rng.below(4) == 0 {
            "Read"
        } else {
            *rng.pick(&enabled)
        };
        if !m.action_enabled(action, &prev) {
            continue;
        }
        w.act(action, mutant);
        let next = w.project();
        let model_next = m.successors(action, &prev);
        if !model_next.contains(&next) {
            return Err((
                format!(
                    "seed {seed}: real `{action}` is not the model's:\n  from {prev:?}\n  to   {next:?}\n  model {model_next:?}"
                ),
                next,
            ));
        }
        for inv in &m.invariants {
            if !m.check_invariant(inv.name, &next) {
                return Err((
                    format!("seed {seed}: {} broken by `{action}`: {next:?}", inv.name),
                    next,
                ));
            }
        }
        ran.push(action);
    }
    Ok(ran)
}

#[test]
fn slog_journal_view_conforms_to_derived_model() {
    let m = session_log_journal_model();
    let mut ran = std::collections::BTreeSet::new();
    let seeds: Vec<u64> = (1..=2000).collect();
    for walked in aterm_spec::check::each(&seeds, |&seed| walk(&m, seed, SHIPPING)) {
        match walked {
            Ok(r) => ran.extend(r),
            Err((why, _)) => panic!("{why}"),
        }
    }
    for a in ACTIONS {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
}

/// Each negative-control mutant is rejected on seeded walks by the specific invariant it violates.
#[test]
fn slog_journal_view_conformance_rejects_each_broken_reader() {
    const REFUSALS: usize = 8;
    let m = session_log_journal_model();
    let rules = |r: ViewRules| Mutant {
        rules: r,
        ..SHIPPING
    };
    for (name, mutant, law) in [
        (
            "no dedupe",
            rules(ViewRules {
                dedupe: false,
                ..ViewRules::READER
            }),
            "ViewAtMostOnce",
        ),
        (
            "no crash closes",
            rules(ViewRules {
                crash_closes: false,
                ..ViewRules::READER
            }),
            "DeadViewClosed",
        ),
        (
            "a #handed a snap voided kept",
            rules(ViewRules {
                snap_voids_handed: false,
                ..ViewRules::READER
            }),
            "DeadViewClosed",
        ),
        (
            "blind to losses",
            rules(ViewRules {
                say_losses: false,
                ..ViewRules::READER
            }),
            "LossSaid",
        ),
        (
            "parent first",
            Mutant {
                reverse_closes: true,
                ..SHIPPING
            },
            "ChildClosesFirst",
        ),
        (
            "a snap past its head, counted",
            Mutant {
                rules: ViewRules {
                    restated_heads: false,
                    ..ViewRules::READER
                },
                present_snap: true,
                ..SHIPPING
            },
            "CrashAfterHead",
        ),
    ] {
        let refused: Vec<(String, State)> = (1..=2000u64)
            .filter_map(|seed| walk(&m, seed, mutant).err())
            .take(REFUSALS)
            .collect();
        assert!(
            !refused.is_empty(),
            "the reader with {name} was never refused"
        );
        for (why, state) in &refused {
            assert!(!m.check_invariant(law, state), "{name}: {why}");
            for inv in &m.invariants {
                if inv.name != law {
                    assert!(
                        m.check_invariant(inv.name, state),
                        "{name} also broke {}: {why}",
                        inv.name
                    );
                }
            }
        }
    }
}

/// A snap written behind the log restates only records at or below its `head`.
#[test]
fn a_snap_written_behind_restates_only_what_its_head_accounts_for() {
    let mut w = World::new();
    for _ in 0..3 {
        w.append();
    }
    w.act("Write", SHIPPING);
    let tk = take(&w.log, w.nxt, 0, true);
    let frames = take_frames(sid(), &tk, true, 1);
    let Some(Frame::Snap { head, open, .. }) = frames.first() else {
        panic!("a snap first: {frames:?}");
    };
    let restated: Vec<u64> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Rec {
                rec,
                restated: true,
                ..
            } => Some(rec.seq),
            _ => None,
        })
        .collect();
    assert_eq!(*head, 1);
    assert_eq!(restated, vec![1], "only the session's open, at or below 1");
    assert_eq!(*open as usize, restated.len());
    let present: Vec<u64> = tk.opens.unwrap_or_default().iter().map(|r| r.seq).collect();
    assert_eq!(present, vec![1, 2, 3]);
}
