// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: what [`aterm_slog::Fold::apply`] does with claims,
//! against the derived `SessionLogClaims` model
//! (`aterm_models_session::session_log_claims_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`).
//!
//! A seeded walk writes real records into a log: a command that fails (an
//! observed `failure` need) and a later one that succeeds (the observation
//! that lifts it), `need` marks of one word from the harness, a `ctl` client
//! and an edge's session, each author's claim `clear` (`src=mark`) and a
//! non-claim `clear` (the owner's meta flag, the 2A rule), and labels and
//! verdicts from the harness, a `ctl` client and an edge. After every record
//! the fold's state is projected onto the model and the step must be the
//! model's. NEGATIVE CONTROLS: a projection in which a claim's clear lifted
//! the failure (the 2A rule), one that shows the edge's label, one in which
//! the edge's verdict took the trusted slot, one in which a `ctl` verdict
//! took the harness's, one that shows a `ctl` label over the harness's, and
//! one in which an edge's raise took the harness's need over, are each
//! rejected.

use aterm_models_session::session_log_claims_model;
use aterm_slog::{
    Actor, Close, Conf, Fields, Key, Kind, Log, Mark, Open, Out, RunOut, SessionState, ShortId,
    Src, shown_label,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use aterm_spec::rng::XorShift;

use crate::support::at;

struct World {
    log: Log,
    clock: u64,
    /// An observed need raised and not since closed by observation or the
    /// owner's clear (the model's `live`).
    live: bool,
    /// The harness raised its need and has not cleared it (`hlive`).
    hlive: bool,
    /// A trusted verdict was made (the model's `hadt`).
    hadt: bool,
    /// A proven (the harness's) verdict was made (`hadh`).
    hadh: bool,
}

/// A claim's author as the model codes the newest of any: 1 trusted, 2
/// another.
fn author(by: Actor) -> i64 {
    if by.is_trusted() { 1 } else { 2 }
}

/// Whose a trusted slot holds: 1 the harness's, 2 a `ctl` client's.
fn holder(by: Actor) -> i64 {
    match by {
        Actor::Harness => 1,
        _ => 2,
    }
}

fn project_state(st: &SessionState, now: u64, w: (bool, bool, bool, bool)) -> State {
    let (live, hlive, hadt, hadh) = w;
    let mut s = State::new();
    let observed = st.needs.iter().flatten().any(|n| n.src != Src::Mark);
    let claimed = |who: &dyn Fn(Actor) -> bool| {
        i64::from(
            st.needs
                .iter()
                .flatten()
                .any(|n| n.src == Src::Mark && who(n.by)),
        )
    };
    s.insert("obs", i64::from(observed));
    s.insert("hn", claimed(&|a| a == Actor::Harness));
    s.insert("cn", claimed(&|a| a == Actor::Ctl));
    s.insert("en", claimed(&|a| matches!(a, Actor::Sid(_))));
    s.insert(
        "top",
        st.top_need()
            .map_or(0, |n| if n.src == Src::Mark { 2 } else { 1 }),
    );
    s.insert("live", i64::from(live));
    s.insert("hlive", i64::from(hlive));
    let shown = shown_label(st, now);
    s.insert("lab", st.label.as_ref().map_or(0, |c| author(c.by)));
    s.insert("tl", shown.map_or(0, |c| author(c.by)));
    s.insert("lby", st.label_trusted.as_ref().map_or(0, |c| holder(c.by)));
    s.insert("hl", i64::from(st.label_proven.is_some()));
    s.insert("sh", i64::from(shown.is_some_and(|c| c.by.is_proven())));
    s.insert("vany", st.verdict.as_ref().map_or(0, |c| author(c.by)));
    s.insert("tv", i64::from(st.verdict_trusted.is_some()));
    s.insert("hadt", i64::from(hadt));
    s.insert(
        "vby",
        st.verdict_trusted.as_ref().map_or(0, |c| holder(c.by)),
    );
    s.insert("hv", i64::from(st.verdict_proven.is_some()));
    s.insert("hadh", i64::from(hadh));
    s
}

impl World {
    fn new() -> World {
        World {
            log: Log::new(0),
            clock: 0,
            live: false,
            hlive: false,
            hadt: false,
            hadh: false,
        }
    }

    fn project(&self) -> State {
        project_state(
            self.log.state(),
            crate::support::WALL0 + self.clock,
            (self.live, self.hlive, self.hadt, self.hadh),
        )
    }

    fn run(&mut self, out: RunOut) {
        self.clock += 5;
        let r = self
            .log
            .open(
                Open::new(Kind::Cmd, Src::Shell, Conf::Exact),
                at(self.clock),
            )
            .expect("cmd");
        self.log
            .close(
                r,
                Close::new(Out::Run(out), Src::Shell, Conf::Exact),
                at(self.clock),
            )
            .expect("closes");
    }

    fn mark(&mut self, kind: Kind, src: Src, by: Actor, f: Fields) {
        self.clock += 5;
        let conf = if src == Src::Mark {
            Conf::Claim
        } else {
            Conf::Strong
        };
        self.log
            .mark(Mark::new(kind, src, conf).by(by).fields(f), at(self.clock))
            .expect("mark");
    }

    fn edge() -> Actor {
        Actor::Sid(ShortId::new("s-00000000000000000001").expect("sid"))
    }

    /// One real step and the model's name for it, when it is enabled.
    fn step(&mut self, m: &Model, rng: &mut XorShift) -> Option<&'static str> {
        const ACTIONS: [&str; 18] = [
            "RaiseObserved",
            "ObserveLifts",
            "HarnessNeed",
            "CtlNeed",
            "EdgeNeed",
            "HarnessClear",
            "CtlClear",
            "EdgeClear",
            "OwnerClear",
            "HarnessLabel",
            "CtlLabel",
            "OtherLabel",
            "HarnessVerdict",
            "CtlVerdict",
            "OtherVerdict",
            // Twice as likely: the claim slots' order is what they judge.
            "CtlVerdict",
            "CtlLabel",
            "EdgeNeed",
        ];
        let s = self.project();
        let pick = ACTIONS[rng.index(ACTIONS.len())];
        if !m.action_enabled(pick, &s) {
            return None;
        }
        let label = |text: &str| {
            Fields::new()
                .text(Key::Text, text)
                .and_then(|f| f.u(Key::Ttl, 600))
                .expect("label")
        };
        let word = |w: &str| Fields::new().word(Key::Word, w).expect("word");
        let none = Fields::new;
        match pick {
            "RaiseObserved" => {
                self.run(RunOut::Fail);
                self.live = true;
            }
            "ObserveLifts" => {
                self.run(RunOut::Ok);
                self.live = false;
            }
            // One word from every author: each its own need.
            "HarnessNeed" => {
                self.mark(Kind::Need, Src::Mark, Actor::Harness, word("help"));
                self.hlive = true;
            }
            "CtlNeed" => self.mark(Kind::Need, Src::Mark, Actor::Ctl, word("help")),
            "EdgeNeed" => self.mark(Kind::Need, Src::Mark, World::edge(), word("help")),
            "HarnessClear" => {
                self.mark(Kind::Clear, Src::Mark, Actor::Harness, none());
                self.hlive = false;
            }
            "CtlClear" => self.mark(Kind::Clear, Src::Mark, Actor::Ctl, none()),
            "EdgeClear" => self.mark(Kind::Clear, Src::Mark, World::edge(), none()),
            "OwnerClear" => {
                self.mark(Kind::Clear, Src::Meta, Actor::Ctl, none());
                self.live = false;
            }
            "HarnessLabel" => self.mark(Kind::Label, Src::Mark, Actor::Harness, label("mine")),
            "CtlLabel" => self.mark(Kind::Label, Src::Mark, Actor::Ctl, label("ours")),
            "OtherLabel" => self.mark(Kind::Label, Src::Mark, World::edge(), label("theirs")),
            "HarnessVerdict" => {
                self.mark(Kind::Verdict, Src::Mark, Actor::Harness, word("on-track"));
                self.hadt = true;
                self.hadh = true;
            }
            "CtlVerdict" => {
                self.mark(Kind::Verdict, Src::Mark, Actor::Ctl, word("done"));
                self.hadt = true;
            }
            _ => self.mark(Kind::Verdict, Src::Mark, World::edge(), word("blocked")),
        }
        Some(pick)
    }
}

fn admitted(m: &Model, action: &str, prev: &State, next: &State) {
    assert!(
        m.successors(action, prev).contains(next),
        "real `{action}` is not the model's:\n  from {prev:?}\n  to   {next:?}"
    );
    for inv in &m.invariants {
        assert!(m.check_invariant(inv.name, next), "{}: {next:?}", inv.name);
    }
}

#[test]
fn fold_claims_conform_to_derived_model() {
    let m = session_log_claims_model();
    let mut ran = std::collections::BTreeSet::new();
    for seed in 1..=400u64 {
        let mut rng = XorShift::new(seed);
        let mut w = World::new();
        for _ in 0..40 {
            let prev = w.project();
            if let Some(action) = w.step(&m, &mut rng) {
                admitted(&m, action, &prev, &w.project());
                ran.insert(action);
            }
        }
    }
    assert_eq!(ran.len(), 15, "every action driven: {ran:?}");
}

/// NEGATIVE CONTROLS: the 2A clear rule, an edge's label shown, an edge's
/// verdict in the trusted slot, a `ctl` verdict in the proven slot, a `ctl`
/// label shown over the harness's, and an edge's raise of the harness's word
/// taking its need over, are not steps the model makes.
#[test]
fn fold_claims_conformance_rejects_what_a_claim_may_never_do() {
    let m = session_log_claims_model();
    let word = |w: &str| Fields::new().word(Key::Word, w).expect("w");
    let label = |t: &str| {
        Fields::new()
            .text(Key::Text, t)
            .and_then(|f| f.u(Key::Ttl, 600))
            .expect("label")
    };
    let mut w = World::new();
    w.run(RunOut::Fail);
    w.live = true;
    w.mark(Kind::Need, Src::Mark, Actor::Ctl, word("help"));
    let prev = w.project();
    w.mark(Kind::Clear, Src::Mark, Actor::Ctl, Fields::new());
    let honest = w.project();
    admitted(&m, "CtlClear", &prev, &honest);
    let mut lifted = honest.clone();
    lifted.insert("obs", 0);
    lifted.insert("top", 0);
    assert!(
        !m.successors("CtlClear", &prev).contains(&lifted),
        "the 2A rule"
    );

    let prev = w.project();
    w.mark(Kind::Label, Src::Mark, World::edge(), label("theirs"));
    let honest = w.project();
    admitted(&m, "OtherLabel", &prev, &honest);
    let mut shown = honest.clone();
    shown.insert("tl", 2);
    assert!(!m.successors("OtherLabel", &prev).contains(&shown), "shown");

    w.mark(Kind::Verdict, Src::Mark, Actor::Harness, word("off-track"));
    w.hadt = true;
    w.hadh = true;
    let prev = w.project();
    w.mark(Kind::Verdict, Src::Mark, World::edge(), word("blocked"));
    let honest = w.project();
    admitted(&m, "OtherVerdict", &prev, &honest);
    let mut displaced = honest.clone();
    displaced.insert("tv", 0);
    assert!(
        !m.successors("OtherVerdict", &prev).contains(&displaced),
        "displaced"
    );

    // The worker, a `ctl` client inside its session, marks `done` after the
    // harness's `off-track`: the trusted slot is the worker's, the proven one
    // stays the harness's.
    let prev = w.project();
    w.mark(Kind::Verdict, Src::Mark, Actor::Ctl, word("done"));
    let honest = w.project();
    admitted(&m, "CtlVerdict", &prev, &honest);
    assert_eq!((honest["vby"], honest["hv"]), (2, 1), "{honest:?}");
    let mut gone = honest.clone();
    gone.insert("hv", 0);
    assert!(
        !m.successors("CtlVerdict", &prev).contains(&gone),
        "a ctl verdict displaced the harness's"
    );

    w.mark(Kind::Label, Src::Mark, Actor::Harness, label("mine"));
    let prev = w.project();
    w.mark(Kind::Label, Src::Mark, Actor::Ctl, label("ours"));
    let honest = w.project();
    admitted(&m, "CtlLabel", &prev, &honest);
    assert_eq!(honest["sh"], 1, "the harness's label stays shown");
    let mut over = honest.clone();
    over.insert("sh", 0);
    assert!(
        !m.successors("CtlLabel", &prev).contains(&over),
        "a ctl label shown over the harness's"
    );

    w.mark(Kind::Need, Src::Mark, Actor::Harness, word("help"));
    w.hlive = true;
    let prev = w.project();
    w.mark(Kind::Need, Src::Mark, World::edge(), word("help"));
    let honest = w.project();
    admitted(&m, "EdgeNeed", &prev, &honest);
    let mut taken = honest.clone();
    taken.insert("hn", 0);
    assert!(
        !m.successors("EdgeNeed", &prev).contains(&taken),
        "an edge's raise took the harness's need over"
    );
}
