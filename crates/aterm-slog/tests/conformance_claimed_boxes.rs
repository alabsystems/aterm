// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: what [`TurnTracker::claim_box`] makes of two serial boxes of one kind a
//! codex-alab TUI shows in one turn, beside a person's, a ctl and the harness's input, codex-alab's
//! own acts, a screen stall, the turn's rest and the program's going, against the derived `ClaimedBoxSteps`
//! model (`aterm_models_session::claimed_box_steps_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`; GAP 26, Stage A).

use std::collections::BTreeSet;

use aterm_models_session::claimed_box_steps_model;
use aterm_slog::{
    ActWord, Actor, Binding, Claimed, ClaimedBox, ClaimedReport, Close, Conf, Fields, Key, Kind,
    Log, Mark, Node, Op, Out, Src, StepOut, TurnTracker, Verdict,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::{agent_session, at, lines, time_order_holds};

/// The model's honest steps the walk drives: each is a real call [`World::step`] makes (or a
/// fact of the TUI it keeps).
const STEPS: [&str; 16] = [
    "Show1",
    "Show2",
    "Refresh",
    "PersonAnswer1",
    "AlabAnswer1",
    "PersonAnswer2",
    "AlabAnswer2",
    "PersonKey",
    "CtlKey",
    "HarnessKey",
    "PreAct",
    "Sweep",
    "TurnEnd",
    "Gone",
    "Stall",
    "StallEnd",
];

/// The began of each report: none shown, box 1's, box 2's (when a report came between).
const BEGAN: [u64; 3] = [11, 7, 9];

/// The host marking codex-alab's acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    /// A stand-in for the shipping host's placement of an act (`SessionLog::act_place_beside`,
    /// D4): an act is marked off the claimed box and closes nothing.
    Ships,
    /// The original D4, for the negative control: an unboxed act (a pre-approval) closes the
    /// standing box `out=approved by=ctl`.
    ClosesOnActs,
}

/// The real tracker and its log, the TUI the model's ghosts are, and what the host saw.
struct World {
    log: Log,
    tr: TurnTracker,
    turn: Node,
    ms: u64,
    tui: [i64; 2],
    gap: i64,
    rep: i64,
    seen: i64,
    saw: [i64; 2],
    pre: i64,
    stall: i64,
    /// Each claimed box step the tracker opened, and the began (0 box 1's, 1 box 2's) it opened on.
    steps: Vec<(Node, usize)>,
    /// Each input that bound a step, as the model words it (1 a person, 2 ctl, 3 the harness's
    /// approve).
    keys: Vec<(Node, i64)>,
}

impl World {
    fn new() -> World {
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        let mut tr = TurnTracker::new(run);
        let _ = tr.claim(&mut log, Claimed::Working, at(1));
        let turn = tr.turn().expect("the claim's turn");
        World {
            log,
            tr,
            turn,
            ms: 1,
            tui: [0; 2],
            gap: 0,
            rep: 0,
            seen: 0,
            saw: [0; 2],
            pre: 0,
            stall: 0,
            steps: Vec::new(),
            keys: Vec::new(),
        }
    }

    /// What the TUI's report shows now (the model's `Refresh` target).
    const fn showing(&self) -> i64 {
        if self.tui[0] == 1 {
            1
        } else if self.tui[1] == 1 && self.gap == 1 {
            2
        } else if self.tui[1] == 1 {
            1
        } else {
            0
        }
    }

    /// codex-alab's act, marked as the host marks it: off the claimed box, as `self_claim_act`
    /// places it.
    fn act_mark(&mut self) {
        let place = self.tr.claim_node().and_then(|c| {
            self.log
                .open_nodes()
                .iter()
                .rev()
                .find(|e| e.node != c)
                .map(|e| e.node)
        });
        let f = Fields::new().word(Key::Word, "approve").expect("the word");
        let mut mark = Mark::new(Kind::Act, Src::Mark, Conf::Claim)
            .by(Actor::Ctl)
            .fields(f);
        if let Some(n) = place {
            mark = mark.at(n);
        }
        let _ = self.log.mark(mark, at(self.ms));
    }

    /// The real call for the model's `step` under `host`, one ms past the last, and the model
    /// action it is.
    fn step(&mut self, step: &'static str, host: Host) -> &'static str {
        self.ms += 1;
        let now = at(self.ms);
        match step {
            "Show1" => self.tui[0] = 1,
            "Show2" => {
                self.tui[1] = 1;
                self.gap = i64::from(self.rep == 0);
            }
            "Refresh" => self.rep = self.showing(),
            "PersonAnswer1" | "PersonAnswer2" | "AlabAnswer1" | "AlabAnswer2" => {
                let k = usize::from(step.ends_with('2'));
                self.tui[k] = 2;
                if step.starts_with("Alab") {
                    self.act_mark();
                }
            }
            "PersonKey" => match self.tr.person(&self.log, now) {
                Binding::Step(n) => self.keys.push((n, 1)),
                other => panic!("a person's key bound {other:?}"),
            },
            "CtlKey" => {
                let adm = self.tr.admit(now);
                match self
                    .tr
                    .accept(&self.log, Actor::Ctl, Some(ActWord::Approve), adm, now)
                {
                    Binding::Step(n) => self.keys.push((n, 2)),
                    other => panic!("a ctl input bound {other:?}"),
                }
            }
            "HarnessKey" => {
                let adm = self.tr.admit(now);
                match self
                    .tr
                    .accept(&self.log, Actor::Harness, Some(ActWord::Approve), adm, now)
                {
                    Binding::Step(n) => self.keys.push((n, 3)),
                    other => panic!("the harness's approve bound {other:?}"),
                }
            }
            "PreAct" => {
                self.pre += 1;
                if host == Host::ClosesOnActs
                    && let Some(c) = self.tr.claim_node()
                {
                    let close = Close::new(Out::Step(StepOut::Approved), Src::Engine, Conf::Claim)
                        .by(Actor::Ctl);
                    let _ = self.log.close(c, close, now);
                    let _ = self.tr.tick(&mut self.log, now);
                    return "PreActCloses";
                }
                self.act_mark();
            }
            "Sweep" => {
                let rep = usize::try_from(self.rep).expect("a report");
                let report = ClaimedReport {
                    boxed: (rep > 0).then_some(ClaimedBox::Approval),
                    began: BEGAN[rep],
                };
                let _ = self.tr.claim(&mut self.log, Claimed::Working, now);
                let _ = self.tr.claim_box(&mut self.log, report, now);
                if let Some((n, ..)) = self.tr.claim_view().held
                    && !self.steps.iter().any(|(s, _)| *s == n)
                {
                    self.steps.push((n, rep.saturating_sub(1)));
                }
                self.seen = self.rep;
                if rep > 0 {
                    self.saw[rep - 1] = 1;
                }
            }
            "TurnEnd" => {
                let _ = self.tr.claim(&mut self.log, Claimed::Rested, now);
                let rested = ClaimedReport {
                    boxed: None,
                    began: 13,
                };
                let _ = self.tr.claim_box(&mut self.log, rested, now);
                self.seen = 0;
            }
            "Gone" => {
                let _ = self.tr.observe(&mut self.log, Verdict::Gone, None, now);
            }
            "Stall" => {
                self.stall = 1;
                let _ = self
                    .tr
                    .observe(&mut self.log, Verdict::Unresponsive, None, now);
            }
            "StallEnd" => {
                self.stall = 2;
                let _ = self.tr.observe(&mut self.log, Verdict::Unknown, None, now);
            }
            other => panic!("no step `{other}`"),
        }
        step
    }

    /// The close record of `node`, if it closed.
    fn close_of(&self, node: Node) -> Option<&aterm_slog::Rec> {
        self.log
            .window()
            .find(|r| r.op == Op::Close && r.node == node)
    }

    /// The model's state: each claimed box step the log holds (`src=engine conf=claim`) by the
    /// began it opened on; a second step on one began, or one the host never saw open, projects
    /// as 3, which the model never holds.
    fn project(&self) -> State {
        let mut s = State::new();
        let ghosts = [
            ("tui1", self.tui[0]),
            ("tui2", self.tui[1]),
            ("gap", self.gap),
            ("rep", self.rep),
            ("seen", self.seen),
            ("saw1", self.saw[0]),
            ("saw2", self.saw[1]),
            ("pre", self.pre),
            ("stall", self.stall),
        ];
        for (k, v) in ghosts {
            s.insert(k, v);
        }
        let turn = match self.close_of(self.turn) {
            None => 1,
            Some(c) if c.out() == Some("gone") => 3,
            Some(_) => 2,
        };
        s.insert("turn", turn);
        let mut slots: [Vec<Node>; 2] = Default::default();
        for r in self.log.window().filter(|r| {
            r.op == Op::Open
                && matches!(r.k, Kind::Approval | Kind::Question)
                && (r.src, r.conf) == (Src::Engine, Conf::Claim)
        }) {
            match self.steps.iter().find(|(n, _)| *n == r.node) {
                Some((_, j)) => slots[*j].push(r.node),
                None => slots[0].extend([r.node, r.node]),
            }
        }
        let held = self.tr.claim_view().held.map(|(n, ..)| n);
        s.insert("held", 0);
        for (j, nodes) in slots.iter().enumerate() {
            let (st, out, by, key) = match nodes.as_slice() {
                [] => (0, 0, 0, 0),
                [n] => {
                    if held == Some(*n) {
                        s.insert("held", i64::try_from(j + 1).expect("1 or 2"));
                    }
                    let key = self
                        .keys
                        .iter()
                        .rev()
                        .find(|(k, _)| k == n)
                        .map_or(0, |(_, w)| *w);
                    match self.close_of(*n) {
                        None => (1, 0, 0, key),
                        Some(c) => {
                            let out = match c.out() {
                                Some("approved") => 1,
                                Some("passed") => 2,
                                Some("gone") => 3,
                                _ => 9,
                            };
                            let by = match c.by {
                                Actor::Nobody => 0,
                                Actor::Human => 1,
                                Actor::Ctl => 2,
                                Actor::Harness => 3,
                                _ => 9,
                            };
                            (2, out, by, key)
                        }
                    }
                }
                _ => (3, 0, 0, 0),
            };
            s.insert(["s1", "s2"][j], st);
            s.insert(["o1", "o2"][j], out);
            s.insert(["by1", "by2"][j], by);
            s.insert(["k1", "k2"][j], key);
        }
        s
    }
}

/// One walk's real steps, in order, as (the model action, the state before, the state after),
/// and the laws its last step broke (none on a walk that ran to the end).
struct Walked {
    steps: Vec<(&'static str, State, State)>,
    broken: Vec<&'static str>,
}

/// Drives `host` from a fresh world through `m`'s enabled steps, each picked by `choose` from
/// the steps enabled (`None` ends the walk), until it stops or a law breaks: each real step must
/// be the model's. `what` names the walk in a failure.
fn walk(
    m: &Model,
    host: Host,
    what: &str,
    mut choose: impl FnMut(&[&'static str]) -> Result<Option<&'static str>, String>,
) -> Result<Walked, String> {
    let mut w = World::new();
    let mut walked = Walked {
        steps: Vec::new(),
        broken: Vec::new(),
    };
    loop {
        let prev = w.project();
        let open: Vec<&'static str> = STEPS
            .into_iter()
            .filter(|a| m.action_enabled(a, &prev))
            .collect();
        let Some(step) = choose(&open).map_err(|e| format!("{what}: {e}"))? else {
            break;
        };
        let action = w.step(step, host);
        let next = w.project();
        if !m.successors(action, &prev).contains(&next) {
            return Err(format!(
                "{what}: real `{step}` is not the model's `{action}`:\n  from {prev:?}\n  \
                 to   {next:?}\n  model {:?}\n{}",
                m.successors(action, &prev),
                lines(&w.log).join("\n")
            ));
        }
        walked.broken = m
            .invariants
            .iter()
            .filter(|inv| !m.check_invariant(inv.name, &next))
            .map(|inv| inv.name)
            .collect();
        walked.steps.push((action, prev, next));
        if !walked.broken.is_empty() {
            return Ok(walked);
        }
    }
    time_order_holds(&lines(&w.log)).map_err(|e| format!("{what}: {e}"))?;
    Ok(walked)
}

/// A walk whose steps `seed` picks at random among those enabled, until none is.
fn walk_seeded(m: &Model, seed: u64, host: Host) -> Result<Walked, String> {
    let mut rng = XorShift::new(seed);
    walk(m, host, &format!("seed {seed}"), |open| {
        Ok((!open.is_empty()).then(|| *rng.pick(open)))
    })
}

/// A walk of exactly `path`, each step enabled when it is taken.
fn walk_path(m: &Model, path: &[&'static str], host: Host, what: &str) -> Result<Walked, String> {
    let mut path = path.iter();
    walk(m, host, what, |open| match path.next() {
        None => Ok(None),
        Some(a) if open.contains(a) => Ok(Some(*a)),
        Some(a) => Err(format!("`{a}` is not enabled: {open:?}")),
    })
}

/// A scripted walk that must reach a shape the laws speak of, and what its last state holds.
struct Witness {
    what: &'static str,
    path: &'static [&'static str],
    holds: [(&'static str, i64); 4],
}

/// The scripted walks that must reach each shape the laws speak of (random walks reach each
/// only rarely, since a turn may end or go at any step).
const WITNESSES: [Witness; 6] = [
    Witness {
        what: "two boxes stacked on one began (R9)",
        path: &[
            "Show1",
            "Refresh",
            "Sweep",
            "AlabAnswer1",
            "Show2",
            "AlabAnswer2",
            "Refresh",
            "Sweep",
        ],
        holds: [("gap", 0), ("tui2", 2), ("s1", 2), ("s2", 0)],
    },
    Witness {
        what: "two boxes on two begans",
        path: &[
            "Show1",
            "Refresh",
            "Sweep",
            "PersonAnswer1",
            "Refresh",
            "Show2",
            "Refresh",
            "Sweep",
        ],
        holds: [("gap", 1), ("s1", 2), ("s2", 1), ("held", 2)],
    },
    Witness {
        what: "a box left by a person's key",
        path: &[
            "Show1",
            "Refresh",
            "Sweep",
            "PersonKey",
            "PersonAnswer1",
            "Refresh",
            "Sweep",
        ],
        holds: [("s1", 2), ("o1", 2), ("by1", 1), ("k1", 1)],
    },
    Witness {
        what: "a box left by a ctl input",
        path: &[
            "Show1",
            "Refresh",
            "Sweep",
            "CtlKey",
            "AlabAnswer1",
            "Refresh",
            "Sweep",
        ],
        holds: [("s1", 2), ("o1", 2), ("by1", 2), ("k1", 2)],
    },
    Witness {
        what: "a box approved by the harness's bound approve",
        path: &[
            "Show1",
            "Refresh",
            "Sweep",
            "HarnessKey",
            "AlabAnswer1",
            "Refresh",
            "Sweep",
        ],
        holds: [("s1", 2), ("o1", 1), ("by1", 3), ("k1", 3)],
    },
    Witness {
        what: "a box closed gone",
        path: &["Show1", "Refresh", "Sweep", "Gone"],
        holds: [("turn", 3), ("s1", 2), ("o1", 3), ("by1", 0)],
    },
];

#[test]
fn claimed_box_steps_conform_to_derived_model() {
    let m = claimed_box_steps_model();
    assert_eq!(World::new().project(), m.init_state(), "the walk's start");
    let mut ran = BTreeSet::new();
    for seed in 1..=600u64 {
        let walked = walk_seeded(&m, seed, Host::Ships).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            walked.broken.is_empty(),
            "seed {seed}: the shipping tracker broke {:?}",
            walked.broken
        );
        ran.extend(walked.steps.iter().map(|(a, ..)| *a));
    }
    for a in STEPS {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
    for Witness { what, path, holds } in WITNESSES {
        let walked = walk_path(&m, path, Host::Ships, what).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            walked.broken.is_empty() && walked.steps.len() == path.len(),
            "{what}: the shipping tracker broke {:?}",
            walked.broken
        );
        let (_, _, last) = walked.steps.last().expect("a scripted walk takes steps");
        for (k, v) in holds {
            assert_eq!(last[k], v, "{what}: `{k}` in {last:?}");
        }
    }
}

#[test]
fn claimed_box_steps_conformance_catches_a_host_whose_unboxed_act_closes_the_box() {
    let m = claimed_box_steps_model();
    let buggy = with_buggy(&m, 1);
    let caught_as = |walked: &Walked| {
        walked.broken.contains(&"ApprovedOnlyByProvenApprove")
            && walked
                .broken
                .iter()
                .all(|law| matches!(*law, "ApprovedOnlyByProvenApprove" | "NoPersonAsCtl"))
    };
    let pre = walk_path(
        &buggy,
        &["Show1", "Refresh", "Sweep", "PreAct"],
        Host::ClosesOnActs,
        "a pre-approval over a standing box",
    )
    .unwrap_or_else(|e| {
        panic!("NEGATIVE CONTROL: the broken host is not Buggy=1's `PreActCloses`: {e}")
    });
    assert!(
        caught_as(&pre),
        "NEGATIVE CONTROL: a host whose act closed the standing box broke {:?}",
        pre.broken
    );
    let (mut caught, mut refused) = (0, 0);
    for seed in 1..=600u64 {
        let walked = walk_seeded(&buggy, seed, Host::ClosesOnActs).unwrap_or_else(|e| {
            panic!("NEGATIVE CONTROL: the broken host is not Buggy=1's `PreActCloses`: {e}")
        });
        for (action, prev, next) in &walked.steps {
            if *action == "PreActCloses" {
                refused += usize::from(!m.successors("PreAct", prev).contains(next));
            }
        }
        assert!(
            walked.broken.is_empty() || caught_as(&walked),
            "seed {seed}: {:?}",
            walked.broken
        );
        caught += usize::from(!walked.broken.is_empty());
    }
    assert!(
        refused > 0,
        "NEGATIVE CONTROL: the honest model admitted every closing act"
    );
    assert!(
        caught > 0,
        "NEGATIVE CONTROL: no random walk of a host whose acts close the box broke \
         ApprovedOnlyByProvenApprove"
    );
}
