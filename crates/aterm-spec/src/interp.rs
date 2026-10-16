// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! In-process model checking: exhaustive bounded checking of [`crate::derive::Model`]s through
//! the embedded interpreter, with no external toolchain.
//!
//! [`bmc`] enumerates the whole bounded reachable space by BFS over [`Model::successors`]
//! (which fans out the nondeterministic `\in lo..hi` picks as `ty`'s existential search does)
//! and checks every invariant at every state; [`find_deadlock`] is the twin of `ty`'s
//! `CHECK_DEADLOCK`; [`find_nonprogress_cycle`] is the twin of a temporal `[]<>goal` under
//! `WF_vars`/`SF_vars` fairness, the check that sees a livelock; [`admits`] is the twin of a
//! two-step `ty trace validate` (does `Next` admit a real `prev -> next`?).
//!
//! This tier is the default discharge path for every derived-model obligation, so a fresh clone
//! verifies with no toolchain; the external `ty` binary is the escalation tier wherever it is
//! installed (see [`crate::verify`]'s tiered helpers). Both check the same model, and a
//! disagreement is a checker bug that panics.
//!
//! Scalar models only: function-valued models (`Model::fn_vars` non-empty) are
//! TLA+-generation-only, since the integer interpreter cannot evaluate them (`Expr` panics by
//! design). Callers gate on `m.fn_vars.is_empty()`; the tiered helpers in [`crate::verify`] do.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

use crate::compiled::Compiled;
use crate::derive::{Liveness, Model, Value};

/// One concrete interpreter state: every scalar variable's value.
pub type State = BTreeMap<&'static str, i64>;

/// A reached state's identity in a `seen` set: its values, in sorted-name order. Every state of
/// one model binds the same variables, so the names add nothing to the identity but a string
/// compare per probe.
fn state_key(s: &State) -> Vec<i64> {
    s.values().copied().collect()
}

/// A walk's reached states, by [`state_key`], each numbered in the order first seen. Only
/// membership and that number are asked, so it is a hash table keyed by [`key_hash`] (cheaper
/// than `[i64]`'s byte-fed `Hash`). Keys sharing a hash are chained through `next` and told
/// apart by comparing the keys, so a collision costs a comparison, never a wrong answer.
#[derive(Default)]
struct Seen {
    /// Hash -> the most recently numbered key with that hash.
    head: HashMap<u64, usize, BuildHasherDefault<PassThrough>>,
    /// Number -> key.
    keys: Vec<Vec<i64>>,
    /// Number -> the previously numbered key with the same hash, if any.
    next: Vec<Option<usize>>,
}

impl Seen {
    /// The number `key` was given, if it has been seen.
    // Skip: hash-table probe and chain indexing over the walk's own keys (absent std bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn find(&self, key: &[i64]) -> Option<usize> {
        let mut at = self.head.get(&key_hash(key)).copied();
        while let Some(i) = at {
            if self.keys[i] == key {
                return Some(i);
            }
            at = self.next[i];
        }
        None
    }

    /// Number `key`, which must not have been seen; returns its number.
    // Skip: same class as `find`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn add(&mut self, key: Vec<i64>) -> usize {
        let id = self.keys.len();
        let h = key_hash(&key);
        self.next.push(self.head.insert(h, id));
        self.keys.push(key);
        id
    }
}

/// A state key's hash: a multiply-rotate mix of each value (the `FxHash`
/// construction). Keys are small integer vectors from the walk itself, never
/// adversarial input, so a fast mix serves better than `SipHash`'s flood
/// resistance.
fn key_hash(key: &[i64]) -> u64 {
    let mut h = key.len() as u64;
    for &v in key {
        h = (h.rotate_left(5) ^ v as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    h
}

/// The [`Seen`] table's hasher: its keys are already [`key_hash`]es.
#[derive(Default)]
struct PassThrough(u64);

impl Hasher for PassThrough {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        // Only `u64` keys reach this table; any other input is still hashed.
        for &byte in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// One walk's view of a model: how it starts, steps, checks and names a state.
///
/// [`ByName`] evaluates the model's expressions as written over `BTreeMap` states
/// ([`Model::successors_in`], [`Model::check_invariant_in`]); [`Compiled`] evaluates the same
/// expressions resolved once to slice indices (its module docs say why they agree). Every walk
/// below is written once over this trait and runs compiled whenever the model can be, which is
/// every model whose updates write only declared variables.
trait Walker {
    /// A state as the walk holds it.
    type S: Clone + PartialEq;
    /// What every action and invariant at one state reads (loop-invariant
    /// across them, so built once per popped state — see `bmc`).
    type Env;
    fn init(&self) -> Self::S;
    fn env(&self, s: &Self::S) -> Self::Env;
    /// Whether invariant `i` holds at `s`.
    fn holds(&self, env: &Self::Env, s: &Self::S, i: usize) -> bool;
    /// Every successor of `s` under action `a`, in [`Model::successors_in`]'s order.
    fn successors(&self, env: &Self::Env, s: &Self::S, a: usize) -> Vec<Self::S>;
    /// `s`'s number in `seen`, if it has been seen.
    fn lookup(&self, seen: &Seen, s: &Self::S) -> Option<usize>;
    /// `s`'s [`state_key`].
    fn key(&self, s: &Self::S) -> Vec<i64>;
    /// Record `s` in `seen`; `true` if it is new.
    fn first_sight(&self, seen: &mut Seen, s: &Self::S) -> bool {
        if self.lookup(seen, s).is_some() {
            return false;
        }
        seen.add(self.key(s));
        true
    }
    /// `s` as the public [`State`].
    fn state(&self, s: &Self::S) -> State;
}

/// The model evaluated as written: [`Walker`] over `BTreeMap` states.
struct ByName<'m>(&'m Model);

impl Walker for ByName<'_> {
    type S = State;
    type Env = BTreeMap<&'static str, i64>;
    fn init(&self) -> State {
        self.0.init_state()
    }
    fn env(&self, s: &State) -> Self::Env {
        self.0.eval_env(s)
    }
    // Skip: indexes the model's declarations by the walk's counter and drives the skipped
    // `check_invariant_in`/`successors_in`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn holds(&self, env: &Self::Env, _: &State, i: usize) -> bool {
        self.0.check_invariant_in(&self.0.invariants[i], env)
    }
    // Skip: same class as `holds` above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn successors(&self, env: &Self::Env, s: &State, a: usize) -> Vec<State> {
        self.0.successors_in(&self.0.actions[a], env, s)
    }
    fn lookup(&self, seen: &Seen, s: &State) -> Option<usize> {
        seen.find(&state_key(s))
    }
    fn key(&self, s: &State) -> Vec<i64> {
        state_key(s)
    }
    fn state(&self, s: &State) -> State {
        s.clone()
    }
}

/// The model resolved once: [`Walker`] over value slices in sorted-name order,
/// which is exactly the [`state_key`] identity.
impl Walker for Compiled {
    type S = Vec<i64>;
    type Env = ();
    fn init(&self) -> Vec<i64> {
        Compiled::init(self)
    }
    fn env(&self, _: &Vec<i64>) {}
    fn holds(&self, (): &(), s: &Vec<i64>, i: usize) -> bool {
        Compiled::holds(self, i, s)
    }
    fn successors(&self, (): &(), s: &Vec<i64>, a: usize) -> Vec<Vec<i64>> {
        Compiled::successors(self, a, s)
    }
    fn lookup(&self, seen: &Seen, s: &Vec<i64>) -> Option<usize> {
        // By borrow: a revisit, the common case, allocates nothing.
        seen.find(s)
    }
    fn key(&self, s: &Vec<i64>) -> Vec<i64> {
        s.clone()
    }
    fn state(&self, s: &Vec<i64>) -> State {
        Compiled::state(self, s)
    }
}

/// A model resolved once, for a caller that walks its space itself (a record asserted at each
/// step, a law that must break alone, a reachability question no invariant states): the form
/// [`bmc`] and every walk here run on.
///
/// A state is its values in sorted-name order ([`State`]'s iteration order, so
/// `state.values().copied().collect::<Vec<_>>()` is the same slice), and a variable is its
/// [`Resolved::slot`]. [`Resolved::successors`] and [`Resolved::holds`] are
/// [`Model::successors_in`] and [`Model::check_invariant_in`] over `eval_env(state)`, case for
/// case and in the same order, without the string-keyed map every by-name step builds.
pub struct Resolved(Compiled);

impl Resolved {
    /// Resolve `m`; `None` for a model whose updates write a name its initial
    /// state does not bind (a `State` that grows a key has no fixed slice).
    #[must_use]
    pub fn new(m: &Model) -> Option<Self> {
        Compiled::new(m).map(Self)
    }

    /// The initial state.
    #[must_use]
    pub fn init(&self) -> Vec<i64> {
        self.0.init()
    }

    /// The index of state variable `name` in a state slice.
    ///
    /// # Panics
    ///
    /// If `name` is not one of the model's state variables.
    #[must_use]
    // Skip: the deliberate fail-closed panic on a name that is no state variable.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn slot(&self, name: &str) -> usize {
        self.0
            .slot(name)
            .unwrap_or_else(|| panic!("`{name}` is not a state variable"))
    }

    /// Every successor of `s` under the model's `a`-th action
    /// ([`Model::successors_in`]: the same states, in the same order).
    #[must_use]
    pub fn successors(&self, a: usize, s: &[i64]) -> Vec<Vec<i64>> {
        self.0.successors(a, s)
    }

    /// Whether the model's `i`-th invariant holds at `s`
    /// ([`Model::check_invariant_in`]).
    #[must_use]
    pub fn holds(&self, i: usize, s: &[i64]) -> bool {
        self.0.holds(i, s)
    }

    /// `s` as a [`State`].
    #[must_use]
    pub fn state(&self, s: &[i64]) -> State {
        self.0.state(s)
    }
}

/// Run `$walk` (a generic fn over a [`Walker`]) on `$m`'s compiled form, or on
/// the model as written when it has none.
macro_rules! walk {
    ($walk:ident($m:expr $(, $arg:expr)*)) => {
        match Compiled::new($m) {
            Some(c) => $walk($m, &c $(, $arg)*),
            None => $walk($m, &ByName($m) $(, $arg)*),
        }
    };
}

/// Guard against a runaway state space: every derived model is bounded by construction, so
/// crossing this many reachable states means the bounds regressed.
const MAX_STATES: usize = 100_000;

/// The BFS drivers evaluate the `&Action`/`&Invariant` they hold rather than re-resolving each
/// by name ([`Model::successors`] / [`Model::check_invariant`] find the first record with that
/// name), which agrees only when names are distinct within a model: with a duplicate, the
/// by-name form would evaluate the first record twice and never the second. Debug builds
/// assert it.
#[cfg_attr(trust_verify, trust::skip)]
fn names_are_unique(m: &Model) -> bool {
    let actions: BTreeSet<&'static str> = m.actions.iter().map(|a| a.name).collect();
    let invariants: BTreeSet<&'static str> = m.invariants.iter().map(|i| i.name).collect();
    actions.len() == m.actions.len() && invariants.len() == m.invariants.len()
}

/// A copy of `m` with the named constants overridden (the interpreter reads
/// constants from `m.consts`, so this is the interpreter analogue of a `.cfg`
/// override list). Unknown names are ignored, like `to_cfg_with`.
#[must_use]
pub fn with_consts(m: &Model, overrides: &[(&str, i64)]) -> Model {
    let mut m = m.clone();
    for c in &mut m.consts {
        if let Some((_, v)) = overrides.iter().find(|(n, _)| *n == c.0) {
            c.1 = *v;
        }
    }
    m
}

/// A copy of `m` with its `Buggy` constant set to `b` — the prove-and-catch
/// protocol's variant flip. A model without a `Buggy` constant is returned
/// unchanged.
#[must_use]
pub fn with_buggy(m: &Model, b: i64) -> Model {
    with_consts(m, &[("Buggy", b)])
}

/// Exhaustive bounded model check: BFS the reachable state space over every action, checking
/// every invariant at every state. `Ok(n_states)` if all hold everywhere, else
/// `Err((violating_state, invariant_name))` at the first violation.
///
/// # Panics
///
/// If the reachable space exceeds [`MAX_STATES`] (a model-bounds regression,
/// never a property of a healthy derived model).
// Skip: the bounded model-check driver — keyed frontier and caller-chosen closures (absent std
// bodies); spec-model machinery.
#[cfg_attr(trust_verify, trust::skip)]
pub fn bmc(m: &Model) -> Result<usize, (State, &'static str)> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(bmc_on(m))
}

// Skip: same BFS driver class as `bmc`, whose body this is.
#[cfg_attr(trust_verify, trust::skip)]
fn bmc_on<W: Walker>(m: &Model, w: &W) -> Result<usize, (State, &'static str)> {
    let mut seen = Seen::default();
    let mut q: VecDeque<W::S> = VecDeque::new();
    let init = w.init();
    w.first_sight(&mut seen, &init);
    q.push_back(init);
    let mut n = 0usize;
    while let Some(st) = q.pop_front() {
        n += 1;
        assert!(
            n < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        // The env is a pure function of (consts, pre-state), so it is built once per state
        // rather than once per invariant and per action.
        let env = w.env(&st);
        for (i, inv) in m.invariants.iter().enumerate() {
            if !w.holds(&env, &st, i) {
                return Err((w.state(&st), inv.name));
            }
        }
        for a in 0..m.actions.len() {
            for ns in w.successors(&env, &st, a) {
                if w.first_sight(&mut seen, &ns) {
                    q.push_back(ns);
                }
            }
        }
    }
    Ok(n)
}

/// The prove-and-catch protocol (the `Buggy` convention), interpreter tier: the invariants hold
/// across the whole bounded space at `Buggy = 0`, and a counterexample is reachable at
/// `Buggy = 1`, so the property is both true and non-trivial.
///
/// Returns the number of reachable states proved at `Buggy = 0`, the yardstick the `ty` tier's
/// explored space is held to (`verify::prove_and_catch_tiered`).
///
/// # Panics
///
/// On an invariant violation at `Buggy = 0`, or a vacuous property (no counterexample at
/// `Buggy = 1`) — the same failures the `ty` tier fails.
// Skip: the prove/catch driver — keyed-frontier walks and deliberate harness asserts (a
// violated model must fail loudly), the same tier as `bmc`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch(m: &Model) -> usize {
    let proven = match bmc(&with_buggy(m, 0)) {
        Ok(n) => {
            eprintln!(
                "{}: invariant proven over {n} reachable states (interpreter, Buggy=0).",
                m.name
            );
            n
        }
        Err((st, inv)) => panic!("{} invariant `{inv}` VIOLATED at {st:?} (Buggy=0)", m.name),
    };
    match bmc(&with_buggy(m, 1)) {
        Ok(n) => panic!(
            "{} (Buggy=1) MUST yield a counterexample but invariant held over {n} states \
             — the property is trivial / does not catch the defect",
            m.name
        ),
        Err((st, inv)) => eprintln!(
            "{}: invariant `{inv}` correctly CAUGHT at {st:?} (interpreter, Buggy=1).",
            m.name
        ),
    }
    proven
}

/// The invariants some reachable state of `m` violates, all of them in one walk where checking
/// each alone costs a walk apiece. It returns exactly the set that running [`bmc`] with only
/// invariant `I` kept would, for every `I`, including where those runs would panic:
///
/// * The BFS is [`bmc`]'s step for step (queue, successor order, `seen` set, `MAX_STATES`
///   assert). The reachable space and its order do not depend on which invariants are checked
///   ([`Model::init_state`], [`Model::eval_env`] and [`Model::successors_in`] never read
///   `m.invariants`), so every isolated run pops the same states for as long as it lives.
/// * An isolated run for `I` stops at the first state where `I` fails, before expanding it. So
///   this walk keeps a live set: at each state it evaluates every live invariant (in model
///   order), then drops those that failed, and returns without expanding once none is live. A
///   panicking expansion is reached only while some invariant's isolated run would reach it.
/// * With no invariants, nothing is explored.
///
/// Conversely every panic an isolated run hits is hit here: until its invariant fails, that
/// run and this walk pop, evaluate and expand the same states. The result is a set, or a panic
/// for a model the interpreter cannot evaluate (as `verify::uncaught_invariants` expects).
///
/// # Panics
///
/// If the reachable space exceeds [`MAX_STATES`], or on an expression the
/// interpreter cannot evaluate (a function-valued model) — exactly when some
/// isolated per-invariant [`bmc`] would.
#[must_use]
// Skip: the `bmc` driver class.
#[cfg_attr(trust_verify, trust::skip)]
pub fn violated_invariants(m: &Model) -> BTreeSet<&'static str> {
    if m.invariants.is_empty() {
        return BTreeSet::new();
    }
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(violated_invariants_on(m, false))
}

/// [`violated_invariants`] over at most the interpreter's state budget: an
/// invariant still unviolated when the budget runs out is reported unviolated
/// rather than panicking, so a mutant family whose space never ends is judged
/// by the violations it reaches first.
///
/// # Panics
///
/// On an expression the interpreter cannot evaluate (a function-valued model).
#[must_use]
// Skip: same BFS driver class as `violated_invariants`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn violated_within_budget(m: &Model) -> BTreeSet<&'static str> {
    if m.invariants.is_empty() {
        return BTreeSet::new();
    }
    walk!(violated_invariants_on(m, true))
}

// Skip: same BFS driver class as `bmc`; the body of `violated_invariants`.
#[cfg_attr(trust_verify, trust::skip)]
fn violated_invariants_on<W: Walker>(
    m: &Model,
    w: &W,
    stop_at_budget: bool,
) -> BTreeSet<&'static str> {
    let mut violated: BTreeSet<&'static str> = BTreeSet::new();
    let mut live: Vec<usize> = (0..m.invariants.len()).collect();
    let mut seen = Seen::default();
    let mut q: VecDeque<W::S> = VecDeque::new();
    let init = w.init();
    w.first_sight(&mut seen, &init);
    q.push_back(init);
    let mut n = 0usize;
    while let Some(st) = q.pop_front() {
        n += 1;
        if stop_at_budget && n >= MAX_STATES {
            return violated;
        }
        assert!(
            n < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let env = w.env(&st);
        // Evaluate every live invariant here before dropping any: each one's isolated run
        // reaches this state, so a panic in one is a panic its isolated run hits.
        let failed: Vec<bool> = live.iter().map(|&i| !w.holds(&env, &st, i)).collect();
        let mut k = 0;
        live.retain(|&i| {
            let keep = !failed[k];
            k += 1;
            if !keep {
                violated.insert(m.invariants[i].name);
            }
            keep
        });
        if live.is_empty() {
            return violated;
        }
        for a in 0..m.actions.len() {
            for ns in w.successors(&env, &st, a) {
                if w.first_sight(&mut seen, &ns) {
                    q.push_back(ns);
                }
            }
        }
    }
    violated
}

/// The actions that fire (have a successor from some reachable state) anywhere in `m`'s bounded
/// space. Invariants are not checked and exploration does not stop at a violation: this is the
/// reachability question the strict-vacuity dead-action audit asks, and a prove-and-catch
/// mutant typically fires into its violating state.
///
/// # Panics
///
/// If the reachable space exceeds [`MAX_STATES`] (a model-bounds regression),
/// exactly like [`bmc`].
#[must_use]
// Skip: the `bmc` driver class.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fired_actions(m: &Model) -> BTreeSet<&'static str> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(fired_actions_on(m))
}

// Skip: same BFS driver class as `bmc`; the body of `fired_actions`.
#[cfg_attr(trust_verify, trust::skip)]
fn fired_actions_on<W: Walker>(m: &Model, w: &W) -> BTreeSet<&'static str> {
    let mut seen = Seen::default();
    let mut fired: BTreeSet<&'static str> = BTreeSet::new();
    let mut q: VecDeque<W::S> = VecDeque::new();
    let init = w.init();
    w.first_sight(&mut seen, &init);
    q.push_back(init);
    let mut n = 0usize;
    while let Some(st) = q.pop_front() {
        n += 1;
        assert!(
            n < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        // One env per popped state, shared by every action — see `bmc`.
        let env = w.env(&st);
        for (a, act) in m.actions.iter().enumerate() {
            for ns in w.successors(&env, &st, a) {
                fired.insert(act.name);
                if w.first_sight(&mut seen, &ns) {
                    q.push_back(ns);
                }
            }
        }
    }
    fired
}

/// Whether `action` is a VIOLATION STEP of `m`: it fires from some reachable
/// state where every law holds, and every such firing lands in a state that
/// breaks one; `None` when it never fires from a lawful state. Such an action
/// is the defect itself (its own step, or the world's answer to one), so its
/// being dead where no mutant is on is what the laws state rather than
/// vacuity. A firing from a state some law already refused says nothing about
/// the action, so it is not looked at; exploration does not stop at a
/// violating state, like [`fired_actions`].
///
/// # Panics
///
/// If `m` declares no action named `action`, or its reachable space exceeds
/// [`MAX_STATES`], exactly like [`bmc`].
#[must_use]
// Skip: same BFS driver class as `fired_actions`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn violation_step(m: &Model, action: &str) -> Option<bool> {
    let a = m
        .actions
        .iter()
        .position(|act| act.name == action)
        .unwrap_or_else(|| panic!("{} declares no action `{action}`", m.name));
    walk!(violation_step_on(m, a))
}

// Skip: same BFS driver class as `bmc`; the body of `violation_step`.
#[cfg_attr(trust_verify, trust::skip)]
fn violation_step_on<W: Walker>(m: &Model, w: &W, action: usize) -> Option<bool> {
    let lawful = |env: &W::Env, s: &W::S| (0..m.invariants.len()).all(|i| w.holds(env, s, i));
    let mut seen = Seen::default();
    let mut q: VecDeque<W::S> = VecDeque::new();
    let init = w.init();
    w.first_sight(&mut seen, &init);
    q.push_back(init);
    let (mut n, mut fired) = (0usize, false);
    while let Some(st) = q.pop_front() {
        n += 1;
        assert!(
            n < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let env = w.env(&st);
        let from_lawful = lawful(&env, &st);
        for a in 0..m.actions.len() {
            for ns in w.successors(&env, &st, a) {
                if a == action && from_lawful {
                    fired = true;
                    if lawful(&w.env(&ns), &ns) {
                        return Some(false);
                    }
                }
                if w.first_sight(&mut seen, &ns) {
                    q.push_back(ns);
                }
            }
        }
    }
    fired.then_some(true)
}

/// Every state reachable in `m`'s bounded space, each once, in BFS order: the
/// states [`bmc`] counts, in the order it pops them (`Init` first; then each
/// state's successors, action by action in model order, as first seen).
/// Invariants are not checked.
///
/// # Panics
///
/// If the reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
#[must_use]
// Skip: same BFS driver class as `bmc`/`fired_actions`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn reachable_states(m: &Model) -> Vec<State> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(reachable_on(m))
}

// Skip: same BFS driver class as `bmc`; the body of `reachable_states`.
#[cfg_attr(trust_verify, trust::skip)]
fn reachable_on<W: Walker>(m: &Model, w: &W) -> Vec<State> {
    let mut seen = Seen::default();
    let mut q: VecDeque<W::S> = VecDeque::new();
    let mut all = Vec::new();
    let init = w.init();
    w.first_sight(&mut seen, &init);
    q.push_back(init);
    while let Some(st) = q.pop_front() {
        assert!(
            all.len() + 1 < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let env = w.env(&st);
        for a in 0..m.actions.len() {
            for ns in w.successors(&env, &st, a) {
                if w.first_sight(&mut seen, &ns) {
                    q.push_back(ns);
                }
            }
        }
        all.push(w.state(&st));
    }
    all
}

/// A reachable state with no successor under any action that is not declared final: a
/// deadlock, the interpreter twin of `ty`'s `CHECK_DEADLOCK`.
#[must_use]
// Skip: caller-chosen `impl Fn` (the user-T dispatch class).
#[cfg_attr(trust_verify, trust::skip)]
pub fn find_deadlock(m: &Model, is_final: impl Fn(&State) -> bool) -> Option<State> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(find_deadlock_on(m, &is_final))
}

// Skip: caller-chosen `dyn Fn` over the `bmc` driver class; the body of `find_deadlock`.
#[cfg_attr(trust_verify, trust::skip)]
fn find_deadlock_on<W: Walker>(
    m: &Model,
    w: &W,
    is_final: &dyn Fn(&State) -> bool,
) -> Option<State> {
    let mut seen = Seen::default();
    let mut q: VecDeque<W::S> = VecDeque::new();
    let init = w.init();
    w.first_sight(&mut seen, &init);
    q.push_back(init);
    while let Some(st) = q.pop_front() {
        let mut any_succ = false;
        // One env per popped state, shared by every action — see `bmc`.
        let env = w.env(&st);
        for a in 0..m.actions.len() {
            for ns in w.successors(&env, &st, a) {
                any_succ = true;
                if w.first_sight(&mut seen, &ns) {
                    q.push_back(ns);
                }
            }
        }
        if !any_succ {
            let st = w.state(&st);
            if !is_final(&st) {
                return Some(st); // stuck, and not a legitimate work-complete terminal
            }
        }
    }
    None
}

/// What a liveness verdict may assume will happen: weak and strong fairness over named actions.
///
/// An action not named is unconstrained: the environment (a person typing, a program
/// streaming, a release that may never be published) may take it or never take it again. Every
/// state may also stutter forever (TLA+'s `[][Next]_vars`), so the interpreter and `ty` judge
/// the same runs. Nothing is fair by default: each name is an assumption the verdict rests on.
///
/// * `weak` is `WF_vars(A)`: an action enabled continuously from some point on is eventually
///   taken — for a clock, a capped hold, a deadline, a step nothing can disable once due.
/// * `strong` is `SF_vars(A)`: an action enabled again and again is eventually taken, even if
///   the environment disables it in between — for a poll whose gate the environment toggles.
///
/// "Enabled" means enabled to change the state (TLA+'s `ENABLED <<A>>_vars`): a step that
/// writes every variable back to its own value is a stutter, neither demanded nor credited.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fairness<'a> {
    /// Weakly fair actions (`WF_vars`).
    pub weak: &'a [&'a str],
    /// Strongly fair actions (`SF_vars`).
    pub strong: &'a [&'a str],
}

/// A fair behaviour that leaves the goal for good — the counterexample to
/// `[]<>goal`, as a lasso: a stem from `Init`, then a loop a fair run repeats
/// forever without the goal holding anywhere on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonProgressCycle {
    /// From `Init` to the loop's first state: `("Init", init)`, then one `(action, state after
    /// it)` per step. The stem may pass through goal states: leaving the goal for good after
    /// reaching it is still the failure.
    pub stem: Vec<(&'static str, State)>,
    /// The loop, from the stem's last state back to it, one `(action, state after it)` per
    /// step. Empty means the run stutters there forever: nothing the fairness names is enabled
    /// to move it on (a wedge is the case where nothing at all is).
    pub cycle: Vec<(&'static str, State)>,
    /// Every action with a step inside the fair component the loop was drawn
    /// from: the moves the livelock can keep making for ever. Empty for a wedge.
    pub moves: BTreeSet<&'static str>,
    /// How many states that component holds.
    pub component: usize,
}

impl NonProgressCycle {
    /// The state the loop starts and ends at.
    #[must_use]
    pub fn entry(&self) -> &State {
        // The stem always holds at least `("Init", init)`.
        &self.stem[self.stem.len() - 1].1
    }
}

impl std::fmt::Display for NonProgressCycle {
    // Skip: diagnostic rendering over map walks and `write!` (absent formatting bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A step prints as the action and the variables it moved: forty full
        // states in a panic message hide the one variable that matters.
        fn moved(before: &State, after: &State) -> String {
            let changed: Vec<String> = after
                .iter()
                .filter(|(name, value)| before.get(*name) != Some(*value))
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            changed.join(" ")
        }
        let path = |f: &mut std::fmt::Formatter<'_>,
                    from: &State,
                    steps: &[(&'static str, State)]|
         -> std::fmt::Result {
            let mut at = from;
            for (action, state) in steps {
                writeln!(f, "    {action}: {}", moved(at, state))?;
                at = state;
            }
            Ok(())
        };
        let init = &self.stem[0].1;
        writeln!(f, "  from Init {init:?}")?;
        path(f, init, &self.stem[1..])?;
        writeln!(f, "  reaches {:?}", self.entry())?;
        if self.cycle.is_empty() {
            writeln!(f, "  and stutters there forever")?;
        } else {
            writeln!(f, "  and repeats forever:")?;
            path(f, self.entry(), &self.cycle)?;
        }
        write!(
            f,
            "  (a fair component of {} state(s); moves inside it: {:?})",
            self.component, self.moves
        )
    }
}

/// The whole reachable state graph, stutter steps dropped, numbered in BFS order
/// (so a lower index is never farther from `Init`).
struct StateGraph {
    states: Vec<State>,
    /// Per state, `(action index, successor index)` for every step that changes
    /// the state — the steps fairness can demand.
    steps: Vec<Vec<(usize, usize)>>,
    /// How BFS first reached each state (`None` for `Init`): the stem's source.
    parent: Vec<Option<(usize, usize)>>,
    /// The actions with a successor from some reachable state, stutters
    /// counted: exactly [`fired_actions`]' answer, taken from the same walk.
    fired: BTreeSet<&'static str>,
}

impl StateGraph {
    fn enabled(&self, state: usize, action: usize) -> bool {
        self.steps[state].iter().any(|&(a, _)| a == action)
    }

    /// Tarjan's strongly connected components of the subgraph `nodes` induces,
    /// each sorted ascending. Iterative: a recursive walk would be as deep as the
    /// longest simple path, which a bounded model can still make thousands long.
    // Skip: keyed bookkeeping over the caller's graph (absent std bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn components(&self, nodes: &[usize]) -> Vec<Vec<usize>> {
        let member: BTreeSet<usize> = nodes.iter().copied().collect();
        // node -> (discovery index, lowest index reachable through the DFS tree)
        let mut order: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        let mut on_stack: BTreeSet<usize> = BTreeSet::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut out: Vec<Vec<usize>> = Vec::new();
        let mut counter = 0usize;
        for &root in nodes {
            if order.contains_key(&root) {
                continue;
            }
            order.insert(root, (counter, counter));
            counter += 1;
            stack.push(root);
            on_stack.insert(root);
            let mut calls: Vec<(usize, usize)> = vec![(root, 0)];
            while let Some(&(v, pos)) = calls.last() {
                if let Some(&(_, w)) = self.steps[v].get(pos) {
                    if let Some(top) = calls.last_mut() {
                        top.1 += 1;
                    }
                    if !member.contains(&w) {
                        continue;
                    }
                    match order.get(&w).copied() {
                        None => {
                            order.insert(w, (counter, counter));
                            counter += 1;
                            stack.push(w);
                            on_stack.insert(w);
                            calls.push((w, 0));
                        }
                        Some((w_index, _)) if on_stack.contains(&w) => {
                            if let Some(entry) = order.get_mut(&v) {
                                entry.1 = entry.1.min(w_index);
                            }
                        }
                        Some(_) => {}
                    }
                    continue;
                }
                calls.pop();
                let (v_index, v_low) = order.get(&v).copied().unwrap_or((0, 0));
                if let Some(&(u, _)) = calls.last()
                    && let Some(entry) = order.get_mut(&u)
                {
                    entry.1 = entry.1.min(v_low);
                }
                if v_low == v_index {
                    let mut component = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack.remove(&w);
                        component.push(w);
                        if w == v {
                            break;
                        }
                    }
                    component.sort_unstable();
                    out.push(component);
                }
            }
        }
        out
    }

    /// A shortest walk from `from` to `to` over steps inside `member`, appended
    /// to `walk` as `(action index, state index after)`.
    // Skip: BFS bookkeeping over BTreeMap/VecDeque (the absent-std-body class);
    // its expect is the harness's own "a component is strongly connected".
    #[cfg_attr(trust_verify, trust::skip)]
    fn walk_inside(
        &self,
        m: &Model,
        member: &BTreeSet<usize>,
        from: usize,
        to: usize,
        walk: &mut Vec<(usize, usize)>,
    ) {
        if from == to {
            return;
        }
        let mut came: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        let mut q: VecDeque<usize> = VecDeque::from([from]);
        while let Some(s) = q.pop_front() {
            if s == to {
                break;
            }
            for &(a, t) in &self.steps[s] {
                if member.contains(&t) && t != from && !came.contains_key(&t) {
                    came.insert(t, (a, s));
                    q.push_back(t);
                }
            }
        }
        let mut rev: Vec<(usize, usize)> = Vec::new();
        let mut at = to;
        while at != from {
            let Some(&(a, prev)) = came.get(&at) else {
                panic!(
                    "{}: no path inside a strongly connected component — the SCC pass is broken",
                    m.name
                )
            };
            rev.push((a, at));
            at = prev;
        }
        walk.extend(rev.into_iter().rev());
    }

    /// The lasso a fair run can follow in `component`: a stem to its first state, then a loop
    /// that takes one step of every fair action with a step inside it and, for a weakly fair
    /// action with none, visits a state where it is disabled — each what that action's
    /// fairness demands of a run confined here, so the loop is a fair run by construction.
    // Skip: witness assembly over the caller's graph (absent std bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn witness(
        &self,
        m: &Model,
        component: &[usize],
        member: &BTreeSet<usize>,
        inside: &BTreeSet<usize>,
        weak: &[usize],
        strong: &[usize],
    ) -> NonProgressCycle {
        let entry = component[0];
        let mut stem: Vec<(&'static str, State)> = Vec::new();
        let mut at = entry;
        while let Some((a, prev)) = self.parent[at] {
            stem.push((m.actions[a].name, self.states[at].clone()));
            at = prev;
        }
        stem.push(("Init", self.states[at].clone()));
        stem.reverse();

        let mut fair: Vec<usize> = weak.iter().chain(strong).copied().collect();
        fair.sort_unstable();
        let mut cycle: Vec<(usize, usize)> = Vec::new();
        let mut cur = entry;
        // One step of every fair action that has a step inside the component…
        for &a in &fair {
            if !inside.contains(&a) {
                continue;
            }
            let step = component.iter().find_map(|&s| {
                self.steps[s]
                    .iter()
                    .find(|&&(b, t)| b == a && member.contains(&t))
                    .map(|&(_, t)| (s, t))
            });
            if let Some((s, t)) = step {
                self.walk_inside(m, member, cur, s, &mut cycle);
                cycle.push((a, t));
                cur = t;
            }
        }
        self.walk_inside(m, member, cur, entry, &mut cycle);
        // …and, for a weakly fair action with none, a visit to a state where it
        // is off, unless the loop already passes one: a detour from the entry
        // and back, so the loop stays closed.
        for &a in &fair {
            if inside.contains(&a) || !weak.contains(&a) {
                continue;
            }
            let passes_off =
                !self.enabled(entry, a) || cycle.iter().any(|&(_, s)| !self.enabled(s, a));
            if passes_off {
                continue;
            }
            if let Some(&s) = component.iter().find(|&&s| !self.enabled(s, a)) {
                self.walk_inside(m, member, entry, s, &mut cycle);
                self.walk_inside(m, member, s, entry, &mut cycle);
            }
        }
        NonProgressCycle {
            stem,
            cycle: cycle
                .into_iter()
                .map(|(a, s)| (m.actions[a].name, self.states[s].clone()))
                .collect(),
            moves: inside.iter().map(|&a| m.actions[a].name).collect(),
            component: component.len(),
        }
    }
}

/// Walk `m`'s whole reachable graph (stutter steps dropped, numbered in BFS
/// order) over a [`Walker`]. The graph keeps each state as the public
/// [`State`]; the walk's own form of each is returned beside it, by index.
// Skip: keyed BFS over caller-built models (the `bmc` driver class) plus the deliberate
// bounds-regression assert.
#[cfg_attr(trust_verify, trust::skip)]
fn explore_on<W: Walker>(m: &Model, w: &W) -> (StateGraph, Vec<W::S>) {
    let init = w.init();
    let mut ids = Seen::default();
    ids.add(w.key(&init));
    let mut graph = StateGraph {
        states: vec![w.state(&init)],
        steps: vec![Vec::new()],
        parent: vec![None],
        fired: BTreeSet::new(),
    };
    let mut walked: Vec<W::S> = vec![init];
    let mut next = 0;
    while next < walked.len() {
        let st = walked[next].clone();
        // One env per popped state, shared by every action — see `bmc`.
        let env = w.env(&st);
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (a, act) in m.actions.iter().enumerate() {
            for ns in w.successors(&env, &st, a) {
                // Before the stutter drop: a stutter still fires.
                graph.fired.insert(act.name);
                if ns == st {
                    continue; // a stutter: fairness neither demands nor credits it
                }
                let id = match w.lookup(&ids, &ns) {
                    Some(id) => id,
                    None => {
                        let id = walked.len();
                        assert!(
                            id < MAX_STATES,
                            "{} state space unexpectedly large — tighten bounds",
                            m.name
                        );
                        let numbered = ids.add(w.key(&ns));
                        debug_assert_eq!(numbered, id, "the graph numbers states as first seen");
                        graph.states.push(w.state(&ns));
                        graph.steps.push(Vec::new());
                        graph.parent.push(Some((a, next)));
                        walked.push(ns);
                        id
                    }
                };
                if !out.contains(&(a, id)) {
                    out.push((a, id));
                }
            }
        }
        graph.steps[next] = out;
        next += 1;
    }
    (graph, walked)
}

/// The action indices a fairness list names. An undeclared name panics: dropping a typo
/// silently would change what the verdict assumes.
// Skip: name resolution with a deliberate harness panic.
#[cfg_attr(trust_verify, trust::skip)]
fn fair_indices(m: &Model, names: &[&str], kind: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for name in names {
        let Some(index) = m.actions.iter().position(|a| a.name == *name) else {
            panic!(
                "{}: {kind} fairness names `{name}`, which the model does not declare",
                m.name
            )
        };
        out.push(index);
    }
    out
}

/// A reachable non-progress cycle under `fairness`: a fair behaviour along which `is_goal`
/// never holds again, the counterexample to `[]<>goal`. `None` means every fair behaviour of
/// the bounded model keeps reaching the goal.
///
/// [`bmc`] sees a bad state and [`find_deadlock`] a state with no way out. A livelock is
/// neither: every state on it is legal and has successors (the terminal keeps producing output,
/// the lane keeps retrying) and it never lands, so it passes both.
///
/// Explore the whole reachable graph (stutter steps dropped), take the strongly connected
/// components of its non-goal states, and ask of each whether a fair run can stay forever
/// (Emerson–Lei):
///
/// * a weakly fair action enabled at every state of the component with no step inside it would
///   stay enabled and never be taken: no fair run stays, so the component is dropped;
/// * a strongly fair action enabled at some state with no step inside: a fair run may stay only
///   by avoiding the states where it is enabled, so those are removed and the rest is split
///   into components and asked again;
/// * a component surviving both is fair: a loop through all of it takes every fair action with
///   a step there and visits a state where every other weakly fair action is off.
///
/// A single state counts, since a state may stutter forever: a non-goal state where no fair
/// action is enabled is a stall (a wedge when nothing is), reported with an empty `cycle`. This
/// subsumes [`find_deadlock`] for liveness and agrees with `ty`'s reading of `[][Next]_vars`.
/// Components are asked nearest-to-`Init` first, so the reported stem is a shortest one.
///
/// # Panics
///
/// If `fairness` names an action the model does not declare, or names one
/// action both weakly and strongly fair (one of the two would be dead text); or
/// if the reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
#[must_use]
// Skip: caller-chosen `impl Fn` (the user-T dispatch class) over the keyed graph.
#[cfg_attr(trust_verify, trust::skip)]
pub fn find_nonprogress_cycle(
    m: &Model,
    is_goal: impl Fn(&State) -> bool,
    fairness: &Fairness<'_>,
) -> Option<NonProgressCycle> {
    Space::of(m).find_nonprogress_cycle(is_goal, fairness)
}

/// A model's whole reachable space, walked once and then asked any number of questions: which
/// actions fire, how many states there are, and liveness claims over the same graph.
/// [`fired_actions`], [`bmc`]'s count and [`nonprogress_under`] each walk the space on their
/// own, and the graph depends only on the model, so a caller asking several (the liveness audit,
/// a claim under several fairnesses) walks once here. The Space owns the model it walked.
pub struct Space {
    model: Model,
    graph: StateGraph,
    /// The model resolved once, and each graph state in its form (by index),
    /// when the model has one: a liveness goal is then evaluated without
    /// building an environment map per state.
    compiled: Option<(Compiled, Vec<Vec<i64>>)>,
}

impl Space {
    /// Walk `m`'s reachable space.
    ///
    /// # Panics
    ///
    /// If the reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
    #[must_use]
    // Skip: wraps the BTreeMap-keyed BFS above plus a model clone.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn of(m: &Model) -> Self {
        debug_assert!(
            names_are_unique(m),
            "{}: duplicate action/invariant name",
            m.name
        );
        let (graph, compiled) = match Compiled::new(m) {
            Some(c) => {
                let (graph, walked) = explore_on(m, &c);
                (graph, Some((c, walked)))
            }
            None => (explore_on(m, &ByName(m)).0, None),
        };
        Self {
            graph,
            model: m.clone(),
            compiled,
        }
    }

    /// The number of reachable states: what [`bmc`] returns for the same model
    /// with no invariants.
    #[must_use]
    pub fn states(&self) -> usize {
        self.graph.states.len()
    }

    /// The actions that fire somewhere in the space: [`fired_actions`]' answer.
    #[must_use]
    pub fn fired(&self) -> &BTreeSet<&'static str> {
        &self.graph.fired
    }

    /// [`nonprogress_under`] over this space's model, without walking it again.
    #[must_use]
    pub fn nonprogress_under(&self, live: &Liveness) -> Option<NonProgressCycle> {
        let fairness = Fairness {
            weak: &live.weak,
            strong: &live.strong,
        };
        match &self.compiled {
            // `Liveness::goal_holds`, over the compiled state: the same value,
            // and the same panic for an integer goal.
            Some((c, walked)) => {
                let goal = c.expr(&self.model, &live.goal);
                self.nonprogress_by_index(
                    |i| match goal.eval(&walked[i]) {
                        Value::Bool(holds) => holds,
                        Value::Int(_) => panic!(
                            "liveness goal `{}` of `{}` is an integer — model type error",
                            live.name, self.model.name
                        ),
                    },
                    &fairness,
                )
            }
            None => {
                self.find_nonprogress_cycle(|state| live.goal_holds(&self.model, state), &fairness)
            }
        }
    }

    /// [`find_nonprogress_cycle`] over this space's model, without walking it again (the
    /// algorithm is documented there).
    // Skip: caller-chosen `impl Fn` over the keyed graph, as the free fn it serves.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn find_nonprogress_cycle(
        &self,
        is_goal: impl Fn(&State) -> bool,
        fairness: &Fairness<'_>,
    ) -> Option<NonProgressCycle> {
        self.nonprogress_by_index(|s| is_goal(&self.graph.states[s]), fairness)
    }

    /// [`Space::find_nonprogress_cycle`] with the goal asked of each state by
    /// its index in the graph.
    // Skip: caller-chosen `impl Fn` over the keyed graph, as the fn it serves.
    #[cfg_attr(trust_verify, trust::skip)]
    fn nonprogress_by_index(
        &self,
        is_goal: impl Fn(usize) -> bool,
        fairness: &Fairness<'_>,
    ) -> Option<NonProgressCycle> {
        let (m, graph) = (&self.model, &self.graph);
        let weak = fair_indices(m, fairness.weak, "weak");
        let strong = fair_indices(m, fairness.strong, "strong");
        if let Some(&both) = weak.iter().find(|a| strong.contains(a)) {
            panic!(
                "{}: `{}` is named both weakly and strongly fair — say which",
                m.name, m.actions[both].name
            );
        }
        let non_goal: Vec<usize> = (0..graph.states.len()).filter(|&s| !is_goal(s)).collect();
        // Keyed by each component's least (nearest-to-Init) state, distinct since components
        // are disjoint, so `pop_first` asks the nearest first.
        let mut queue: BTreeMap<usize, Vec<usize>> = graph
            .components(&non_goal)
            .into_iter()
            .map(|c| (c[0], c))
            .collect();
        while let Some((_, component)) = queue.pop_first() {
            let member: BTreeSet<usize> = component.iter().copied().collect();
            let inside: BTreeSet<usize> = component
                .iter()
                .flat_map(|&s| {
                    graph.steps[s]
                        .iter()
                        .filter(|(_, t)| member.contains(t))
                        .map(|&(a, _)| a)
                })
                .collect();
            let starved_weakly = weak
                .iter()
                .any(|&a| !inside.contains(&a) && component.iter().all(|&s| graph.enabled(s, a)));
            if starved_weakly {
                continue;
            }
            let starved_strongly: Vec<usize> = strong
                .iter()
                .copied()
                .filter(|&a| !inside.contains(&a) && component.iter().any(|&s| graph.enabled(s, a)))
                .collect();
            if !starved_strongly.is_empty() {
                let rest: Vec<usize> = component
                    .iter()
                    .copied()
                    .filter(|&s| !starved_strongly.iter().any(|&a| graph.enabled(s, a)))
                    .collect();
                for sub in graph.components(&rest) {
                    queue.insert(sub[0], sub);
                }
                continue;
            }
            return Some(graph.witness(m, &component, &member, &inside, &weak, &strong));
        }
        None
    }
}

/// [`find_nonprogress_cycle`] for a declared [`Liveness`] obligation: its goal
/// expression and its stated fairness, evaluated against `m` (which may be a
/// `Buggy` flip or a mutant-isolated copy of the model it was written for).
#[must_use]
pub fn nonprogress_under(m: &Model, live: &Liveness) -> Option<NonProgressCycle> {
    Space::of(m).nonprogress_under(live)
}

/// Tier-1 conformance twin of a two-step `ty trace validate`: does some action of the model's
/// `Next` admit the real `prev -> next` transition? `prev` need not be reachable (like
/// `transition_spec`'s open `Init`). Returns the admitting action's name, or `None` when the
/// transition does not conform.
#[must_use]
pub fn admits(m: &Model, prev: &State, next: &State) -> Option<&'static str> {
    // One env for every candidate action at `prev`. No `names_are_unique` debug-assert: this fn
    // is not `trust::skip`ped and the assert would add a bare panic obligation; every model
    // reaching `admits` is BMC'd by the same suites.
    let env = m.eval_env(prev);
    m.actions
        .iter()
        .find(|a| m.successors_in(a, &env, prev).contains(next))
        .map(|a| a.name)
}

/// The one successor of `state` under `action` whose `var` is `value`: how a Tier-1 lattice
/// walk picks its point among a nondeterministic `\in lo..hi` while still reaching it through
/// the model's own transition (writing the picked values into a state would also accept a point
/// the model cannot reach). `None` when the model cannot pick `value` from `state`.
///
/// # Panics
///
/// If two successors share `var == value`: `var` does not identify the pick, and choosing would
/// be the harness deciding for the model.
// Skip: harness machinery over caller-built states (a collected successor Vec, a filtered
// iterator).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pick(m: &Model, state: &State, action: &str, var: &str, value: i64) -> Option<State> {
    let mut matching = m
        .successors(action, state)
        .into_iter()
        .filter(|s| s.get(var) == Some(&value));
    let chosen = matching.next()?;
    assert!(
        matching.next().is_none(),
        "{}: `{action}` has two successors with {var} = {value} from {state:?}",
        m.name
    );
    Some(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::{config_catalog_snapshot_model, ring_model, subscribe_model};

    /// `fired_actions` is the reachability question the strict-vacuity
    /// dead-action audit asks: at the committed config the Buggy-guarded
    /// mutants are dead; at `Buggy = 1` they fire.
    #[test]
    fn fired_actions_separates_committed_dead_mutants_from_buggy_live_ones() {
        let m = config_catalog_snapshot_model();
        let fired0 = fired_actions(&with_buggy(&m, 0));
        assert!(fired0.contains("AdmitPatch"));
        assert!(fired0.contains("PublishOne"));
        assert!(!fired0.contains("AdmitStaleTrail"));
        assert!(!fired0.contains("AdmitStaleKitty"));
        assert!(!fired0.contains("AdmitStaleTheme"));
        assert!(!fired0.contains("AdmitStaleSparkle"));
        assert!(!fired0.contains("PublishLiveUnadmitted"));
        let fired1 = fired_actions(&with_buggy(&m, 1));
        assert!(fired1.contains("AdmitStaleTrail"));
        assert!(fired1.contains("AdmitStaleKitty"));
        assert!(fired1.contains("AdmitStaleTheme"));
        assert!(fired1.contains("AdmitStaleSparkle"));
        assert!(fired1.contains("PublishLiveUnadmitted"));
    }

    /// `admits` accepts a real ring transition and rejects two corrupted ones.
    #[test]
    fn admits_is_a_real_conformance_check() {
        let m = ring_model();
        let prev = m.init_state();
        // The canonical successor of some enabled action is admitted...
        let (a, next) = m
            .actions
            .iter()
            .find_map(|a| {
                m.successors(a.name, &prev)
                    .into_iter()
                    .next()
                    .map(|s| (a.name, s))
            })
            .expect("ring Init has an enabled action");
        assert_eq!(admits(&m, &prev, &next), Some(a));
        // ...and a corrupted target is rejected.
        let mut bad = next.clone();
        for v in bad.values_mut() {
            *v += 7;
        }
        assert_eq!(
            admits(&m, &prev, &bad),
            None,
            "corrupted transition must not conform"
        );
    }

    // ---- find_nonprogress_cycle: the livelock checker, on toy models ----

    /// A reported lasso must be one: every stem and loop step admitted by the action it names,
    /// the loop closing on its entry, and the goal false on the loop — checked against the
    /// model's own transition relation, not the checker's word.
    fn assert_lasso(m: &Model, cycle: &NonProgressCycle, goal: impl Fn(&State) -> bool) {
        assert_eq!(
            cycle.stem[0],
            ("Init", m.init_state()),
            "the stem starts at Init"
        );
        for pair in cycle.stem.windows(2) {
            let ((_, before), (action, after)) = (&pair[0], &pair[1]);
            assert!(
                m.successors(action, before).contains(after),
                "stem step {action} is not the model's: {before:?} -> {after:?}"
            );
        }
        let mut at = cycle.entry().clone();
        assert!(!goal(&at), "the loop's entry is a goal state");
        for (action, next) in &cycle.cycle {
            assert!(
                m.successors(action, &at).contains(next),
                "loop step {action} is not the model's: {at:?} -> {next:?}"
            );
            assert!(!goal(next), "the loop passes a goal state at {next:?}");
            at = next.clone();
        }
        assert_eq!(&at, cycle.entry(), "the loop closes on its entry");
    }

    /// A lane that retries forever and never lands: every state is legal and has a successor,
    /// so the invariant and deadlock checks both pass it. The non-progress check finds the loop,
    /// and finds none in the lane that can land.
    #[test]
    fn a_retry_loop_that_never_lands_passes_every_older_check_and_is_found() {
        let m = crate::ty_model! {
            RetryLoop {
                const Buggy = 0;
                var landed = 0;
                var attempt = 0;
                action Retry when (landed == 0) {
                    attempt = if attempt == 0 { 1 } else { 0 };
                }
                action Land when (Buggy == 0 && landed == 0) { landed = 1; }
                invariant AttemptIsAFlag: attempt <= 1;
            }
        };
        let landed = |s: &State| s["landed"] == 1;
        let fair = Fairness {
            weak: &["Retry", "Land"],
            strong: &[],
        };
        assert_eq!(find_nonprogress_cycle(&m, landed, &fair), None);

        let buggy = with_buggy(&m, 1);
        assert!(bmc(&buggy).is_ok(), "no invariant sees the loop");
        assert_eq!(
            find_deadlock(&buggy, landed),
            None,
            "no state is stuck, so the deadlock check sees nothing"
        );
        let cycle = find_nonprogress_cycle(&buggy, landed, &fair)
            .expect("the retry loop never lands, and fairness cannot help it");
        assert_lasso(&buggy, &cycle, landed);
        assert_eq!(cycle.moves, BTreeSet::from(["Retry"]));
        assert!(
            !cycle.cycle.is_empty() && cycle.cycle.iter().all(|(a, _)| *a == "Retry"),
            "a real loop of retries, not a stall: {cycle}"
        );
    }

    /// A lane that always reaches its goal is clean — and only because of the
    /// fairness it states. With none, the run may simply stop at `Init`
    /// (stuttering is always allowed), which is reported as a stall.
    #[test]
    fn a_lane_that_always_lands_is_clean_and_its_fairness_is_what_makes_it_so() {
        let m = crate::ty_model! {
            Climb {
                const Top = 3;
                var x = 0;
                action Step when (x <= Top - 1) { x = x + 1; }
            }
        };
        let top = |s: &State| s["x"] == 3;
        let fair = Fairness {
            weak: &["Step"],
            strong: &[],
        };
        assert_eq!(find_nonprogress_cycle(&m, top, &fair), None);

        let stall = find_nonprogress_cycle(&m, top, &Fairness::default())
            .expect("with nothing fair, the run may stop anywhere short of the top");
        assert_lasso(&m, &stall, top);
        assert!(stall.cycle.is_empty(), "a stall stutters: {stall}");
        assert_eq!(stall.entry(), &m.init_state(), "the nearest stall is Init");
        assert_eq!(
            find_deadlock(&m, top),
            None,
            "and no deadlock check would ever report a stall that has a way out"
        );
    }

    /// Fairness excludes starvation-only cycles: a loop that exists only by never taking an
    /// action that stays enabled is not weakly fair, and one that exists only by never taking
    /// an action whose gate it keeps passing open is not strongly fair. Each is found without
    /// the assumption that excludes it.
    #[test]
    fn fairness_excludes_starvation_only_cycles() {
        // Land is enabled at every state of the tick loop.
        let spin = crate::ty_model! {
            Spin {
                var tick = 0;
                var done = 0;
                action Tick when (done == 0) { tick = if tick == 0 { 1 } else { 0 }; }
                action Land when (done == 0) { done = 1; }
            }
        };
        let done = |s: &State| s["done"] == 1;
        let unfair = Fairness {
            weak: &["Tick"],
            strong: &[],
        };
        let starved = find_nonprogress_cycle(&spin, done, &unfair)
            .expect("with Land unfair, ticking forever is a behaviour");
        assert_lasso(&spin, &starved, done);
        assert_eq!(starved.moves, BTreeSet::from(["Tick"]));
        let weakly = Fairness {
            weak: &["Tick", "Land"],
            strong: &[],
        };
        assert_eq!(
            find_nonprogress_cycle(&spin, done, &weakly),
            None,
            "the loop starves a continuously enabled Land: weak fairness excludes it"
        );

        // Land is enabled only while the terminal is idle, and the terminal
        // keeps going busy.
        let flicker = crate::ty_model! {
            Flicker {
                var busy = 0;
                var done = 0;
                action Busy when (done == 0 && busy == 0) { busy = 1; }
                action Idle when (done == 0 && busy == 1) { busy = 0; }
                action Land when (done == 0 && busy == 0) { done = 1; }
            }
        };
        let weak_land = Fairness {
            weak: &["Idle", "Land"],
            strong: &[],
        };
        let flickers = find_nonprogress_cycle(&flicker, done, &weak_land)
            .expect("weak fairness does not force a gate the environment keeps closing");
        assert_lasso(&flicker, &flickers, done);
        assert_eq!(flickers.moves, BTreeSet::from(["Busy", "Idle"]));
        assert_eq!(flickers.cycle.len(), 2, "busy, then idle again: {flickers}");
        let strong_land = Fairness {
            weak: &["Idle"],
            strong: &["Land"],
        };
        assert_eq!(
            find_nonprogress_cycle(&flicker, done, &strong_land),
            None,
            "a gate that keeps opening is caught open: strong fairness excludes it"
        );
    }

    /// Strong fairness removes the states where the gate is open, not the whole component: a
    /// loop that never passes the gate is still a fair livelock (Emerson–Lei), while one where
    /// every loop passes the gate lands.
    #[test]
    fn strong_fairness_keeps_a_loop_that_never_passes_the_gate() {
        // Three rooms in a ring, landing only from room 1.
        let ring = crate::ty_model! {
            Rooms {
                var at = 0;
                var done = 0;
                action Forward when (done == 0) { at = if at == 2 { 0 } else { at + 1 }; }
                action Backward when (done == 0) { at = if at == 0 { 2 } else { at - 1 }; }
                action Land when (done == 0 && at == 1) { done = 1; }
            }
        };
        let done = |s: &State| s["done"] == 1;
        let fair = Fairness {
            weak: &["Forward", "Backward"],
            strong: &["Land"],
        };
        let avoids = find_nonprogress_cycle(&ring, done, &fair)
            .expect("rooms 0 and 2 are adjacent: a run can circle them forever");
        assert_lasso(&ring, &avoids, done);
        assert!(
            avoids.cycle.iter().all(|(_, s)| s["at"] != 1) && avoids.entry()["at"] != 1,
            "the fair loop never enters the room the gate is in: {avoids}"
        );
        assert_eq!(avoids.component, 2);

        // Two rooms: every loop passes the gate, so a strongly fair lane lands.
        let pair = crate::ty_model! {
            TwoRooms {
                var at = 0;
                var done = 0;
                action Forward when (done == 0) { at = if at == 1 { 0 } else { 1 }; }
                action Land when (done == 0 && at == 1) { done = 1; }
            }
        };
        let fair = Fairness {
            weak: &["Forward"],
            strong: &["Land"],
        };
        assert_eq!(find_nonprogress_cycle(&pair, done, &fair), None);
    }

    /// A wedge short of the goal is a non-progress behaviour too (it stutters
    /// there forever), so the check subsumes `find_deadlock` for liveness.
    #[test]
    fn a_wedge_short_of_the_goal_is_reported_as_a_stall() {
        let m = crate::ty_model! {
            Wedge {
                var at = 0;
                action Stray when (at == 0) { at = 1; }
                action Finish when (at == 0) { at = 2; }
            }
        };
        let finished = |s: &State| s["at"] == 2;
        let fair = Fairness {
            weak: &["Finish"],
            strong: &[],
        };
        let wedge = find_nonprogress_cycle(&m, finished, &fair).expect("`Stray` wedges");
        assert_lasso(&m, &wedge, finished);
        assert!(wedge.cycle.is_empty() && wedge.moves.is_empty(), "{wedge}");
        assert_eq!(wedge.entry()["at"], 1);
        assert_eq!(find_deadlock(&m, finished).map(|s| s["at"]), Some(1));
    }

    /// A step that writes every variable back to its own value is a stutter
    /// (TLA+'s `<<A>>_vars`): it is not "enabled" for fairness, so declaring it
    /// fair forces nothing, and it is never progress.
    #[test]
    fn a_step_that_changes_nothing_is_a_stutter_not_progress() {
        let m = crate::ty_model! {
            Poller {
                var done = 0;
                action Poll when (done == 0) { done = 0; }
            }
        };
        let done = |s: &State| s["done"] == 1;
        let fair = Fairness {
            weak: &["Poll"],
            strong: &[],
        };
        let stall = find_nonprogress_cycle(&m, done, &fair).expect("polling is not landing");
        assert!(stall.cycle.is_empty() && stall.moves.is_empty(), "{stall}");
    }

    /// The iterative Tarjan pass against its definition: two states share a component exactly
    /// when each reaches the other inside the subset asked about (the low-link hand-back to the
    /// caller frame is easy to get wrong). Pseudo-random graphs from a fixed xorshift seed.
    #[test]
    fn components_are_exactly_the_mutual_reachability_classes() {
        let mut rng = crate::rng::XorShift::new(0x2545_F491_4F6C_DD1D);
        for _ in 0..300 {
            let n = rng.index(12) + 1;
            let steps: Vec<Vec<(usize, usize)>> = (0..n)
                .map(|_| {
                    let fan = rng.index(4);
                    (0..fan).map(|_| (0, rng.index(n))).collect()
                })
                .collect();
            let graph = StateGraph {
                states: vec![State::new(); n],
                steps,
                parent: vec![None; n],
                fired: BTreeSet::new(),
            };
            let nodes: Vec<usize> = (0..n).filter(|_| rng.index(4) != 0).collect();
            let member: BTreeSet<usize> = nodes.iter().copied().collect();
            let reaches = |from: usize| -> BTreeSet<usize> {
                let mut seen = BTreeSet::from([from]);
                let mut q = VecDeque::from([from]);
                while let Some(v) = q.pop_front() {
                    for &(_, w) in &graph.steps[v] {
                        if member.contains(&w) && seen.insert(w) {
                            q.push_back(w);
                        }
                    }
                }
                seen
            };
            let reach: BTreeMap<usize, BTreeSet<usize>> =
                nodes.iter().map(|&v| (v, reaches(v))).collect();
            let components = graph.components(&nodes);
            let mut covered = BTreeSet::new();
            for component in &components {
                for &v in component {
                    assert!(
                        covered.insert(v),
                        "{v} is in two components: {components:?}"
                    );
                }
                for &a in component {
                    for &b in &nodes {
                        let mutual = reach[&a].contains(&b) && reach[&b].contains(&a);
                        assert_eq!(
                            component.contains(&b),
                            mutual,
                            "{a} and {b}: components {components:?} over {:?}",
                            graph.steps
                        );
                    }
                }
            }
            assert_eq!(covered, member, "every asked-about state is in a component");
        }
    }

    /// A [`Space`] answers what the one-shot walks answer: the same cycle (or `None`) under each
    /// fairness asked of one graph, `bmc`'s state count, and `fired_actions`' fired set,
    /// stutters included.
    #[test]
    fn a_space_answers_what_the_one_shot_walks_answer() {
        let m = crate::ty_model! {
            RetryLoop {
                const Buggy = 0;
                var landed = 0;
                var attempt = 0;
                action Retry when (landed == 0) {
                    attempt = if attempt == 0 { 1 } else { 0 };
                }
                action Land when (Buggy == 0 && landed == 0) { landed = 1; }
                invariant AttemptIsAFlag: attempt <= 1;
            }
        };
        let landed = |s: &State| s["landed"] == 1;
        let fair = |weak: &'static [&'static str]| Fairness { weak, strong: &[] };
        for model in [with_buggy(&m, 0), with_buggy(&m, 1)] {
            let space = Space::of(&model);
            for weak in [&["Retry", "Land"][..], &["Retry"], &[]] {
                assert_eq!(
                    space.find_nonprogress_cycle(landed, &fair(weak)),
                    find_nonprogress_cycle(&model, landed, &fair(weak)),
                    "{weak:?} at Buggy = {:?}",
                    model.consts
                );
            }
            let live = Liveness {
                name: "Lands",
                goal: crate::derive::eq(crate::derive::var("landed"), crate::derive::int(1)),
                weak: vec!["Retry", "Land"],
                strong: vec![],
                mutants: vec![],
            };
            assert_eq!(
                space.nonprogress_under(&live),
                nonprogress_under(&model, &live)
            );
            let mut bare = model.clone();
            bare.invariants.clear();
            assert_eq!(Ok(space.states()), bmc(&bare));
            assert_eq!(space.fired(), &fired_actions(&model));
        }
        // The walk both agree on is not a clean one: the lane lands only at
        // `Buggy = 0`, and the Space sees the loop at `Buggy = 1`.
        assert_eq!(
            Space::of(&with_buggy(&m, 0)).find_nonprogress_cycle(landed, &fair(&["Retry", "Land"])),
            None
        );
        assert!(
            Space::of(&with_buggy(&m, 1))
                .find_nonprogress_cycle(landed, &fair(&["Retry", "Land"]))
                .is_some()
        );

        // A step that changes nothing is dropped from the graph's steps but still fires:
        // `fired_actions` counts it, so the Space must.
        let poller = crate::ty_model! {
            Poller {
                var done = 0;
                action Poll when (done == 0) { done = 0; }
            }
        };
        let space = Space::of(&poller);
        assert_eq!(space.fired(), &BTreeSet::from(["Poll"]));
        assert_eq!(space.fired(), &fired_actions(&poller));
        assert_eq!(space.states(), 1);
    }

    // ---- the compiled form is the model as written ----

    /// Every walk, run on the compiled form and on the model as written, gives
    /// the same answer — and so does every single step and invariant at every
    /// reachable state, so an agreement cannot hide behind a coincidence of
    /// whole-walk results.
    fn assert_walks_agree(m: &Model) {
        let c = Compiled::new(m).expect("a model whose updates write declared variables");
        let by = ByName(m);
        assert_eq!(bmc_on(m, &c), bmc_on(m, &by), "{}: bmc", m.name);
        assert_eq!(
            fired_actions_on(m, &c),
            fired_actions_on(m, &by),
            "{}: fired",
            m.name
        );
        assert_eq!(
            violated_invariants_on(m, &c, false),
            violated_invariants_on(m, &by, false),
            "{}: violated",
            m.name
        );
        let even = |s: &State| s.values().sum::<i64>() % 2 == 0;
        assert_eq!(
            find_deadlock_on(m, &c, &|_| false),
            find_deadlock_on(m, &by, &|_| false),
            "{}: deadlock",
            m.name
        );
        assert_eq!(
            find_deadlock_on(m, &c, &even),
            find_deadlock_on(m, &by, &even),
            "{}: deadlock (even states final)",
            m.name
        );
        let ((gc, walked), (gb, _)) = (explore_on(m, &c), explore_on(m, &by));
        assert_eq!(gc.states, gb.states, "{}: graph states", m.name);
        assert_eq!(gc.steps, gb.steps, "{}: graph steps", m.name);
        assert_eq!(gc.parent, gb.parent, "{}: graph parents", m.name);
        assert_eq!(gc.fired, gb.fired, "{}: graph fired", m.name);
        let states = reachable_on(m, &c);
        assert_eq!(states, reachable_on(m, &by), "{}: reachable", m.name);
        assert_eq!(states, gc.states, "{}: reachable is the graph's", m.name);
        // The public form names each variable by its place in a state's
        // values, which is `State`'s own iteration order.
        let r = Resolved::new(m).expect("compiles");
        for (i, name) in m.init_state().keys().enumerate() {
            assert_eq!(r.slot(name), i, "{}: slot of {name}", m.name);
        }
        for (st, compact) in states.iter().zip(&walked) {
            assert_eq!(&c.state(compact), st);
            assert_eq!(&st.values().copied().collect::<Vec<_>>(), compact);
            let env = m.eval_env(st);
            for (a, act) in m.actions.iter().enumerate() {
                let compiled: Vec<State> = c
                    .successors(a, compact)
                    .iter()
                    .map(|s| c.state(s))
                    .collect();
                assert_eq!(
                    compiled,
                    m.successors_in(act, &env, st),
                    "{}: {} at {st:?}",
                    m.name,
                    act.name
                );
            }
            for (i, inv) in m.invariants.iter().enumerate() {
                assert_eq!(
                    c.holds(i, compact),
                    m.check_invariant_in(inv, &env),
                    "{}: {} at {st:?}",
                    m.name,
                    inv.name
                );
            }
        }
    }

    /// Committed models, nondeterministic ranges among them (the gui and
    /// update models), each at both `Buggy` settings. (Every committed `\A`
    /// sits in a function-valued model, which no walk evaluates; the corners
    /// test below covers `\A`.)
    #[test]
    fn compiled_walks_are_the_by_name_walks_on_committed_models() {
        use crate::derive::{
            native_update_status_reconciliation_model, pane_tree_model, tab_nav_model,
        };
        for m in [
            ring_model(),
            subscribe_model(),
            config_catalog_snapshot_model(),
            tab_nav_model(),
            pane_tree_model(),
            native_update_status_reconciliation_model(),
        ] {
            for b in [0, 1] {
                assert_walks_agree(&with_buggy(&m, b));
            }
        }
    }

    /// The resolution corners no committed model happens to exercise: a state
    /// variable shadowing a constant, a constant declared twice, a `\A` index
    /// shadowing a variable, nested `\A`s over one index name, an empty range,
    /// a range and a plain update writing the same variable, and every
    /// operator.
    #[test]
    fn compiled_walks_are_the_by_name_walks_on_the_resolution_corners() {
        use crate::derive::{
            Action, Expr, Invariant, StateVar, Update, add, and_, bool_lit, cst, eq, forall, gt,
            if_, iff, in_range, int, le, neq, or_, sub, var,
        };
        let up = |v: &'static str, e: Expr| Update { var: v, expr: e };
        let m = Model {
            name: "Corners",
            // `x` is also a variable (the variable wins); `K` is declared
            // twice (the last declaration wins).
            consts: vec![("x", 7), ("K", 1), ("K", 2), ("Buggy", 0)],
            vars: vec![
                StateVar { name: "y", init: 0 },
                StateVar { name: "x", init: 0 },
                StateVar { name: "i", init: 1 },
            ],
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: "Grow",
                    // `\A i \in 0..x : \A i \in i..2 : i <= K + x` — the inner
                    // index shadows the outer one, which shadows the variable.
                    guard: Some(and_(
                        le(var("x"), int(2)),
                        forall(
                            "i",
                            int(0),
                            var("x"),
                            forall("i", var("i"), int(2), le(var("i"), add(cst("K"), var("x")))),
                        ),
                    )),
                    updates: vec![up("x", in_range(var("x"), add(var("x"), int(1))))],
                },
                Action {
                    name: "Twice",
                    guard: Some(le(var("y"), int(1))),
                    // A range, then a plain write of the same variable.
                    updates: vec![
                        up("y", in_range(int(0), int(2))),
                        up("y", add(var("y"), int(1))),
                    ],
                },
                Action {
                    name: "MaybeEmpty",
                    guard: None,
                    // Empty once `x >= 2`: the action is then disabled.
                    updates: vec![up("i", in_range(var("x"), int(1)))],
                },
                Action {
                    name: "Ops",
                    guard: Some(or_(
                        iff(gt(var("y"), int(0)), neq(var("i"), int(0))),
                        eq(cst("Buggy"), int(1)),
                    )),
                    updates: vec![
                        up(
                            "y",
                            if_(
                                gt(var("y"), int(2)),
                                int(0),
                                sub(var("x"), sub(var("y"), int(1))),
                            ),
                        ),
                        up("i", if_(bool_lit(true), sub(int(1), var("i")), int(9))),
                    ],
                },
            ],
            invariants: vec![
                Invariant {
                    name: "Bounded",
                    kind: crate::derive::InvariantKind::Claim,
                    expr: forall("k", int(1), var("i"), le(var("y"), add(cst("x"), var("k")))),
                },
                Invariant {
                    name: "SmallY",
                    kind: crate::derive::InvariantKind::Claim,
                    expr: le(var("y"), int(2)),
                },
            ],
        };
        for b in [0, 1] {
            assert_walks_agree(&with_buggy(&m, b));
        }
        // And the corners are walked, not skipped: every action fires, a
        // nondeterministic pick fans out, and an invariant is broken.
        assert_eq!(
            fired_actions(&m),
            BTreeSet::from(["Grow", "Twice", "MaybeEmpty", "Ops"])
        );
        assert!(reachable_states(&m).len() > 8);
        assert_eq!(
            violated_invariants(&m),
            BTreeSet::from(["Bounded", "SmallY"])
        );
    }

    /// A panic the model as written raises is raised by the compiled form, with
    /// the same message, and only when the expression is evaluated.
    #[test]
    #[should_panic(expected = "unbound identifier `nope` in model evaluation")]
    fn the_compiled_form_raises_an_unbound_identifier_when_it_reads_it() {
        use crate::derive::{Invariant, StateVar, and_, bool_lit, int, le, var};
        let m = Model {
            name: "Unbound",
            consts: vec![],
            vars: vec![StateVar { name: "x", init: 0 }],
            fn_vars: vec![],
            actions: vec![],
            invariants: vec![Invariant {
                name: "Reads",
                kind: crate::derive::InvariantKind::Claim,
                // `TRUE /\ nope <= 1`: the conjunction reads `nope`.
                expr: and_(bool_lit(true), le(var("nope"), int(1))),
            }],
        };
        // Resolving it is fine; reading it is the model's type error.
        let c = Compiled::new(&m).expect("compiles");
        let _ = c.holds(0, &[0]);
    }

    /// An update writing a name the initial state does not bind grows a
    /// `BTreeMap` state by a key, which a slice cannot: such a model is walked
    /// as written, never compiled into a different identity.
    #[test]
    fn a_model_writing_an_undeclared_variable_is_walked_as_written() {
        use crate::derive::{Action, StateVar, Update, int};
        let m = Model {
            name: "Grows",
            consts: vec![],
            vars: vec![StateVar { name: "x", init: 0 }],
            fn_vars: vec![],
            actions: vec![Action {
                name: "Stray",
                guard: None,
                updates: vec![Update {
                    var: "z",
                    expr: int(1),
                }],
            }],
            invariants: vec![],
        };
        assert!(Compiled::new(&m).is_none());
        assert_eq!(bmc(&m), bmc_on(&m, &ByName(&m)));
        assert_eq!(reachable_states(&m), reachable_on(&m, &ByName(&m)));
    }

    /// A fairness assumption about an action the model does not have is a typo,
    /// never a silently weaker verdict.
    #[test]
    #[should_panic(expected = "which the model does not declare")]
    fn fairness_naming_an_undeclared_action_panics() {
        let m = crate::ty_model! {
            Typo {
                var x = 0;
                action Step when (x == 0) { x = 1; }
            }
        };
        let fair = Fairness {
            weak: &["Stpe"],
            strong: &[],
        };
        let _ = find_nonprogress_cycle(&m, |s| s["x"] == 1, &fair);
    }
}
