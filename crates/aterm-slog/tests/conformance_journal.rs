// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the journal reader's view
//! ([`aterm_slog::journal::view_of`]) against the derived `SessionLogJournal`
//! model (`aterm_models_session::session_log_journal_model`, checked at Tier-0
//! in `aterm-models/tests/derived_ring_ty.rs`).
//!
//! A seeded walk picks the model's enabled actions and does each for real: a
//! session's open, a run's under it and a turn's under the run are appended
//! to a real [`Log`] (`Append`), and the writer, behind it, journals the
//! oldest it has not reached through the real [`take`] and [`take_frames`]
//! (`Write`), or says it lost by a gap frame (`Lose`), or journals a line
//! again (`Again<i>`); `Snap` writes a snap at the writer's cursor through
//! the real take, behind the log or not; `Hand` writes the session's
//! `#handed`; `Crash` makes the chain's writer dead with no `#fin`; `Read`
//! decodes the chain's bytes and builds the view. Every step's projection
//! must be the model's successor, and every law must hold. NEGATIVE
//! CONTROLS: a reader without the dedupe, without the crash closes, blind to
//! losses, one whose crash closes run parent first, one that keeps a
//! `#handed` a later snap voided, and the writer and reader before phase
//! 2B's fixes (a snap behind restating the open path past its head, counted
//! toward the head) are each rejected by the model, by the law they break.

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

/// How a walk's reader and writer differ from the shipping ones (each a
/// negative control).
#[derive(Clone, Copy)]
struct Mutant {
    rules: ViewRules,
    /// The reader's closes reordered parent first.
    reverse_closes: bool,
    /// The writer before phase 2B's fix: a snap restates the open path as
    /// the log stands, past the snap's head.
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

    /// Append slot `lg + 1`'s record to the log: the session's open, a run
    /// under it, a turn under the run.
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
            // NEGATIVE CONTROL: the same closes, parent first.
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
        // The crash gap's `after=`.
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
                // The writer's own take of the one record it reaches next.
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
                // The writer's snap at its cursor, the log as it stands.
                let tk = take(&self.log, self.nxt, 0, true);
                let frames = if m.present_snap {
                    // NEGATIVE CONTROL: the open path as the log stands,
                    // past the snap's head.
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

/// One seeded walk: `Ok(actions)` when every step was the model's and every
/// law held, `Err((why, state))` at the first step that was not, with the
/// state the real step reached.
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
        // Read often, so every state is looked at.
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
    for seed in 1..=2000u64 {
        match walk(&m, seed, SHIPPING) {
            Ok(r) => ran.extend(r),
            Err((why, _)) => panic!("{why}"),
        }
    }
    for a in ACTIONS {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
}

/// NEGATIVE CONTROLS: each reader (or writer and reader) that breaks one law
/// is refused on some walk, by the law it breaks, and only that law.
#[test]
fn slog_journal_view_conformance_rejects_each_broken_reader() {
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
            .collect();
        assert!(
            !refused.is_empty(),
            "the reader with {name} was never refused"
        );
        // Refused because the state its read reached breaks the law, and
        // only that law.
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

/// The shipping writer's snap behind the log never restates past its head:
/// the walk's own check on the frames, beside the model's law on the view.
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
    // NEGATIVE CONTROL: the open path as the log stands names seqs 2 and 3.
    let present: Vec<u64> = tk.opens.unwrap_or_default().iter().map(|r| r.seq).collect();
    assert_eq!(present, vec![1, 2, 3]);
}
