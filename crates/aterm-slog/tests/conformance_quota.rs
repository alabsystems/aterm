// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the mark quota ([`Log::enforce_mark_quota`], the one
//! policy function the quota keeps, run by every append of a mark, by a
//! change of the byte budget and by the readers' fence lifting) against the
//! derived `SessionLogMarkQuota` model
//! (`aterm_models_session::session_log_mark_quota_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`).
//!
//! A seeded walk drives a real eight-record ring (a quota of two marks):
//! observations (a bell) and claims (`note` marks, `src=mark`) appended, the
//! readers fenced and the fence lifted (ruling R4), the byte budget lowered
//! until one claim fills the marks' quarter. After every step the live
//! records are projected onto the model (observations and marks, fenced or
//! not, the newest's kind, the log's own `lost`), and the step must be the
//! model's: the action, then whatever the level-aware batch took. NEGATIVE
//! CONTROLS: a projection that miscounts a claim as an observation, and one
//! in which the quota took a fenced claim, are each rejected.

use aterm_models_session::session_log_mark_quota_model;
use aterm_slog::{Conf, Fields, Key, Kind, Limits, Log, Mark, Node, Src};
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use aterm_spec::rng::XorShift;

use crate::support::at;

/// The model's appends.
const MAX_SEQ: u64 = 7;
/// The budget that leaves the marks' quarter room for one claim.
const LOW_BUDGET: usize = 512;

struct World {
    log: Log,
    lowered: bool,
    newest_mark: bool,
    clock: u64,
}

impl World {
    fn new() -> World {
        World {
            log: Log::with_limits(0, Limits::new(8, 1 << 20).expect("a small ring")),
            lowered: false,
            newest_mark: false,
            clock: 0,
        }
    }

    fn project(&self) -> State {
        let fence = self.log.read_fence();
        let fenced = |seq: u64| fence.is_some_and(|f| seq <= f);
        let mut s = State::new();
        for k in ["uo", "fo", "um", "fm"] {
            s.insert(k, 0);
        }
        for r in self.log.window() {
            let key = match (r.src == Src::Mark, fenced(r.seq)) {
                (false, false) => "uo",
                (false, true) => "fo",
                (true, false) => "um",
                (true, true) => "fm",
            };
            *s.get_mut(key).expect("seeded") += 1;
        }
        s.insert("seq", self.log.last_seq() as i64);
        s.insert("lost", self.log.lost() as i64);
        s.insert("fence", i64::from(fence.is_some()));
        s.insert("mq", if self.lowered { 1 } else { 2 });
        s.insert("nwm", i64::from(self.newest_mark));
        s.insert(
            "nwf",
            i64::from(self.log.last_seq() > 0 && fenced(self.log.last_seq())),
        );
        s.insert("qobs", 0);
        s.insert("fgone", 0);
        s
    }

    fn obs(&mut self) {
        self.clock += 5;
        self.log
            .mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(Node::SESSION),
                at(self.clock),
            )
            .expect("an observation");
        self.newest_mark = false;
    }

    fn claim(&mut self) {
        self.clock += 5;
        let f = Fields::new()
            .text(Key::Text, "a claim long enough to fill the quarter")
            .expect("text");
        self.log
            .mark(
                Mark::new(Kind::Note, Src::Mark, Conf::Claim).fields(f),
                at(self.clock),
            )
            .expect("a claim");
        self.newest_mark = true;
    }
}

/// The states the model reaches from `prev` by `action`, then any evictions.
fn reach(m: &Model, action: &str, prev: &State) -> Vec<State> {
    let mut all: Vec<State> = m.successors(action, prev);
    let mut i = 0;
    while i < all.len() {
        for e in ["EvictObs", "EvictMark"] {
            for t in m.successors(e, &all[i]) {
                if !all.contains(&t) {
                    all.push(t);
                }
            }
        }
        i += 1;
    }
    all
}

fn admitted(m: &Model, action: &str, prev: &State, next: &State) {
    let frontier = reach(m, action, prev);
    assert!(
        frontier.contains(next),
        "real `{action}` is not the model's:\n  from {prev:?}\n  to   {next:?}"
    );
    for inv in &m.invariants {
        assert!(m.check_invariant(inv.name, next), "{}: {next:?}", inv.name);
    }
}

fn step(w: &mut World, rng: &mut XorShift) -> Option<&'static str> {
    let room = w.log.last_seq() < MAX_SEQ;
    match rng.below(6) {
        0 if room => {
            w.obs();
            Some("AppendObs")
        }
        1 | 2 if room => {
            w.claim();
            Some("AppendMark")
        }
        3 if w.log.read_fence().is_none() => {
            w.log.fence_reads();
            Some("Fence")
        }
        4 if w.log.read_fence().is_some() => {
            w.log.lift_read_fence();
            Some("Lift")
        }
        5 if !w.lowered => {
            w.log.set_byte_budget(LOW_BUDGET);
            w.lowered = true;
            Some("LowerBudget")
        }
        _ => None,
    }
}

#[test]
fn slog_mark_quota_conforms_to_derived_model() {
    let m = session_log_mark_quota_model();
    // The lowered budget leaves room for one claim and not two.
    let mut probe = World::new();
    probe.claim();
    let (_, one) = probe.log.marks_held();
    assert!(
        one <= aterm_slog::mark_quota(Limits::new(8, LOW_BUDGET).expect("l")).bytes
            && 2 * one > LOW_BUDGET / 4,
        "a claim of {one} bytes"
    );
    let mut ran = std::collections::BTreeSet::new();
    for seed in 1..=400u64 {
        let mut rng = XorShift::new(seed);
        let mut w = World::new();
        for _ in 0..24 {
            let prev = w.project();
            if let Some(action) = step(&mut w, &mut rng) {
                admitted(&m, action, &prev, &w.project());
                ran.insert(action);
            }
        }
    }
    for a in ["AppendObs", "AppendMark", "Fence", "Lift", "LowerBudget"] {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
}

/// NEGATIVE CONTROLS: a claim counted as an observation, and a fenced claim
/// the quota took, are not steps the model makes.
#[test]
fn slog_mark_quota_conformance_rejects_a_miscount_and_a_fenced_take() {
    let m = session_log_mark_quota_model();
    let mut w = World::new();
    w.claim();
    w.obs();
    w.claim();
    let prev = w.project();
    w.claim();
    let honest = w.project();
    admitted(&m, "AppendMark", &prev, &honest);
    assert_eq!((honest["um"], honest["uo"], honest["lost"]), (2, 1, 1));
    // A claim counted as an observation.
    let mut miscounted = honest.clone();
    *miscounted.get_mut("um").expect("um") -= 1;
    *miscounted.get_mut("uo").expect("uo") += 1;
    assert!(!reach(&m, "AppendMark", &prev).contains(&miscounted));

    // Fenced: two claims held by the fence, the third beside them.
    let mut f = World::new();
    f.claim();
    f.claim();
    f.log.fence_reads();
    let prev = f.project();
    f.claim();
    let honest = f.project();
    admitted(&m, "AppendMark", &prev, &honest);
    assert_eq!((honest["fm"], honest["um"], honest["lost"]), (2, 1, 0));
    // The same append had the quota taken a fenced claim instead.
    let mut took_fenced = honest.clone();
    *took_fenced.get_mut("fm").expect("fm") -= 1;
    *took_fenced.get_mut("lost").expect("lost") += 1;
    assert!(!reach(&m, "AppendMark", &prev).contains(&took_fenced));
}
