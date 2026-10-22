// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1: the real [`Boundary`] follows `AosBoundaryPublication` (`aterm-models-aos`) in lock
//! step, so the laws the model holds over its bounded space (`NeverMixed`, `EachInputOnce`,
//! `StaleNeverCommitted`, `DeliveredToTheLivePair`) are properties of the code.
//!
//! The boundary hosts the toy schema under *tagged* code: the good toy step, then the code's tag
//! folded into `seen`. The attached code has tag 0 and every prepare takes a fresh one, so two
//! codes over the same inputs leave different states, and the projection can tell which
//! publication installed the live state as well as which installed the live code.
//!
//! The projection (model variable: real observable):
//!
//! * `paused`: [`Boundary::is_paused`].
//! * `rev`, `cur`: `pair().head.revision` and `pair().head.input_cursor`, less their values just
//!   after attach (the walk's prelude moves both off zero for some seeds).
//! * `code_v`: the publication whose candidate tag the live code carries (0 for the attached
//!   code), or -1 for a code no publication installed.
//! * `state_v`: `code_v` when the live state is that publication's installed state folded
//!   through the live code over every input delivered since; otherwise the publication whose
//!   pair the live state is (its installed state folded through its own code), or -1.
//! * `queued`: [`Boundary::queued`], cross-checked against the harness's own copy of the queue.
//! * `arrived`: the harness's count of `admit` calls that returned `Run`, `Queued` or `Rejected`.
//! * `delivered`: the harness's count of inputs handed to a step, cross-checked against
//!   `Stats::delivered`.
//! * `pa`/`ra`/`ia` and `pb`/`rb`/`ib`: each slot's held `Prepared` and its `expected()` head,
//!   relative to attach (zero while the slot is empty).
//! * `stale`: latched when `commit` returned `Ok` for a `Prepared` whose expected head was not
//!   the live head read just before the call.
//!
//! Each model action is one real call: `Input` is an `admit` that returned `Run`, then the live
//! code's step and `advance`; `Hold` is an `admit` that returned `Queued`; `Overflow` one that
//! returned `Rejected`; a `commit` the model's guard rules out is refused, consumes its candidate
//! and is the model's `Discard`. Before every move the walk also makes the calls whose model
//! guard is false (`pause` while paused, `prepare` and `reject` while running, a tick while
//! paused): each refuses exactly and moves no projected variable. Delivery order, which the
//! scalar model cannot state, is checked on the real queue: what each delivered input returned
//! and the order the receiving pair's log holds them in.
//!
//! The seeded walks (a deterministic xorshift stream per seed and episode, under the committed
//! configuration and the `Cap = 0` scenario) reach about half of the model's 510 states; a second
//! test replays the model's shortest path to every reachable state and takes every move the
//! model allows from it, so every transition of the bounded space is a checked real step.
//!
//! The negative controls take each `Buggy` dial in turn: the real boundary refuses the commit
//! the stale mutant publishes (1), never shows the mixed mutant's successor at any store a commit
//! can stop after (2, gated like the seam), and takes a step the lost-input (3) and wrong-pair
//! (4) mutants do not admit.

use std::collections::BTreeSet;

use aterm_aos::contract::Contract;
use aterm_aos::effects::{EffectPort, NoEffects};
use aterm_aos::publish::{Admit, Boundary, Commit, Prepared, PublishRefusal};
use aterm_aos::replay::{Run, replay};
use aterm_aos::task::{Code, EntryRefusal, Schema, StepRefusal};
use aterm_aos::{ArtifactId, ArtifactKind, TaskIdentity};
use aterm_models_aos::aos_boundary_publication_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use crate::toy::{
    TickClears, ToyCode, ToyInput, ToyObs, ToySchema, ToyState, Variant, code, contract, mix,
    profile, task,
};

/// The log epoch of the attach the model's initial state is.
const ATTACH_EPOCH: u64 = 7;
/// Room for every input of a walk, so no walk re-anchors.
const MAX_EVENTS: usize = 64;
/// Seeds of the lock-step walk, each run for [`EPISODES`] episodes of at most [`STEPS`] moves.
const SEEDS: u64 = 64;
const EPISODES: u64 = 16;
const STEPS: usize = 40;
/// Seeds whose first commit over a non-empty replay range is stopped at every store.
#[cfg(debug_assertions)]
const SEAM_SEEDS: u64 = 24;
/// Folded with the tag, so tag 0 also leaves its mark.
const TAG_FOLD: u64 = 0x7a67;

/// The good toy step, then the code's tag folded into `seen`.
#[derive(Clone, Debug)]
struct Tagged {
    id: ArtifactId,
    tag: u64,
    good: ToyCode,
}

fn tagged(tag: u64) -> Tagged {
    Tagged {
        id: ArtifactId::new(ArtifactKind::Code, "toy-tagged", 1, &tag.to_le_bytes()),
        tag,
        good: code(Variant::Good),
    }
}

impl Code<ToySchema> for Tagged {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn entry(&self, schema: &ToySchema, state: &ToyState) -> Result<(), EntryRefusal> {
        self.good.entry(schema, state)
    }

    fn step(
        &self,
        schema: &ToySchema,
        state: &mut ToyState,
        input: &ToyInput,
        port: &mut dyn EffectPort,
    ) -> Result<ToyObs, StepRefusal> {
        let obs = self.good.step(schema, state, input, port)?;
        state.seen = mix(state.seen, self.tag.wrapping_add(TAG_FOLD), 0);
        Ok(obs)
    }
}

fn identity() -> TaskIdentity {
    TaskIdentity {
        task: task(),
        code: tagged(0).id,
        contract: contract().id().clone(),
        schema: ToySchema::new().id().clone(),
        profile: profile(),
    }
}

type Tb = Boundary<ToySchema, Tagged, TickClears>;

/// The model's names for one candidate slot.
struct SlotNames {
    held: &'static str,
    rev: &'static str,
    cur: &'static str,
    prepare: &'static str,
    commit: &'static str,
    discard: &'static str,
}

const SLOT: [SlotNames; 2] = [
    SlotNames {
        held: "pa",
        rev: "ra",
        cur: "ia",
        prepare: "PrepareA",
        commit: "CommitA",
        discard: "DiscardA",
    },
    SlotNames {
        held: "pb",
        rev: "rb",
        cur: "ib",
        prepare: "PrepareB",
        commit: "CommitB",
        discard: "DiscardB",
    },
];

fn slot(action: &str) -> usize {
    SLOT.iter()
        .position(|n| action == n.prepare || action == n.commit || action == n.discard)
        .unwrap_or_else(|| panic!("`{action}` names no slot"))
}

/// A deterministic xorshift stream: the walk draws nothing from the host.
struct Rng(u64);

impl Rng {
    fn new(seed: u64, episode: u64) -> Rng {
        Rng(seed
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(episode.wrapping_mul(0xbf58_476d_1ce4_e5b9))
            | 1)
    }

    fn draw(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.draw() % n as u64) as usize
    }

    /// Mostly adds, some resets and, unless only queue-class inputs will do, ticks.
    fn input(&mut self, queue_class: bool) -> ToyInput {
        match self.below(if queue_class { 5 } else { 6 }) {
            0 => ToyInput::Reset,
            5 => ToyInput::Tick,
            _ => ToyInput::Add(1 + (self.draw() % 9) as i64),
        }
    }
}

fn index(i: usize) -> i64 {
    i64::try_from(i).expect("a walk's counts are small")
}

fn rel(v: u64, base: u64) -> i64 {
    i64::try_from(v).expect("small") - i64::try_from(base).expect("small")
}

fn fold(code: &Tagged, start: &ToyState, inputs: &[ToyInput]) -> Run<ToySchema> {
    replay(
        &ToySchema::new(),
        code,
        start.clone(),
        inputs,
        0,
        &mut NoEffects,
    )
}

fn observations(run: &Run<ToySchema>) -> Vec<Result<ToyObs, StepRefusal>> {
    assert!(
        run.refused.is_none(),
        "the good step takes every walk input"
    );
    run.observations
        .iter()
        .map(|(_, o)| Ok(o.clone()))
        .collect()
}

/// The successor `action` takes `state` to under `model`; the action must be enabled.
fn successor(model: &Model, action: &str, state: &State) -> State {
    let mut next = state.clone();
    assert!(model.fire(action, &mut next), "`{action}` is enabled");
    next
}

/// The host's live path for an input that runs: `admit` returned `Run`, then the live code's
/// step, then `advance`.
fn live_step(b: &mut Tb, input: &ToyInput) {
    assert_eq!(b.admit(input), Admit::Run);
    let schema = b.schema().clone();
    let (code, state) = b.pair_mut();
    code.step(&schema, state, input, &mut NoEffects)
        .expect("the good step takes every walk input");
    b.advance();
}

/// One publication as the harness saw it: the tag of the code it installed (0: the attached
/// code), the state it installed, and each input delivered while it was live.
struct Gen {
    tag: u64,
    start: ToyState,
    since: Vec<ToyInput>,
}

/// How often each call the model's guards rule out was made, and refused.
#[derive(Debug, Default)]
struct Refusals {
    already_paused: u32,
    prepare_not_paused: u32,
    reject_running: u32,
    skipped: u32,
    commit_not_paused: u32,
    commit_stale: u32,
}

impl Refusals {
    fn add(&mut self, o: &Refusals) {
        self.already_paused += o.already_paused;
        self.prepare_not_paused += o.prepare_not_paused;
        self.reject_running += o.reject_running;
        self.skipped += o.skipped;
        self.commit_not_paused += o.commit_not_paused;
        self.commit_stale += o.commit_stale;
    }

    fn each_ran(&self) -> bool {
        [
            self.already_paused,
            self.prepare_not_paused,
            self.reject_running,
            self.skipped,
            self.commit_not_paused,
            self.commit_stale,
        ]
        .iter()
        .all(|&n| n > 0)
    }
}

/// What a commit is expected to do, read before the `Prepared` is handed over.
struct PendingCommit {
    tag: u64,
    start: ToyState,
    stale: bool,
    drained: Vec<ToyInput>,
    expect: Vec<Result<ToyObs, StepRefusal>>,
}

/// The real boundary and what the harness knows of the run.
struct Real {
    b: Tb,
    rev0: u64,
    cur0: u64,
    delivered0: u64,
    slots: [Option<Prepared<ToySchema, Tagged>>; 2],
    next_tag: u64,
    gens: Vec<Gen>,
    queue: Vec<ToyInput>,
    arrived: i64,
    delivered: i64,
    stale: i64,
}

impl Real {
    /// An attached boundary with a queue of `cap`. `prelude` picks a history before the attach
    /// the model starts at: up to two live inputs, and on odd values a restore (which mints a
    /// revision) and a tick, so neither the revision nor the cursor starts at zero for every
    /// seed.
    fn new(prelude: u64, cap: usize) -> Real {
        let start = ToyState {
            total: 2,
            hot: true,
            seen: prelude,
        };
        let mut b = Boundary::new(
            ToySchema::new(),
            identity(),
            tagged(0),
            contract(),
            start,
            cap,
        );
        b.attach(1, MAX_EVENTS).unwrap();
        let c0 = b.checkpoint().expect("attached").clone();
        for n in 0..prelude % 3 {
            live_step(&mut b, &ToyInput::Add(1 + i64::try_from(n).expect("small")));
        }
        if prelude % 2 == 1 {
            b.restore(&c0).expect("the attach checkpoint restores");
            live_step(&mut b, &ToyInput::Tick);
        }
        b.attach(ATTACH_EPOCH, MAX_EVENTS).unwrap();
        let head = b.pair().head.clone();
        let attached = b.pair().state.clone();
        Real {
            rev0: head.revision.0,
            cur0: head.input_cursor,
            delivered0: b.stats().delivered,
            b,
            slots: [None, None],
            next_tag: 1,
            gens: vec![Gen {
                tag: 0,
                start: attached,
                since: Vec::new(),
            }],
            queue: Vec::new(),
            arrived: 0,
            delivered: 0,
            stale: 0,
        }
    }

    /// The model's variables, read off the real boundary and the harness's counts.
    fn project(&self) -> State {
        let pair = self.b.pair();
        let mut s = State::new();
        s.insert("paused", i64::from(self.b.is_paused()));
        s.insert("rev", rel(pair.head.revision.0, self.rev0));
        s.insert("cur", rel(pair.head.input_cursor, self.cur0));
        let code_v = self.code_v();
        s.insert("code_v", code_v);
        s.insert("state_v", self.state_v(code_v));
        s.insert("arrived", self.arrived);
        s.insert("queued", index(self.b.queued()));
        s.insert("delivered", self.delivered);
        for (held, names) in self.slots.iter().zip(&SLOT) {
            let (present, rev, cur) = match held {
                Some(p) => (
                    1,
                    rel(p.expected().revision.0, self.rev0),
                    rel(p.expected().input_cursor, self.cur0),
                ),
                None => (0, 0, 0),
            };
            s.insert(names.held, present);
            s.insert(names.rev, rev);
            s.insert(names.cur, cur);
        }
        s.insert("stale", self.stale);
        s
    }

    fn code_v(&self) -> i64 {
        let live = self.b.pair().code.tag;
        self.gens
            .iter()
            .position(|g| g.tag == live)
            .map_or(-1, index)
    }

    fn state_v(&self, code_v: i64) -> i64 {
        let live = &self.b.pair().state;
        let installed = |g: &Gen| fold(&tagged(g.tag), &g.start, &g.since).state == *live;
        if let Some(g) = usize::try_from(code_v).ok().and_then(|i| self.gens.get(i))
            && installed(g)
        {
            return code_v;
        }
        self.gens.iter().position(installed).map_or(-1, index)
    }

    fn live_gen(&mut self) -> &mut Gen {
        self.gens.last_mut().expect("the attached publication")
    }

    fn fresh_tag(&mut self) -> u64 {
        let tag = self.next_tag;
        self.next_tag += 1;
        tag
    }

    /// What the live pair returns for `inputs`, delivered in order from its current state.
    fn through_live(&self, inputs: &[ToyInput]) -> Vec<Result<ToyObs, StepRefusal>> {
        observations(&fold(&self.b.pair().code, &self.b.pair().state, inputs))
    }

    /// The old pair received `drained` (a reject, or an overflow and its input): its log ends
    /// with them, in order.
    fn delivered_to_old_pair(&mut self, drained: Vec<ToyInput>) {
        let log = self.b.log().expect("attached").inputs();
        assert!(
            log.ends_with(&drained),
            "the old pair's log ends with the queue in order: {log:?} / {drained:?}"
        );
        self.delivered += index(drained.len());
        self.queue.clear();
        self.live_gen().since.extend(drained);
    }

    fn pending_commit(&mut self, p: &Prepared<ToySchema, Tagged>) -> PendingCommit {
        let live = self.b.head().clone();
        PendingCommit {
            tag: p.candidate().tag,
            start: p.state().clone(),
            stale: p.expected() != &live,
            drained: self.queue.clone(),
            expect: observations(&fold(p.candidate(), p.state(), &self.queue)),
        }
    }

    fn finish_commit(&mut self, pending: PendingCommit, c: Commit<ToySchema>) {
        if pending.stale {
            self.stale = 1;
        }
        assert_eq!(c.revision, self.b.pair().head.revision);
        assert_eq!(
            c.delivered, pending.expect,
            "the queue reached the new pair once each, in order"
        );
        assert_eq!(
            self.b.log().expect("attached").inputs(),
            &pending.drained[..],
            "the new pair's log opens at the commit and holds the queue, in order"
        );
        self.delivered += index(pending.drained.len());
        self.queue.clear();
        self.gens.push(Gen {
            tag: pending.tag,
            start: pending.start,
            since: pending.drained,
        });
    }

    /// The real call a healthy model action names, with what the call must return.
    fn act(&mut self, action: &str, rng: &mut Rng) {
        match action {
            "Input" => {
                let input = rng.input(false);
                live_step(&mut self.b, &input);
                self.arrived += 1;
                self.delivered += 1;
                self.live_gen().since.push(input);
            }
            "Pause" => {
                let h = self
                    .b
                    .pause()
                    .expect("a running, attached boundary pauses at a step boundary");
                assert_eq!(&h, self.b.head());
            }
            "PrepareA" | "PrepareB" => {
                let tag = self.fresh_tag();
                let p = self
                    .b
                    .prepare(tagged(tag))
                    .expect("a candidate prepares against the paused head");
                assert_eq!(Some(p.expected()), self.b.paused_head());
                // The candidate replayed from the ancestor over the whole log: the live
                // publication's installed state and every input delivered since.
                let g = self.gens.last().expect("the attached publication");
                assert_eq!(p.state(), &fold(&tagged(tag), &g.start, &g.since).state);
                self.slots[slot(action)] = Some(p);
            }
            "Hold" => {
                let input = rng.input(true);
                assert_eq!(self.b.admit(&input), Admit::Queued);
                self.arrived += 1;
                self.queue.push(input);
            }
            "Overflow" => {
                let input = rng.input(true);
                let held = self.queue.len();
                let mut drained = self.queue.clone();
                drained.push(input.clone());
                let expect = self.through_live(&drained);
                assert_eq!(
                    self.b.admit(&input),
                    Admit::Rejected {
                        delivered_after: held
                    }
                );
                assert_eq!(
                    self.b.take_delivered(),
                    expect,
                    "the queue, then the input, reached the old pair once each"
                );
                self.arrived += 1;
                self.delivered_to_old_pair(drained);
            }
            "CommitA" | "CommitB" => {
                let p = self.slots[slot(action)]
                    .take()
                    .expect("the slot holds a candidate");
                let pending = self.pending_commit(&p);
                let c = self
                    .b
                    .commit(p)
                    .expect("a candidate prepared against the live head commits");
                self.finish_commit(pending, c);
            }
            "Reject" => {
                let drained = self.queue.clone();
                let expect = self.through_live(&drained);
                assert_eq!(
                    self.b.reject(),
                    expect,
                    "the queue reached the old pair once each, in order"
                );
                self.delivered_to_old_pair(drained);
            }
            "DiscardA" | "DiscardB" => {
                drop(
                    self.slots[slot(action)]
                        .take()
                        .expect("the slot holds a candidate"),
                );
            }
            other => panic!("no action `{other}` in the publication model"),
        }
    }

    /// A commit of slot `x`'s candidate where the model's guard is false: refused exactly,
    /// changing nothing but the candidate it consumed.
    fn refused_commit(&mut self, x: usize, seen: &mut Refusals) -> PublishRefusal {
        let p = self.slots[x].take().expect("the slot holds a candidate");
        let want = if self.b.is_paused() {
            seen.commit_stale += 1;
            PublishRefusal::StaleHead {
                expected: Box::new(p.expected().clone()),
                live: Box::new(self.b.head().clone()),
            }
        } else {
            seen.commit_not_paused += 1;
            PublishRefusal::NotPaused
        };
        let stats = self.b.stats();
        assert_eq!(self.b.commit(p).err().as_ref(), Some(&want));
        assert_eq!(self.b.stats(), stats);
        want
    }

    /// The calls the model's guards rule out in this state, each refused, moving no projected
    /// variable and no counter (a skipped tick counts as skipped).
    fn probe(&mut self, seen: &mut Refusals) {
        let before = self.project();
        let mut stats = self.b.stats();
        if self.b.is_paused() {
            assert_eq!(self.b.pause(), Err(PublishRefusal::AlreadyPaused));
            // A tick is a frame the task does not observe while paused: neither queued nor
            // delivered, so no input in the model.
            assert_eq!(self.b.admit(&ToyInput::Tick), Admit::Skipped);
            stats.skipped += 1;
            seen.already_paused += 1;
            seen.skipped += 1;
        } else {
            let tag = self.fresh_tag();
            assert_eq!(
                self.b.prepare(tagged(tag)).err(),
                Some(PublishRefusal::NotPaused)
            );
            assert!(
                self.b.reject().is_empty(),
                "nothing to reject while running"
            );
            seen.prepare_not_paused += 1;
            seen.reject_running += 1;
        }
        assert_eq!(
            self.project(),
            before,
            "a refused call moves no projected variable"
        );
        assert_eq!(self.b.stats(), stats);
    }

    /// The harness's copies agree with the boundary.
    fn cross_check(&mut self) {
        assert_eq!(self.b.queued(), self.queue.len());
        assert_eq!(
            self.b.stats().delivered - self.delivered0,
            u64::try_from(self.delivered).expect("non-negative")
        );
        if let Some(h) = self.b.paused_head().cloned() {
            assert_eq!(&h, self.b.head(), "nothing moves the head while paused");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Move {
    /// An enabled model action, taken by the real call it names.
    Act(&'static str),
    /// A commit of slot `x`'s candidate where the model's `Commit` guard is false: the real
    /// boundary refuses it, and the consumed candidate is the model's `Discard`.
    RefusedCommit(usize),
}

/// One move taken in lock step.
struct Step {
    mv: Move,
    prev: State,
    action: &'static str,
    got: State,
    refusal: Option<PublishRefusal>,
}

/// One episode of the walk: the real boundary, the model's state, and the stream that picks.
struct Walker<'m> {
    model: &'m Model,
    rng: Rng,
    real: Real,
    st: State,
    refusals: Refusals,
    at: String,
}

impl<'m> Walker<'m> {
    fn new(model: &'m Model, cap: usize, seed: u64, episode: u64) -> Self {
        let mut rng = Rng::new(seed, episode);
        let real = Real::new(rng.draw() % 6, cap);
        let st = model.init_state();
        let at = format!("cap {cap} seed {seed} episode {episode}");
        assert_eq!(
            real.project(),
            st,
            "{at}: the boundary just after attach is the model's initial state"
        );
        Walker {
            model,
            rng,
            real,
            st,
            refusals: Refusals::default(),
            at,
        }
    }

    /// Makes the calls the model rules out here, then picks a move the model allows: an enabled
    /// action, or a commit of a held candidate whose `Commit` guard is false. `None` once the
    /// bounded box is spent.
    fn next_move(&mut self) -> Option<Move> {
        self.real.probe(&mut self.refusals);
        let held = [self.real.slots[0].is_some(), self.real.slots[1].is_some()];
        let moves = allowed(self.model, &self.st, held);
        // Lean away from arrivals, which spend the bounded box, so runs reach later revisions.
        let quiet: Vec<Move> = moves
            .iter()
            .copied()
            .filter(|m| !matches!(m, Move::Act("Input" | "Hold" | "Overflow")))
            .collect();
        let pool = if !quiet.is_empty() && self.rng.below(3) != 0 {
            quiet
        } else {
            moves
        };
        if pool.is_empty() {
            return None;
        }
        Some(pool[self.rng.below(pool.len())])
    }

    /// Takes `mv` on the real boundary and its action on the model, then checks that the
    /// projection is the model's state and that the model admits the real step by that name.
    fn take(&mut self, mv: Move) -> Step {
        let (action, refusal) = match mv {
            Move::Act(a) => {
                self.real.act(a, &mut self.rng);
                (a, None)
            }
            Move::RefusedCommit(x) => {
                let r = self.real.refused_commit(x, &mut self.refusals);
                (SLOT[x].discard, Some(r))
            }
        };
        let prev = self.st.clone();
        assert!(
            self.model.fire(action, &mut self.st),
            "{}: {mv:?} is enabled",
            self.at
        );
        let got = self.real.project();
        assert_eq!(got, self.st, "{}: {mv:?} from {prev:?}", self.at);
        assert_eq!(
            interp::admits(self.model, &prev, &got),
            Some(action),
            "{}: the model admits the real step {mv:?}, by name",
            self.at
        );
        self.real.cross_check();
        Step {
            mv,
            prev,
            action,
            got,
            refusal,
        }
    }
}

/// The moves the model allows in `st`: each enabled action, and a commit of each held candidate
/// whose `Commit` guard is false (refused, and the model's `Discard`).
fn allowed(model: &Model, st: &State, held: [bool; 2]) -> Vec<Move> {
    let mut moves: Vec<Move> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| model.action_enabled(a, st))
        .map(Move::Act)
        .collect();
    for (x, names) in SLOT.iter().enumerate() {
        if held[x] && !model.action_enabled(names.commit, st) {
            moves.push(Move::RefusedCommit(x));
        }
    }
    moves
}

/// Each reachable state of `model`, once, with the shortest action path to it from the initial
/// state (breadth first, actions in model order).
fn shortest_paths(model: &Model) -> Vec<(State, Vec<&'static str>)> {
    let init = model.init_state();
    let mut seen = BTreeSet::from([init.clone()]);
    let mut tree = vec![(init, Vec::new())];
    let mut next = 0;
    while let Some((state, path)) = tree.get(next).cloned() {
        for a in &model.actions {
            for succ in model.successors(a.name, &state) {
                if seen.insert(succ.clone()) {
                    let mut longer = path.clone();
                    longer.push(a.name);
                    tree.push((succ, longer));
                }
            }
        }
        next += 1;
    }
    tree
}

/// Every seeded episode of the walk under `model` with a queue of `cap`; `each` sees every move
/// taken. Returns how often each ruled-out call was made.
fn lockstep(model: &Model, cap: usize, seeds: u64, mut each: impl FnMut(&Step)) -> Refusals {
    let mut refusals = Refusals::default();
    for seed in 1..=seeds {
        for episode in 0..EPISODES {
            let mut w = Walker::new(model, cap, seed, episode);
            for _ in 0..STEPS {
                let Some(mv) = w.next_move() else { break };
                each(&w.take(mv));
            }
            refusals.add(&w.refusals);
        }
    }
    refusals
}

/// A walker that took `actions` in order from the initial state.
fn scripted<'m>(model: &'m Model, cap: usize, actions: &[&'static str]) -> Walker<'m> {
    let mut w = Walker::new(model, cap, 1, 0);
    for a in actions {
        w.take(Move::Act(a));
    }
    w
}

/// `seed`'s first episode, stopped before its first commit over a non-empty replay range. The
/// walk is deterministic, so every call returns the same boundary.
#[cfg(debug_assertions)]
fn first_commit_over_a_range(model: &Model, seed: u64) -> Option<(Walker<'_>, Move)> {
    let mut w = Walker::new(model, 2, seed, 0);
    for _ in 0..STEPS {
        let mv = w.next_move()?;
        if matches!(mv, Move::Act(a) if a.starts_with("Commit"))
            && !w.real.b.log().expect("attached").is_empty()
        {
            return Some((w, mv));
        }
        w.take(mv);
    }
    None
}

#[test]
fn the_real_boundary_follows_the_publication_model_in_lock_step() {
    let healthy = aos_boundary_publication_model();
    // The committed configuration, then the scenario of a boundary built with no queue.
    for cap in [2usize, 0] {
        let model = interp::with_consts(&healthy, &[("Cap", index(cap))]);
        let mut fired = BTreeSet::new();
        let refusals = lockstep(&model, cap, SEEDS, |s| {
            fired.insert(s.action);
        });
        assert_eq!(
            fired,
            interp::fired_actions(&model),
            "cap {cap}: the real boundary took every action the model can take"
        );
        assert!(
            refusals.each_ran(),
            "cap {cap}: every ruled-out call was made: {refusals:?}"
        );
    }
}

/// The seeded walks reach about half of the model's states. This replays the model's shortest
/// path to each reachable state on a fresh boundary, makes the calls the model rules out there,
/// then takes each move the model allows from it, so every transition of the bounded space is
/// a checked real step.
#[test]
fn every_transition_of_the_publication_model_is_a_real_step() {
    let healthy = aos_boundary_publication_model();
    for cap in [2usize, 0] {
        let model = interp::with_consts(&healthy, &[("Cap", index(cap))]);
        let tree = shortest_paths(&model);
        let reachable: BTreeSet<State> = interp::reachable_states(&model).into_iter().collect();
        assert_eq!(
            tree.iter().map(|(s, _)| s.clone()).collect::<BTreeSet<_>>(),
            reachable,
            "cap {cap}"
        );
        let mut refusals = Refusals::default();
        let mut taken = 0usize;
        let mut fired = BTreeSet::new();
        for (i, (state, path)) in tree.iter().enumerate() {
            let seed = u64::try_from(i).expect("small");
            let held = |x: usize| state[SLOT[x].held] == 1;
            for mv in allowed(&model, state, [held(0), held(1)]) {
                let mut w = Walker::new(&model, cap, seed, 0);
                for a in path {
                    w.take(Move::Act(a));
                }
                assert_eq!(&w.st, state);
                w.real.probe(&mut w.refusals);
                fired.insert(w.take(mv).action);
                refusals.add(&w.refusals);
                taken += 1;
            }
        }
        let transitions: usize = reachable
            .iter()
            .map(|s| {
                let held = |x: usize| s[SLOT[x].held] == 1;
                allowed(&model, s, [held(0), held(1)]).len()
            })
            .sum();
        assert_eq!(taken, transitions, "cap {cap}");
        assert_eq!(fired, interp::fired_actions(&model), "cap {cap}");
        assert!(refusals.each_ran(), "cap {cap}: {refusals:?}");
    }
}

/// Dial 1 publishes the second of two candidates prepared against one head once the first has
/// committed (it compares the cursor and forgets the revision). The real boundary refuses that
/// commit as `StaleHead`, and the candidate it consumed is the model's `Discard`.
#[test]
fn negative_control_stale_the_second_candidate_against_one_head_is_refused_where_the_mutant_publishes_it()
 {
    let model = aos_boundary_publication_model();
    let mutant = interp::with_buggy(&model, 1);

    let mut w = scripted(
        &model,
        2,
        &["Pause", "PrepareA", "PrepareB", "CommitA", "Pause"],
    );
    assert!(mutant.action_enabled("CommitB", &w.st));
    assert!(!model.action_enabled("CommitB", &w.st));
    let expected = w.real.slots[1]
        .as_ref()
        .expect("B is held")
        .expected()
        .clone();
    let live = w.real.b.head().clone();
    assert_eq!(expected.input_cursor, live.input_cursor);
    assert_ne!(expected.revision, live.revision);
    let step = w.take(Move::RefusedCommit(1));
    assert_eq!(
        step.refusal,
        Some(PublishRefusal::StaleHead {
            expected: Box::new(expected),
            live: Box::new(live)
        })
    );
    assert_eq!(step.action, "DiscardB");
    let published = successor(&mutant, "CommitB", &step.prev);
    assert!(!mutant.check_invariant("StaleNeverCommitted", &published));
    assert_ne!(step.got, published);

    // Every state the seeded walks reach where only the mutant may commit a held candidate.
    let mut caught = 0u32;
    lockstep(&model, 2, SEEDS, |s| {
        let Move::RefusedCommit(x) = s.mv else { return };
        if !mutant.action_enabled(SLOT[x].commit, &s.prev) {
            return;
        }
        assert!(
            matches!(s.refusal, Some(PublishRefusal::StaleHead { .. })),
            "{:?}",
            s.refusal
        );
        let published = successor(&mutant, SLOT[x].commit, &s.prev);
        assert!(!mutant.check_invariant("StaleNeverCommitted", &published));
        assert_ne!(s.got, published);
        caught += 1;
    });
    assert!(caught > 0, "the walks reached the stale winner");
}

/// Dial 2 publishes the code alone. A commit stopped after any number of its stores leaves the
/// complete old pair (the model's `Discard` of the candidate it consumed) or the complete new
/// pair still paused with the queue whole, which the reject that ends the pause turns into the
/// model's `Commit`; and a commit that completes is the model's `Commit`. None of them is the
/// mutant's new code over old state.
#[cfg(debug_assertions)]
#[test]
fn negative_control_mixed_a_commit_stopped_after_any_store_never_shows_new_code_over_old_state() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    enum Left {
        Old,
        New,
        Done,
    }

    let model = aos_boundary_publication_model();
    let mixed = interp::with_buggy(&model, 2);
    // The scripted commit, then each seed's first commit over a non-empty replay range (the
    // candidate's state differs from the live one, so the state tells old from new too).
    let script: &[&'static str] = &["Input", "Pause", "PrepareA", "Hold", "Hold"];
    let reach = |seed: Option<u64>| match seed {
        None => Some((scripted(&model, 2, script), Move::Act("CommitA"))),
        Some(seed) => first_commit_over_a_range(&model, seed),
    };
    let points: Vec<Option<u64>> = std::iter::once(None)
        .chain(
            (1..=SEAM_SEEDS)
                .filter(|&s| reach(Some(s)).is_some())
                .map(Some),
        )
        .collect();
    assert!(points.len() > SEAM_SEEDS as usize / 2, "{points:?}");

    let mut kinds = BTreeSet::new();
    for &point in &points {
        let mut left = Vec::new();
        for k in 0..=8 {
            let (mut w, mv) = reach(point).expect("the walk is deterministic");
            let Move::Act(action) = mv else {
                unreachable!()
            };
            let x = slot(action);
            let prev = w.st.clone();
            let healthy = successor(&model, action, &prev);
            let mutant = successor(&mixed, action, &prev);
            assert!(!mixed.check_invariant("NeverMixed", &mutant));
            let p = w.real.slots[x].take().expect("the slot holds a candidate");
            assert_ne!(
                p.state(),
                &w.real.b.pair().state,
                "the candidate's state differs from the live one"
            );
            let pending = w.real.pending_commit(&p);
            let (tag, start) = (pending.tag, pending.start.clone());
            let outcome = match w.real.b.commit_stopping_after(p, k) {
                Some(done) => {
                    w.real
                        .finish_commit(pending, done.expect("the candidate commits"));
                    let got = w.real.project();
                    assert_eq!(got, healthy, "k = {k}");
                    assert_eq!(interp::admits(&model, &prev, &got), Some(action));
                    assert_eq!(interp::admits(&mixed, &prev, &got), None, "k = {k}");
                    Left::Done
                }
                None => {
                    let new = w.real.b.pair().code.tag == tag;
                    if new {
                        w.real.gens.push(Gen {
                            tag,
                            start,
                            since: Vec::new(),
                        });
                    }
                    assert!(w.real.b.is_paused(), "k = {k}");
                    assert_eq!(w.real.b.queued(), w.real.queue.len(), "k = {k}");
                    let got = w.real.project();
                    assert!(
                        model.check_invariant("NeverMixed", &got),
                        "k = {k}: {got:?}"
                    );
                    assert_ne!(got, mutant, "k = {k}");
                    if new {
                        let mut whole = healthy.clone();
                        for var in ["paused", "queued", "cur", "delivered"] {
                            whole.insert(var, prev[var]);
                        }
                        assert_eq!(got, whole, "k = {k}: the new pair, the pause not yet over");
                    } else {
                        assert_eq!(
                            interp::admits(&model, &prev, &got),
                            Some(SLOT[x].discard),
                            "k = {k}"
                        );
                    }
                    // Ending the pause delivers the queue to whichever pair is installed.
                    w.real.act("Reject", &mut w.rng);
                    let after = w.real.project();
                    assert_ne!(after, mutant, "k = {k}");
                    if new {
                        assert_eq!(after, healthy, "k = {k}");
                        assert_eq!(interp::admits(&model, &prev, &after), Some(action));
                        Left::New
                    } else {
                        assert_eq!(interp::admits(&model, &got, &after), Some("Reject"));
                        Left::Old
                    }
                }
            };
            left.push(outcome);
        }
        assert!(left.is_sorted(), "old, then new, then done: {left:?}");
        assert_eq!(left.last(), Some(&Left::Done), "{left:?}");
        kinds.extend(left);
    }
    assert_eq!(kinds, BTreeSet::from([Left::Old, Left::New, Left::Done]));
}

/// Dial 3 resumes the old pair on a reject but drops the queue. The real reject delivers every
/// queued input to the old pair, which the mutant does not admit.
#[test]
fn negative_control_lost_input_a_reject_delivers_every_queued_input_to_the_old_pair() {
    let model = aos_boundary_publication_model();
    let mutant = interp::with_buggy(&model, 3);

    let mut w = scripted(&model, 2, &["Pause", "Hold", "Hold"]);
    let step = w.take(Move::Act("Reject"));
    assert_eq!(step.got["cur"], step.prev["cur"] + 2);
    let dropped = successor(&mutant, "Reject", &step.prev);
    assert!(!mutant.check_invariant("EachInputOnce", &dropped));
    assert_eq!(interp::admits(&mutant, &step.prev, &step.got), None);

    let mut caught = 0u32;
    lockstep(&model, 2, SEEDS, |s| {
        if s.action != "Reject" || s.prev["queued"] == 0 {
            return;
        }
        let dropped = successor(&mutant, "Reject", &s.prev);
        assert!(!mutant.check_invariant("EachInputOnce", &dropped));
        assert_eq!(interp::admits(&mutant, &s.prev, &s.got), None);
        caught += 1;
    });
    assert!(caught > 0, "the walks rejected a non-empty queue");
}

/// Dial 4 delivers the queue to the old pair and then installs the candidate's replayed state,
/// which never saw it. The real commit delivers the queue to the new pair, so the live cursor
/// counts it, which the mutant does not admit.
#[test]
fn negative_control_wrong_pair_a_commit_delivers_the_queue_to_the_new_pair() {
    let model = aos_boundary_publication_model();
    let mutant = interp::with_buggy(&model, 4);

    let mut w = scripted(&model, 2, &["Pause", "PrepareA", "Hold"]);
    let step = w.take(Move::Act("CommitA"));
    assert_eq!(step.got["cur"], step.prev["ia"] + 1);
    let misdelivered = successor(&mutant, "CommitA", &step.prev);
    assert_eq!(misdelivered["cur"], step.prev["ia"]);
    assert!(!mutant.check_invariant("DeliveredToTheLivePair", &misdelivered));
    assert_eq!(interp::admits(&mutant, &step.prev, &step.got), None);

    let mut caught = 0u32;
    lockstep(&model, 2, SEEDS, |s| {
        if !s.action.starts_with("Commit") || s.prev["queued"] == 0 {
            return;
        }
        let misdelivered = successor(&mutant, s.action, &s.prev);
        assert!(!mutant.check_invariant("DeliveredToTheLivePair", &misdelivered));
        assert_eq!(interp::admits(&mutant, &s.prev, &s.got), None);
        caught += 1;
    });
    assert!(caught > 0, "the walks committed over a non-empty queue");
}
