// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the lease ledger ([`Ledger`]) to `AosWorkerLease`
//! (`aterm_models_aos::aos_worker_lease_model`, design §11.7): the real ledger, driven with one
//! job, two workers and `max_attempts = MaxAttempts`, takes every step the model takes, in lock
//! step, and the calls the model's guards rule out move nothing.
//!
//! Two walks drive it: seeded random episodes (deterministic xorshift, no host entropy) and an
//! exhaustive walk that replays the model's shortest path to each reachable state on a fresh
//! ledger and takes every move allowed from it. Each `Buggy` dial's negative control runs over
//! the exhaustive walk at every transition its dial touches.
//!
//! The projection reads the job, the attempts (`Counts::leases`), the holder, each worker's
//! state and epoch, the accepted claim, the awaiting late claim ([`Ledger::awaiting`]) and `out`
//! (its own `Complete | Void`) off the ledger. The frames in flight, the one resend and the
//! ghosts come from the harness: `twice` is a second `Effect::Accepted`, `stale` an accept of a
//! lease the harness ticked past its deadline, `post` an accept while the ledger was void, and
//! `ignored` an accepted claim that differs from a claim delivered for comparison while the
//! ledger is not void.
//!
//! Time: the window is short and the grace long, every call advances the clock, and an expiry
//! or drain timeout is a `tick` at exactly that worker's deadline. Drains begin at distinct
//! times and end in that order, which the model's `DrainTimeoutX` guard states; a busy worker's
//! deadline is pushed past another's drain timeout by one progress frame, a call the model does
//! not see (it moves no projected variable).

use std::collections::BTreeSet;

use aterm_aos::Digest;
use aterm_aos::wire::{Dec, Enc};
use aterm_models_aos::aos_worker_lease_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use super::dist::{ClaimKind, JOB_DOMAIN, RawClaim, claim_digest};
use super::lease::{
    ClockBackwards, Drain, Effect, Gone, JobState, Ledger, Limits, Nanos, Outcome, Token, VoidBy,
    WorkerId, WorkerState,
};

/// A lease's window: short, so an expiry never reaches a drain's grace.
const WINDOW: Nanos = 1_000;
/// A drain's grace: long, so no walk's clock reaches it but by a drain timeout.
const GRACE: Nanos = 1_000_000_000_000;
/// The search the walks open.
const SEARCH: u32 = 1;
/// Seeds of the lock-step walk, each run for [`EPISODES`] episodes of at most [`STEPS`] moves.
const SEEDS: u64 = 64;
const EPISODES: u64 = 16;
const STEPS: usize = 40;
/// The two workers, A (honest) and B.
const WORKERS: [WorkerId; 2] = [WorkerId(0), WorkerId(1)];

/// The model's names for one worker.
struct Names {
    state: &'static str,
    epoch: &'static str,
    flight: &'static str,
    lease: &'static str,
    accept: &'static str,
}

const NAMES: [Names; 2] = [
    Names {
        state: "wa",
        epoch: "ea",
        flight: "ra",
        lease: "LeaseA",
        accept: "AcceptA",
    },
    Names {
        state: "wb",
        epoch: "eb",
        flight: "rb",
        lease: "LeaseB",
        accept: "AcceptB",
    },
];

/// What a model action does, by its name's stem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Lease,
    Send,
    Lie,
    Resend,
    Accept,
    Compare,
    Late,
    Drop,
    Expire,
    Lose,
    DrainTimeout,
    Ack,
    Cancel,
    Finish,
}

/// The kind of `action` and the worker it names (`usize::MAX` for none).
fn parse(action: &str) -> (Kind, usize) {
    let (stem, x) = if let Some(stem) = action.strip_suffix('A') {
        (stem, 0)
    } else if let Some(stem) = action.strip_suffix('B') {
        (stem, 1)
    } else {
        (action, usize::MAX)
    };
    let kind = match stem {
        "Lease" => Kind::Lease,
        "Send" => Kind::Send,
        "Lie" => Kind::Lie,
        "Resend" => Kind::Resend,
        "Accept" => Kind::Accept,
        "Compare" => Kind::Compare,
        "Late" => Kind::Late,
        "Drop" => Kind::Drop,
        "Expire" => Kind::Expire,
        "Lose" => Kind::Lose,
        "DrainTimeout" => Kind::DrainTimeout,
        "Ack" => Kind::Ack,
        "Cancel" => Kind::Cancel,
        "Finish" => Kind::Finish,
        _ => panic!("no real call for the model action `{action}`"),
    };
    (kind, x)
}

/// The digest of a claim of `kind` with no rows, for the one job the walks judge.
fn claim(kind: ClaimKind) -> Digest {
    let mut e = Enc::new();
    e.u8(kind.tag());
    e.u64(0);
    let raw = RawClaim::decode(&mut Dec::new(e.as_bytes())).expect("a claim of no rows decodes");
    let job = Digest::of(JOB_DOMAIN, &[b"aos-lease-conformance".as_slice()]);
    claim_digest(&job, &raw)
}

/// The honest claim and the lie (an admitted plan claimed refused).
fn claims() -> (Digest, Digest) {
    (claim(ClaimKind::Admitted), claim(ClaimKind::Refused))
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
}

fn int(n: impl TryInto<i64>) -> i64 {
    n.try_into()
        .unwrap_or_else(|_| panic!("a walk's counts are small"))
}

fn wid(x: usize) -> WorkerId {
    WORKERS[x]
}

/// The model's code for a worker state.
fn worker_code(s: WorkerState) -> i64 {
    match s {
        WorkerState::Idle => 0,
        WorkerState::Busy { .. } => 1,
        WorkerState::Draining {
            why: Drain::Expired,
            ..
        } => 2,
        WorkerState::Draining {
            why: Drain::Cancelled,
            ..
        } => 3,
        WorkerState::Gone(Gone::Eof) => 4,
        WorkerState::Gone(Gone::DrainTimeout) => 5,
        WorkerState::Gone(Gone::Retired) => 6,
        WorkerState::Gone(Gone::Refused) => 7,
        other => panic!("{other:?} is outside the model: every worker said hello"),
    }
}

/// The lease a worker holds or drains.
fn held(s: WorkerState) -> Option<Token> {
    match s {
        WorkerState::Busy { lease, .. } | WorkerState::Draining { lease, .. } => Some(lease),
        _ => None,
    }
}

/// The successor `action` takes `state` to under `model`; the action must be enabled.
fn successor(model: &Model, action: &str, state: &State) -> State {
    let mut next = state.clone();
    assert!(model.fire(action, &mut next), "`{action}` is enabled");
    next
}

/// How often each call the model's guards rule out was made, each moving nothing.
#[derive(Debug, Default)]
struct Probes {
    lease: u32,
    assign: u32,
    early_tick: u32,
    second_void: u32,
    backwards: u32,
    finish: u32,
}

impl Probes {
    fn add(&mut self, o: &Probes) {
        self.lease += o.lease;
        self.assign += o.assign;
        self.early_tick += o.early_tick;
        self.second_void += o.second_void;
        self.backwards += o.backwards;
        self.finish += o.finish;
    }

    fn each_ran(&self) -> bool {
        [
            self.lease,
            self.assign,
            self.early_tick,
            self.second_void,
            self.backwards,
            self.finish,
        ]
        .iter()
        .all(|&n| n > 0)
    }
}

/// The real ledger and what the harness knows of the run.
struct Real {
    l: Ledger,
    now: Nanos,
    truth: Digest,
    lie: Digest,
    /// Each worker's result in flight: its token and claim.
    flight: [Option<(Token, Digest)>; 2],
    resent: bool,
    accepts: u32,
    /// The leases the harness ticked at their deadline.
    expired: BTreeSet<Token>,
    /// Every claim delivered from a worker the ledger reads it from (none crossed a cancel).
    arrived: Vec<Digest>,
    twice: i64,
    stale: i64,
    post: i64,
    ignored: i64,
}

impl Real {
    /// Two workers greeted and one job opened, with `max_attempts` leases.
    fn new(max_attempts: u8) -> Real {
        let mut l = Ledger::new(Limits {
            max_workers: 2,
            hello_ns: GRACE,
            window_ns: WINDOW,
            grace_ns: GRACE,
            max_attempts,
            items: 17,
        });
        for w in WORKERS {
            assert_eq!(l.join(0).expect("clock"), Some(w));
            assert!(l.hello(w, Ok(()), 0).expect("clock").is_empty());
        }
        assert!(l.begin(SEARCH, 1, 0).expect("clock").is_empty());
        let (truth, lie) = claims();
        Real {
            l,
            now: 0,
            truth,
            lie,
            flight: [None, None],
            resent: false,
            accepts: 0,
            expired: BTreeSet::new(),
            arrived: Vec::new(),
            twice: 0,
            stale: 0,
            post: 0,
            ignored: 0,
        }
    }

    fn claim_code(&self, d: Digest) -> i64 {
        if d == self.truth {
            1
        } else if d == self.lie {
            2
        } else {
            panic!("{d:?} is neither walk claim")
        }
    }

    fn worker(&self, x: usize) -> WorkerState {
        self.l.worker(wid(x)).expect("two workers")
    }

    /// The model's variables, read off the real ledger and the harness.
    fn project(&self) -> State {
        let l = &self.l;
        let leases = l.counts().leases;
        let mut s = State::new();
        let job = l.job(0).expect("one job");
        let (code, holder, acc) = match job {
            JobState::Pending { attempts } => {
                assert_eq!(u64::from(attempts), leases, "pending after every lease");
                (0, 0, 0)
            }
            JobState::Leased { lease } => {
                assert_eq!(u64::from(lease.attempt), leases, "the latest lease is live");
                (1, int(lease.worker) + 1, 0)
            }
            JobState::Claimed { lease, digest } => {
                assert_eq!(u64::from(lease.attempt), leases, "no lease after a claim");
                (2, int(lease.worker) + 1, self.claim_code(digest))
            }
            JobState::Cancelled => (3, 0, 0),
            JobState::Exhausted => (4, 0, 0),
        };
        s.insert("job", code);
        s.insert("attempts", int(leases));
        s.insert("holder", holder);
        for (x, names) in NAMES.iter().enumerate() {
            let st = self.worker(x);
            s.insert(names.state, worker_code(st));
            s.insert(names.epoch, held(st).map_or(0, |t| int(t.attempt)));
            s.insert(
                names.flight,
                self.flight[x].map_or(0, |(t, _)| int(t.attempt)),
            );
        }
        s.insert(
            "lb",
            i64::from(self.flight[1].is_some_and(|(_, d)| d == self.lie)),
        );
        s.insert("acc", acc);
        let late = match l.awaiting(0) {
            [] => 0,
            [d] => self.claim_code(*d),
            more => panic!("the model holds one late claim, the ledger {}", more.len()),
        };
        s.insert("late", late);
        s.insert("resent", i64::from(self.resent));
        s.insert(
            "out",
            match l.outcome() {
                Outcome::Open => 0,
                Outcome::Complete => 1,
                Outcome::Void(_) => 2,
                Outcome::Idle => panic!("the walk's search is open"),
            },
        );
        s.insert("twice", self.twice);
        s.insert("stale", self.stale);
        s.insert("post", self.post);
        s.insert("ignored", self.ignored);
        s
    }

    /// The next instant: every call advances the clock, so no two drains begin together.
    fn advance(&mut self) -> Nanos {
        self.now += 1;
        self.now
    }

    /// The real calls a model action names, and the effects the last of them returned.
    fn act(&mut self, action: &str) -> Vec<Effect> {
        let (kind, x) = parse(action);
        let effects = match kind {
            Kind::Lease => {
                let w = wid(x);
                let want = Token {
                    search: SEARCH,
                    slot: 0,
                    attempt: u8::try_from(self.l.counts().leases + 1).expect("few leases"),
                    worker: w.0,
                };
                let lowest_idle = (0..2).find(|&v| self.worker(v) == WorkerState::Idle);
                let now = self.advance();
                let e = if lowest_idle == Some(x) {
                    self.l.assign(now)
                } else {
                    self.l.lease(w, now)
                }
                .expect("clock");
                assert_eq!(e, [Effect::Lease(w, want)]);
                e
            }
            Kind::Send | Kind::Lie | Kind::Resend => {
                let st = self.worker(x);
                let token = match (kind, held(st)) {
                    (Kind::Send | Kind::Lie, Some(lease)) => lease,
                    (Kind::Resend | Kind::Lie, None) => {
                        assert_eq!(st, WorkerState::Idle, "a resend is the claim holder's");
                        let Some(JobState::Claimed { lease, .. }) = self.l.job(0) else {
                            panic!("a resend of a claim not accepted")
                        };
                        assert!(!self.resent, "one resend");
                        self.resent = true;
                        lease
                    }
                    other => panic!("`{action}` from {st:?}: {other:?}"),
                };
                let digest = if kind == Kind::Lie {
                    self.lie
                } else {
                    self.truth
                };
                assert!(self.flight[x].is_none(), "one frame in flight per worker");
                self.flight[x] = Some((token, digest));
                Vec::new()
            }
            Kind::Accept | Kind::Compare | Kind::Late | Kind::Drop => {
                let (token, digest) = self.flight[x].take().expect("a result in flight");
                let was_void = matches!(self.l.outcome(), Outcome::Void(_));
                let now = self.advance();
                let e = self.l.result(wid(x), token, digest, now).expect("clock");
                if kind != Kind::Drop {
                    self.arrived.push(digest);
                }
                for effect in &e {
                    if let Effect::Accepted(_, t) = *effect {
                        self.accepts += 1;
                        if self.accepts > 1 {
                            self.twice = 1;
                        }
                        if self.expired.contains(&t) {
                            self.stale = 1;
                        }
                        if was_void {
                            self.post = 1;
                        }
                    }
                }
                e
            }
            Kind::Expire => {
                let WorkerState::Busy {
                    lease, deadline, ..
                } = self.worker(x)
                else {
                    panic!("`{action}` of a worker not busy")
                };
                self.now = (self.now + 1).max(deadline);
                self.expired.insert(lease);
                self.l.tick(self.now).expect("clock")
            }
            Kind::DrainTimeout => {
                let WorkerState::Draining { until, .. } = self.worker(x) else {
                    panic!("`{action}` of a worker not draining")
                };
                if let WorkerState::Busy { lease, k, .. } = self.worker(1 - x) {
                    let before = self.project();
                    self.now = (self.now + 1).max(until - 1);
                    let next = k.map_or(0, |k| k + 1);
                    let e = self
                        .l
                        .progress(wid(1 - x), lease, next, self.now)
                        .expect("clock");
                    assert!(e.is_empty(), "progress from the holder: {e:?}");
                    assert_eq!(self.project(), before, "progress moves nothing modelled");
                }
                self.now = (self.now + 1).max(until);
                self.l.tick(self.now).expect("clock")
            }
            Kind::Ack => {
                let lease = held(self.worker(x)).expect("a drained lease");
                let now = self.advance();
                self.l.cancelled(wid(x), lease, now).expect("clock")
            }
            Kind::Lose => {
                let now = self.advance();
                self.l.lost(wid(x), now).expect("clock")
            }
            Kind::Cancel => {
                let now = self.advance();
                self.l.void(VoidBy::Interrupted, now).expect("clock")
            }
            Kind::Finish => {
                let now = self.advance();
                self.l.finish(now).expect("clock");
                Vec::new()
            }
        };
        for x in 0..2 {
            if matches!(self.worker(x), WorkerState::Gone(_)) {
                // Killed, and its reader stopped: a frame still in flight is never read.
                self.flight[x] = None;
            }
        }
        if let Some(JobState::Claimed { digest, .. }) = self.l.job(0)
            && self.arrived.iter().any(|d| *d != digest)
            && !matches!(self.l.outcome(), Outcome::Void(_))
        {
            self.ignored = 1;
        }
        effects
    }

    /// The calls the model's guards rule out in `st`, each moving no projected variable and no
    /// counter: a lease to a worker that may take none, an assignment with none to make, a tick
    /// before any deadline, a second void, an earlier `now`, and a finish before completion.
    fn probe(&mut self, model: &Model, st: &State, seen: &mut Probes) {
        let before = self.project();
        let counts = self.l.counts();
        let now = self.now;
        for (x, names) in NAMES.iter().enumerate() {
            if !model.action_enabled(names.lease, st) {
                assert!(self.l.lease(wid(x), now).expect("clock").is_empty());
                seen.lease += 1;
            }
        }
        if !NAMES.iter().any(|n| model.action_enabled(n.lease, st)) {
            assert!(self.l.assign(now).expect("clock").is_empty());
            seen.assign += 1;
        }
        if self.l.next_deadline().is_none_or(|d| d > now) {
            assert!(self.l.tick(now).expect("clock").is_empty());
            seen.early_tick += 1;
        }
        if st["out"] == 2 {
            assert!(
                self.l
                    .void(VoidBy::Interrupted, now)
                    .expect("clock")
                    .is_empty()
            );
            assert!(self.l.void(VoidBy::Caller, now).expect("clock").is_empty());
            seen.second_void += 1;
        }
        if let Some(earlier) = now.checked_sub(1) {
            let refused = ClockBackwards {
                latest: now,
                given: earlier,
            };
            assert_eq!(self.l.tick(earlier), Err(refused));
            assert_eq!(self.l.lease(wid(0), earlier), Err(refused));
            assert_eq!(self.l.lost(wid(1), earlier), Err(refused));
            assert_eq!(self.l.void(VoidBy::Interrupted, earlier), Err(refused));
            assert_eq!(self.l.finish(earlier), Err(refused));
            seen.backwards += 1;
        }
        if !model.action_enabled("Finish", st) {
            self.l.finish(now).expect("clock");
            seen.finish += 1;
        }
        assert_eq!(
            self.project(),
            before,
            "a ruled-out call moves no projected variable"
        );
        assert_eq!(self.l.counts(), counts, "nor any counter");
    }
}

/// One move taken in lock step.
struct Step {
    action: &'static str,
    prev: State,
    got: State,
}

/// One episode of the walk: the real ledger, the model's state, and the stream that picks.
struct Walker<'m> {
    model: &'m Model,
    rng: Rng,
    real: Real,
    st: State,
    probes: Probes,
    at: String,
}

impl<'m> Walker<'m> {
    fn new(model: &'m Model, max_attempts: u8, seed: u64, episode: u64) -> Self {
        let real = Real::new(max_attempts);
        let st = model.init_state();
        let at = format!("MaxAttempts {max_attempts} seed {seed} episode {episode}");
        assert_eq!(
            real.project(),
            st,
            "{at}: the opened search is the model's initial state"
        );
        Walker {
            model,
            rng: Rng::new(seed, episode),
            real,
            st,
            probes: Probes::default(),
            at,
        }
    }

    /// Makes the calls the model rules out here, then picks an action the model allows.
    fn next_move(&mut self) -> Option<&'static str> {
        self.real.probe(self.model, &self.st, &mut self.probes);
        let moves = allowed(self.model, &self.st);
        if moves.is_empty() {
            return None;
        }
        Some(moves[self.rng.below(moves.len())])
    }

    /// Takes `action` on the real ledger and on the model, then checks that the projection is
    /// the model's state, that the model admits the real step by that name, and that every
    /// worker the step removed was killed and every holder it drained was sent `Cancel`.
    fn take(&mut self, action: &'static str) -> Step {
        let effects = self.real.act(action);
        let prev = self.st.clone();
        assert!(
            self.model.fire(action, &mut self.st),
            "{}: `{action}` is enabled",
            self.at
        );
        let got = self.real.project();
        assert_eq!(got, self.st, "{}: `{action}` from {prev:?}", self.at);
        assert_eq!(
            interp::admits(self.model, &prev, &got),
            Some(action),
            "{}: the model admits the real step `{action}`, by name",
            self.at
        );
        for (x, names) in NAMES.iter().enumerate() {
            let (before, after) = (prev[names.state], got[names.state]);
            let w = wid(x);
            assert_eq!(
                effects.contains(&Effect::Kill(w)),
                before <= 3 && after >= 4,
                "{}: `{action}` kills worker {x} exactly when it leaves: {effects:?}",
                self.at
            );
            let cancelled = effects
                .iter()
                .any(|e| matches!(e, Effect::Cancel(v, _) if *v == w));
            assert_eq!(
                cancelled,
                before == 1 && (after == 2 || after == 3),
                "{}: `{action}` sends Cancel to worker {x} exactly when it drains: {effects:?}",
                self.at
            );
        }
        Step { action, prev, got }
    }
}

/// The actions the model allows in `st`, in model order.
fn allowed(model: &Model, st: &State) -> Vec<&'static str> {
    model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| model.action_enabled(a, st))
        .collect()
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

/// The model at `max_attempts`, the configurations the walks run.
fn config(max_attempts: u8) -> Model {
    interp::with_consts(
        &aos_worker_lease_model(),
        &[("MaxAttempts", i64::from(max_attempts))],
    )
}

/// Every seeded episode of the walk at `max_attempts`; `each` sees every move taken.
fn lockstep(max_attempts: u8, mut each: impl FnMut(&Step)) -> Probes {
    let model = config(max_attempts);
    let mut probes = Probes::default();
    for seed in 1..=SEEDS {
        for episode in 0..EPISODES {
            let mut w = Walker::new(&model, max_attempts, seed, episode);
            for _ in 0..STEPS {
                let Some(action) = w.next_move() else { break };
                each(&w.take(action));
            }
            probes.add(&w.probes);
        }
    }
    probes
}

/// A walker that took `actions` in order from the initial state, at the committed config.
fn scripted<'m>(model: &'m Model, actions: &[&'static str]) -> Walker<'m> {
    let mut w = Walker::new(model, 2, 1, 0);
    for a in actions {
        w.take(a);
    }
    w
}

#[test]
fn the_real_ledger_follows_the_lease_model_in_lock_step() {
    for max_attempts in [2u8, 1] {
        let model = config(max_attempts);
        let mut fired = BTreeSet::new();
        let probes = lockstep(max_attempts, |s| {
            fired.insert(s.action);
        });
        assert_eq!(
            fired,
            interp::fired_actions(&model),
            "MaxAttempts {max_attempts}: the real ledger took every action the model can take"
        );
        assert!(
            probes.each_ran(),
            "MaxAttempts {max_attempts}: every ruled-out call was made: {probes:?}"
        );
    }
}

/// Every transition of the model's bounded space at `max_attempts`, each on a fresh ledger: the
/// model's shortest path to its state replayed, the calls the model rules out there made, then
/// `each` given the walker and the action to take. Returns the probes made and the transitions.
fn every_transition(
    max_attempts: u8,
    mut each: impl FnMut(&mut Walker<'_>, &'static str),
) -> (Probes, usize) {
    let model = config(max_attempts);
    let tree = shortest_paths(&model);
    let reachable: BTreeSet<State> = interp::reachable_states(&model).into_iter().collect();
    assert_eq!(
        tree.iter().map(|(s, _)| s.clone()).collect::<BTreeSet<_>>(),
        reachable,
        "MaxAttempts {max_attempts}"
    );
    let mut probes = Probes::default();
    let mut taken = 0usize;
    for (i, (state, path)) in tree.iter().enumerate() {
        for action in allowed(&model, state) {
            let mut w = Walker::new(&model, max_attempts, 1, 0);
            w.at = format!("MaxAttempts {max_attempts} state {i} by {path:?}");
            for a in path {
                w.take(a);
            }
            assert_eq!(&w.st, state, "{}", w.at);
            w.at = format!("{}, then {action}", w.at);
            w.real.probe(&model, &w.st, &mut w.probes);
            each(&mut w, action);
            probes.add(&w.probes);
            taken += 1;
        }
    }
    let transitions: usize = reachable.iter().map(|s| allowed(&model, s).len()).sum();
    assert_eq!(taken, transitions, "MaxAttempts {max_attempts}");
    (probes, taken)
}

/// The seeded walks reach only part of the model's states. This replays the model's shortest
/// path to each reachable state on a fresh ledger, makes the calls the model rules out there,
/// then takes each move the model allows from it, so every transition of the bounded space is a
/// checked real step.
#[test]
fn every_transition_of_the_lease_model_is_a_real_step() {
    for max_attempts in [2u8, 1] {
        let mut fired = BTreeSet::new();
        let (probes, _) = every_transition(max_attempts, |w, action| {
            fired.insert(w.take(action).action);
        });
        assert_eq!(
            fired,
            interp::fired_actions(&config(max_attempts)),
            "MaxAttempts {max_attempts}"
        );
        assert!(probes.each_ran(), "MaxAttempts {max_attempts}: {probes:?}");
    }
}

/// Dial 1 (`ExpireKeepsLease`, an action the healthy model does not have) drains the holder and
/// keeps its job leased. At every expiry of the bounded space, the mutant admits that step and
/// the healthy model does not; the real ledger, ticked just before the deadline, moves nothing,
/// and ticked at it, releases the job (the healthy `ExpireX`), never the mutant's state.
#[test]
fn negative_control_expire_keeps_lease_the_real_tick_releases_the_job() {
    let mutant = interp::with_buggy(&config(2), 1);
    let mut caught = 0u32;
    every_transition(2, |w, action| {
        let (kind, x) = parse(action);
        if kind != Kind::Expire {
            w.take(action);
            return;
        }
        let prev = w.st.clone();
        let kept = successor(&mutant, "ExpireKeepsLease", &prev);
        assert!(!mutant.check_invariant("LeasedByBusy", &kept));
        assert_eq!(interp::admits(w.model, &prev, &kept), None, "{}", w.at);
        assert_eq!(
            interp::admits(&mutant, &prev, &kept),
            Some("ExpireKeepsLease")
        );
        let WorkerState::Busy { deadline, .. } = w.real.worker(x) else {
            panic!("an expiry of a busy worker")
        };
        assert!(deadline > w.real.now + 1, "{}: the window is open", w.at);
        w.real.now = deadline - 1;
        assert!(w.real.l.tick(w.real.now).expect("clock").is_empty());
        assert_eq!(w.real.project(), prev, "{}: before the deadline", w.at);
        let step = w.take(action);
        assert_ne!(step.got, kept);
        assert_ne!(step.got["job"], 1, "{}: the job is not kept", w.at);
        assert_eq!(interp::admits(&mutant, &prev, &step.got), Some(action));
        caught += 1;
    });
    assert!(caught > 0, "the space has an expiry");
}

/// The negative control of a dial that changes an existing action's update: at every transition
/// of the bounded space `applies` picks, the healthy model admits the real step (checked by
/// `take`), the mutant does not, and the mutant's own step from the same state breaks `law`.
/// Returns how many it caught.
fn changed_update_control(dial: i64, law: &str, applies: impl Fn(&Step) -> bool) -> u32 {
    let mutant = interp::with_buggy(&config(2), dial);
    let mut caught = 0u32;
    every_transition(2, |w, action| {
        let s = w.take(action);
        if !applies(&s) {
            return;
        }
        assert_eq!(
            interp::admits(&mutant, &s.prev, &s.got),
            None,
            "Buggy {dial}: `{}` from {:?}",
            s.action,
            s.prev
        );
        let wrong = successor(&mutant, s.action, &s.prev);
        assert!(
            !mutant.check_invariant(law, &wrong),
            "Buggy {dial}: {wrong:?}"
        );
        caught += 1;
    });
    caught
}

/// Whether the claim in flight from `s.action`'s worker differs from `against` (1 truth, 2 lie).
fn differs(s: &Step, against: i64) -> bool {
    let x = parse(s.action).1;
    let mine = if x == 1 { s.prev["lb"] + 1 } else { 1 };
    against != 0 && mine != against
}

/// Dial 2 accepts an equal resend again. The real ledger counts it and changes nothing.
#[test]
fn negative_control_duplicate_an_equal_resend_is_counted_not_accepted() {
    let model = config(2);
    let mutant = interp::with_buggy(&model, 2);
    let mut w = scripted(&model, &["LeaseB", "SendB", "AcceptB", "ResendB"]);
    let duplicates = w.real.l.counts().duplicate;
    let step = w.take("CompareB");
    assert_eq!(w.real.l.counts().duplicate, duplicates + 1);
    assert_eq!(interp::admits(&mutant, &step.prev, &step.got), None);
    assert!(!mutant.check_invariant(
        "NoDuplicateAccept",
        &successor(&mutant, "CompareB", &step.prev)
    ));

    let caught = changed_update_control(2, "NoDuplicateAccept", |s| {
        parse(s.action).0 == Kind::Compare && !differs(s, s.prev["acc"])
    });
    assert!(caught > 0, "the space compares an equal resend");
}

/// Dial 3 takes a late result for a job back in `Pending` as its claim. The real ledger keeps it
/// waiting for the job's claim and retires its sender.
#[test]
fn negative_control_stale_a_late_result_for_an_open_job_waits_for_its_claim() {
    let model = config(2);
    let mutant = interp::with_buggy(&model, 3);
    let mut w = scripted(&model, &["LeaseA", "SendA", "ExpireA"]);
    let step = w.take("LateA");
    assert_eq!(w.real.l.awaiting(0), [w.real.truth]);
    assert_eq!(w.real.l.job(0), Some(JobState::Pending { attempts: 1 }));
    assert_eq!(interp::admits(&mutant, &step.prev, &step.got), None);
    assert!(!mutant.check_invariant("NoStaleAccept", &successor(&mutant, "LateA", &step.prev)));

    let caught = changed_update_control(3, "NoStaleAccept", |s| {
        parse(s.action).0 == Kind::Late && s.prev["job"] == 0
    });
    assert!(
        caught > 0,
        "the space delivers a late result to a pending job"
    );
}

/// Dial 4's interrupt leaves the holder holding, so its result is then accepted. The real
/// interrupt drains the holder, and that result is dropped.
#[test]
fn negative_control_post_an_interrupt_drains_the_holder_and_drops_its_result() {
    let model = config(2);
    let mutant = interp::with_buggy(&model, 4);
    let mut w = scripted(&model, &["LeaseA", "SendA"]);
    let step = w.take("Cancel");
    assert_eq!(interp::admits(&mutant, &step.prev, &step.got), None);
    let holding = successor(&mutant, "Cancel", &step.prev);
    assert!(!mutant.check_invariant(
        "NoAcceptAfterCancel",
        &successor(&mutant, "AcceptA", &holding)
    ));
    let dropped = w.real.l.counts().dropped;
    w.take("DropA");
    assert_eq!(w.real.l.counts().dropped, dropped + 1);
    assert_eq!(w.real.accepts, 0);

    let mut caught = 0u32;
    every_transition(2, |w, action| {
        let s = w.take(action);
        if s.action != "Cancel" || s.prev["job"] != 1 {
            return;
        }
        assert_eq!(interp::admits(&mutant, &s.prev, &s.got), None);
        let holding = successor(&mutant, "Cancel", &s.prev);
        let x = usize::try_from(s.prev["holder"] - 1).expect("a holder");
        assert_eq!(
            holding[NAMES[x].state], 1,
            "the mutant's holder still holds"
        );
        if mutant.action_enabled(NAMES[x].accept, &holding) {
            let accepted = successor(&mutant, NAMES[x].accept, &holding);
            assert!(!mutant.check_invariant("NoAcceptAfterCancel", &accepted));
            caught += 1;
        }
    });
    assert!(
        caught > 0,
        "the space interrupts a holder with its result in flight"
    );
}

/// Dial 5 drops a differing resend or late result, or a differing awaiting late claim, without
/// comparing it. The real ledger compares each and voids.
#[test]
fn negative_control_ignored_every_differing_claim_is_compared_and_voids() {
    let model = config(2);
    let mutant = interp::with_buggy(&model, 5);
    for (path, last) in [
        (
            ["LeaseB", "SendB", "AcceptB", "LieB"].as_slice(),
            "CompareB",
        ),
        (
            ["LeaseA", "SendA", "ExpireA", "LeaseB", "LieB", "AcceptB"].as_slice(),
            "LateA",
        ),
        (
            ["LeaseA", "SendA", "ExpireA", "LateA", "LeaseB", "LieB"].as_slice(),
            "AcceptB",
        ),
    ] {
        let mut w = scripted(&model, path);
        let step = w.take(last);
        assert!(matches!(w.real.l.outcome(), Outcome::Void(_)), "{last}");
        assert_eq!(interp::admits(&mutant, &step.prev, &step.got), None);
        assert!(!mutant.check_invariant(
            "NoContradictionIgnored",
            &successor(&mutant, last, &step.prev)
        ));
    }

    let caught = changed_update_control(5, "NoContradictionIgnored", |s| {
        let (kind, _) = parse(s.action);
        s.prev["out"] <= 1
            && match kind {
                Kind::Compare => differs(s, s.prev["acc"]),
                Kind::Late => s.prev["job"] == 2 && differs(s, s.prev["acc"]),
                Kind::Accept => differs(s, s.prev["late"]),
                _ => false,
            }
    });
    assert!(caught > 0, "the space delivers a differing claim");
}

/// Dial 6 re-leases a job past its last attempt to the holder whose lease expired. The real
/// ledger exhausts the job and voids.
#[test]
fn negative_control_attempts_the_last_expiry_exhausts_the_job() {
    let model = config(2);
    let mutant = interp::with_buggy(&model, 6);
    let mut w = scripted(&model, &["LeaseA", "ExpireA", "LeaseB"]);
    let step = w.take("ExpireB");
    assert_eq!(w.real.l.job(0), Some(JobState::Exhausted));
    assert!(matches!(w.real.l.outcome(), Outcome::Void(_)));
    assert_eq!(interp::admits(&mutant, &step.prev, &step.got), None);
    assert!(!mutant.check_invariant(
        "AttemptsBounded",
        &successor(&mutant, "ExpireB", &step.prev)
    ));

    let caught = changed_update_control(6, "AttemptsBounded", |s| {
        parse(s.action).0 == Kind::Expire && s.prev["attempts"] == 2
    });
    assert!(caught > 0, "the space expires a last lease");
}
