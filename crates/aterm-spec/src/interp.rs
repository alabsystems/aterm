// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! In-process model checking.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::compiled::Compiled;
use crate::derive::{Liveness, Model, Value};

/// One concrete interpreter state: every scalar variable's value.
pub type State = BTreeMap<&'static str, i64>;

/// A compiled walk's reached states, each numbered in the order first seen. Only membership
/// and that number are asked, so it is an open-addressed hash table of numbers, placed by
/// [`key_hash`] and probed linearly.
#[derive(Default)]
struct Seen {
    /// Slot -> the top half of a key's hash, above 1 + that key's number (0 for an empty
    /// slot): a power of two long (or empty), at most half full.
    table: Vec<u64>,
    /// Every state's values, in number order.
    values: Vec<i64>,
    /// How many values each state has (the first state's).
    width: usize,
    /// How many states are numbered.
    len: usize,
}

/// A [`Seen`] entry's hash half.
const TAG: u64 = !0 << 32;

impl Seen {
    /// State number `i`.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    fn key(&self, i: usize) -> &[i64] {
        &self.values[i * self.width..(i + 1) * self.width]
    }

    /// Where a key whose hash's top half is `tag` starts its probe: the hash's top bits,
    /// which its last multiply mixed from every value.
    #[inline]
    const fn home(&self, tag: u64) -> usize {
        // A power-of-two length of at least 16 and below 2^32 slots: `tag >> shift` is a
        // slot.
        (tag >> (64 - self.table.len().trailing_zeros())) as usize
    }

    /// `key`'s number, and whether this call gave it (`false`: it had been seen).
    #[cfg_attr(trust_verify, trust::skip)]
    fn number(&mut self, key: &[i64]) -> (usize, bool) {
        let id = self.len;
        if id == 0 {
            self.width = key.len();
        }
        debug_assert_eq!(key.len(), self.width, "one walk's states are equally wide");
        if (id + 1) * 2 > self.table.len() {
            self.grow();
        }
        let tag = key_hash(key) & TAG;
        let mask = self.table.len() - 1;
        let mut at = self.home(tag);
        loop {
            let entry = self.table[at];
            if entry == 0 {
                break;
            }
            if entry & TAG == tag {
                let i = (entry & !TAG) as usize - 1;
                if self.key(i) == key {
                    return (i, false);
                }
            }
            at = (at + 1) & mask;
        }
        self.table[at] = tag | (id as u64 + 1);
        self.values.extend_from_slice(key);
        self.len += 1;
        (id, true)
    }

    /// Double the table (16 slots at first) and place every entry again.
    #[cfg_attr(trust_verify, trust::skip)]
    fn grow(&mut self) {
        let len = (self.table.len() * 2).max(16);
        let old = std::mem::replace(&mut self.table, vec![0; len]);
        let mask = len - 1;
        for entry in old {
            if entry == 0 {
                continue;
            }
            let mut at = self.home(entry & TAG);
            while self.table[at] != 0 {
                at = (at + 1) & mask;
            }
            self.table[at] = entry;
        }
    }
}

/// A state key's hash: a multiply-rotate mix of each value (the `FxHash`
/// construction).
#[inline]
const fn key_hash(key: &[i64]) -> u64 {
    let mut h = key.len() as u64;
    let mut i = 0;
    while i < key.len() {
        h = (h.rotate_left(5) ^ key[i] as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        i += 1;
    }
    h
}

/// `0..n`, counted by a type of this crate's own. The walks loop over actions and successors
/// once per state; a `Range<usize>` loop there runs a dependency's unoptimized copy of
/// `Range::next` (an opt-level-1 crate links the generic copies its dependencies export rather
/// than inlining its own), which took about a tenth of the non-vacuity sweep.
struct Upto {
    at: usize,
    end: usize,
}

#[inline]
const fn upto(end: usize) -> Upto {
    Upto { at: 0, end }
}

impl Iterator for Upto {
    type Item = usize;
    #[inline]
    fn next(&mut self) -> Option<usize> {
        if self.at < self.end {
            self.at += 1;
            Some(self.at - 1)
        } else {
            None
        }
    }
}

/// A walk's reached states, numbered in the order first seen. Every walk here is breadth-first
/// and queues a state exactly when it numbers it, so the numbers ARE the queue: the state to
/// expand next is the lowest number not yet expanded, and no state is held twice.
trait Reached<S: ?Sized>: Default {
    /// How many states are numbered.
    fn len(&self) -> usize;
    /// State number `i`.
    fn get(&self, i: usize) -> &S;
    /// `s`'s number, and whether this call gave it (`false`: it had been seen).
    fn number(&mut self, s: &S) -> (usize, bool);
}

/// A compiled walk's states: each is its key, so the table holds them.
impl Reached<[i64]> for Seen {
    fn len(&self) -> usize {
        self.len
    }
    fn get(&self, i: usize) -> &[i64] {
        self.key(i)
    }
    fn number(&mut self, s: &[i64]) -> (usize, bool) {
        Seen::number(self, s)
    }
}

/// A by-name walk's states, each kept as written and numbered by the whole state, its names
/// with its values: a model walked by name may grow a state by a key, so two states can hold
/// the same values under different names (`{a: 0, b: 1}` and `{a: 0, c: 1}`).
#[derive(Default)]
struct Kept {
    numbers: HashMap<State, usize>,
    states: Vec<State>,
}

impl Reached<State> for Kept {
    fn len(&self) -> usize {
        self.states.len()
    }
    fn get(&self, i: usize) -> &State {
        &self.states[i]
    }
    #[cfg_attr(trust_verify, trust::skip)]
    fn number(&mut self, s: &State) -> (usize, bool) {
        if let Some(&id) = self.numbers.get(s) {
            return (id, false);
        }
        let id = self.states.len();
        self.numbers.insert(s.clone(), id);
        self.states.push(s.clone());
        (id, true)
    }
}

/// One walk's view of a model: how it starts, steps, checks and names a state.
trait Walker {
    /// A state as the walk reads it.
    type S: ?Sized + PartialEq;
    /// The states a walk has numbered ([`Reached`]).
    type Reached: Reached<Self::S>;
    /// The state being expanded, copied out of [`Walker::Reached`] (which grows meanwhile) into
    /// a holder reused for every state of the walk.
    type Held: Default + std::borrow::Borrow<Self::S>;
    /// One step's successors, in a buffer reused for every step of the walk.
    type Next: Default;
    /// What every action and invariant at one state reads (loop-invariant
    /// across them, so built once per expanded state — see `bmc`).
    type Env;
    fn init(&self) -> Self::Held;
    /// Copy `s` into `held`.
    fn hold(&self, s: &Self::S, held: &mut Self::Held);
    fn env(&self, s: &Self::S) -> Self::Env;
    /// Whether invariant `i` holds at `s`.
    fn holds(&self, env: &Self::Env, s: &Self::S, i: usize) -> bool;
    /// Every successor of `s` under action `a`, in [`Model::successors_in`]'s order, into
    /// `next` (replacing what it held); returns how many.
    fn successors(&self, env: &Self::Env, s: &Self::S, a: usize, next: &mut Self::Next) -> usize;
    /// Successor `k` of the last [`Walker::successors`] into `next`.
    fn nth<'n>(&self, next: &'n Self::Next, k: usize) -> &'n Self::S;
    /// `s` as the public [`State`].
    fn state(&self, s: &Self::S) -> State;
}

/// The model evaluated as written: [`Walker`] over `BTreeMap` states.
struct ByName<'m>(&'m Model);

impl Walker for ByName<'_> {
    type S = State;
    type Reached = Kept;
    type Held = State;
    type Next = Vec<State>;
    type Env = BTreeMap<&'static str, i64>;
    fn init(&self) -> State {
        self.0.init_state()
    }
    fn hold(&self, s: &State, held: &mut State) {
        held.clone_from(s);
    }
    fn env(&self, s: &State) -> Self::Env {
        self.0.eval_env(s)
    }
    #[cfg_attr(trust_verify, trust::skip)]
    fn holds(&self, env: &Self::Env, _: &State, i: usize) -> bool {
        self.0.check_invariant_in(&self.0.invariants[i], env)
    }
    #[cfg_attr(trust_verify, trust::skip)]
    fn successors(&self, env: &Self::Env, s: &State, a: usize, next: &mut Vec<State>) -> usize {
        *next = self.0.successors_in(&self.0.actions[a], env, s);
        next.len()
    }
    fn nth<'n>(&self, next: &'n Vec<State>, k: usize) -> &'n State {
        &next[k]
    }
    fn state(&self, s: &State) -> State {
        s.clone()
    }
}

/// The model resolved once: [`Walker`] over value slices in sorted-name order. Every state of
/// a compiled model binds the same names, so its values are its identity.
impl Walker for Compiled {
    type S = [i64];
    type Reached = Seen;
    type Held = Vec<i64>;
    type Next = Vec<i64>;
    type Env = ();
    fn init(&self) -> Vec<i64> {
        Compiled::init(self)
    }
    fn hold(&self, s: &[i64], held: &mut Vec<i64>) {
        held.clear();
        held.extend_from_slice(s);
    }
    fn env(&self, _: &[i64]) {}
    fn holds(&self, (): &(), s: &[i64], i: usize) -> bool {
        Compiled::holds(self, i, s)
    }
    fn successors(&self, (): &(), s: &[i64], a: usize, next: &mut Vec<i64>) -> usize {
        Compiled::successors_into(self, a, s, next)
    }
    fn nth<'n>(&self, next: &'n Vec<i64>, k: usize) -> &'n [i64] {
        let width = self.width();
        &next[k * width..(k + 1) * width]
    }
    fn state(&self, s: &[i64]) -> State {
        Compiled::state(self, s)
    }
}

/// One breadth-first walk's working set, each part reused for the whole walk: the states
/// numbered so far (which are also the queue, [`Reached`]), the state being expanded, and one
/// step's successors.
struct Walk<W: Walker> {
    reached: W::Reached,
    held: W::Held,
    next: W::Next,
    /// How many states have been expanded: the next to expand is this number.
    expanded: usize,
}

impl<W: Walker> Walk<W> {
    /// A walk with `w`'s initial state numbered (0) and nothing expanded.
    fn new(w: &W) -> Self {
        let mut reached = W::Reached::default();
        reached.number(w.init().borrow());
        Self {
            reached,
            held: W::Held::default(),
            next: W::Next::default(),
            expanded: 0,
        }
    }

    /// Hold the next state to expand, in the order numbered; `false` once every numbered state
    /// has been expanded.
    fn pop(&mut self, w: &W) -> bool {
        if self.expanded == self.reached.len() {
            return false;
        }
        w.hold(self.reached.get(self.expanded), &mut self.held);
        self.expanded += 1;
        true
    }
}

/// A model resolved once, for a caller that walks its space itself (a record asserted at each step, a law that must break alone, a reachability question no invariant states).
pub struct Resolved(Compiled);

impl Resolved {
    /// Resolve `m`; `None` for a model whose updates write a name its initial state does not bind (a `State` that grows a key has no fixed slice).
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
    /// If `name` is not one of the model's state variables.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn slot(&self, name: &str) -> usize {
        self.0
            .slot(name)
            .unwrap_or_else(|| panic!("`{name}` is not a state variable"))
    }

    /// Every successor of `s` under the model's `a`-th action ([`Model::successors_in`]: the same states, in the same order).
    #[must_use]
    pub fn successors(&self, a: usize, s: &[i64]) -> Vec<Vec<i64>> {
        self.0.successors(a, s)
    }

    /// Whether the model's `i`-th invariant holds at `s` ([`Model::check_invariant_in`]).
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

/// Run `$walk` (a generic fn over a [`Walker`]) on `$m`'s compiled form, or on the model as written when it has none.
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
/// by-name form would evaluate the first record twice and never the second.
#[cfg_attr(trust_verify, trust::skip)]
fn names_are_unique(m: &Model) -> bool {
    let actions: BTreeSet<&'static str> = m.actions.iter().map(|a| a.name).collect();
    let invariants: BTreeSet<&'static str> = m.invariants.iter().map(|i| i.name).collect();
    actions.len() == m.actions.len() && invariants.len() == m.invariants.len()
}

/// A copy of `m` with the named constants overridden (the interpreter reads constants from `m.consts`, so this is the interpreter analogue of a `.cfg` override list).
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

/// A copy of `m` with its `Buggy` constant set to `b` — the prove-and-catch protocol's variant flip.
#[must_use]
pub fn with_buggy(m: &Model, b: i64) -> Model {
    with_consts(m, &[("Buggy", b)])
}

/// Exhaustive bounded model check: BFS the reachable state space over every action, checking every invariant at every state.
///
/// # Panics
/// If the reachable space exceeds [`MAX_STATES`] (a model-bounds regression, never a property of a healthy derived model).
#[cfg_attr(trust_verify, trust::skip)]
pub fn bmc(m: &Model) -> Result<usize, (State, &'static str)> {
    bmc_fired(m).0
}

/// A bmc verdict, beside the actions that fired in the space walked to reach it.
pub type BmcFired = (Result<usize, (State, &'static str)>, BTreeSet<&'static str>);

/// [`bmc`], and the actions that fired (had a successor) from the states it checked.
///
/// # Panics
/// As [`bmc`].
#[cfg_attr(trust_verify, trust::skip)]
pub fn bmc_fired(m: &Model) -> BmcFired {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    let (verdict, fired) = walk!(bmc_on(m));
    let fired = m
        .actions
        .iter()
        .zip(fired)
        .filter_map(|(a, f)| f.then_some(a.name))
        .collect();
    (verdict, fired)
}

#[cfg_attr(trust_verify, trust::skip)]
fn bmc_on<W: Walker>(m: &Model, w: &W) -> (Result<usize, (State, &'static str)>, Vec<bool>) {
    let mut fired = vec![false; m.actions.len()];
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        assert!(
            walk.expanded < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let st = walk.held.borrow();
        let env = w.env(st);
        for (i, inv) in m.invariants.iter().enumerate() {
            if !w.holds(&env, st, i) {
                return (Err((w.state(st), inv.name)), fired);
            }
        }
        for (a, fired) in fired.iter_mut().enumerate() {
            let made = w.successors(&env, st, a, &mut walk.next);
            *fired |= made > 0;
            for k in upto(made) {
                walk.reached.number(w.nth(&walk.next, k));
            }
        }
    }
    (Ok(walk.expanded), fired)
}

/// The prove-and-catch protocol (the `Buggy` convention), interpreter tier.
///
/// # Panics
/// On an invariant violation at `Buggy = 0`, or a vacuous property (no counterexample at `Buggy = 1`).
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

/// The invariants some reachable state of `m` violates, all of them in one walk where checking each alone costs a walk apiece.
///
/// # Panics
/// If the reachable space exceeds [`MAX_STATES`], or on an expression the interpreter cannot evaluate (a function-valued model).
#[must_use]
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

/// [`violated_invariants`] over at most the interpreter's state budget.
///
/// # Panics
/// On an expression the interpreter cannot evaluate (a function-valued model).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn violated_within_budget(m: &Model) -> BTreeSet<&'static str> {
    if m.invariants.is_empty() {
        return BTreeSet::new();
    }
    walk!(violated_invariants_on(m, true))
}

#[cfg_attr(trust_verify, trust::skip)]
fn violated_invariants_on<W: Walker>(
    m: &Model,
    w: &W,
    stop_at_budget: bool,
) -> BTreeSet<&'static str> {
    let mut violated: BTreeSet<&'static str> = BTreeSet::new();
    let mut live: Vec<usize> = (0..m.invariants.len()).collect();
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        let n = walk.expanded;
        if stop_at_budget && n >= MAX_STATES {
            return violated;
        }
        assert!(
            n < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let st = walk.held.borrow();
        let env = w.env(st);
        live.retain(|&i| {
            let keep = w.holds(&env, st, i);
            if !keep {
                violated.insert(m.invariants[i].name);
            }
            keep
        });
        if live.is_empty() {
            return violated;
        }
        for a in upto(m.actions.len()) {
            let made = w.successors(&env, st, a, &mut walk.next);
            for k in upto(made) {
                walk.reached.number(w.nth(&walk.next, k));
            }
        }
    }
    violated
}

/// The actions that fire (have a successor from some reachable state) anywhere in `m`'s bounded space.
///
/// # Panics
/// If the reachable space exceeds [`MAX_STATES`] (a model-bounds regression), exactly like [`bmc`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fired_actions(m: &Model) -> BTreeSet<&'static str> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(fired_actions_on(m))
}

#[cfg_attr(trust_verify, trust::skip)]
fn fired_actions_on<W: Walker>(m: &Model, w: &W) -> BTreeSet<&'static str> {
    let mut fired = vec![false; m.actions.len()];
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        assert!(
            walk.expanded < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let st = walk.held.borrow();
        let env = w.env(st);
        for (a, fired) in fired.iter_mut().enumerate() {
            let made = w.successors(&env, st, a, &mut walk.next);
            *fired |= made > 0;
            for k in upto(made) {
                walk.reached.number(w.nth(&walk.next, k));
            }
        }
    }
    m.actions
        .iter()
        .zip(fired)
        .filter_map(|(a, f)| f.then_some(a.name))
        .collect()
}

/// Whether each of `actions` is a VIOLATION STEP of `m`.
///
/// # Panics
/// If `m` declares no action named one of `actions`, or its reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn violation_steps(m: &Model, actions: &[&str]) -> Vec<Option<bool>> {
    let watched: Vec<usize> = actions
        .iter()
        .map(|action| {
            m.actions
                .iter()
                .position(|act| act.name == *action)
                .unwrap_or_else(|| panic!("{} declares no action `{action}`", m.name))
        })
        .collect();
    walk!(violation_steps_on(m, &watched))
}

#[cfg_attr(trust_verify, trust::skip)]
fn violation_steps_on<W: Walker>(m: &Model, w: &W, watched: &[usize]) -> Vec<Option<bool>> {
    let lawful = |env: &W::Env, s: &W::S| upto(m.invariants.len()).all(|i| w.holds(env, s, i));
    let mut fired = vec![false; watched.len()];
    let mut landed_lawful = vec![false; watched.len()];
    let verdicts = |fired: &[bool], landed_lawful: &[bool]| -> Vec<Option<bool>> {
        fired
            .iter()
            .zip(landed_lawful)
            .map(|(&f, &l)| if l { Some(false) } else { f.then_some(true) })
            .collect()
    };
    let mut open = watched.len();
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        assert!(
            walk.expanded < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let st = walk.held.borrow();
        let env = w.env(st);
        let from_lawful = lawful(&env, st);
        for a in upto(m.actions.len()) {
            let watching = from_lawful && watched.contains(&a);
            let made = w.successors(&env, st, a, &mut walk.next);
            for j in upto(made) {
                let ns = w.nth(&walk.next, j);
                if watching {
                    let mut lands_lawful = None;
                    for (k, _) in watched.iter().enumerate().filter(|&(_, &x)| x == a) {
                        fired[k] = true;
                        if !landed_lawful[k]
                            && *lands_lawful.get_or_insert_with(|| lawful(&w.env(ns), ns))
                        {
                            landed_lawful[k] = true;
                            open -= 1;
                        }
                    }
                    if open == 0 {
                        return verdicts(&fired, &landed_lawful);
                    }
                }
                walk.reached.number(ns);
            }
        }
    }
    verdicts(&fired, &landed_lawful)
}

/// Every state reachable in `m`'s bounded space, each once, in BFS order.
///
/// # Panics
/// If the reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn reachable_states(m: &Model) -> Vec<State> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(reachable_on(m))
}

#[cfg_attr(trust_verify, trust::skip)]
fn reachable_on<W: Walker>(m: &Model, w: &W) -> Vec<State> {
    let mut all = Vec::new();
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        assert!(
            walk.expanded < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        let st = walk.held.borrow();
        let env = w.env(st);
        for a in upto(m.actions.len()) {
            let made = w.successors(&env, st, a, &mut walk.next);
            for k in upto(made) {
                walk.reached.number(w.nth(&walk.next, k));
            }
        }
        all.push(w.state(st));
    }
    all
}

/// A reachable state with no successor under any action that is not declared final.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn find_deadlock(m: &Model, is_final: impl Fn(&State) -> bool) -> Option<State> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(find_deadlock_on(m, &is_final))
}

#[cfg_attr(trust_verify, trust::skip)]
fn find_deadlock_on<W: Walker>(
    m: &Model,
    w: &W,
    is_final: &dyn Fn(&State) -> bool,
) -> Option<State> {
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        let mut any_succ = false;
        let st = walk.held.borrow();
        let env = w.env(st);
        for a in upto(m.actions.len()) {
            let made = w.successors(&env, st, a, &mut walk.next);
            any_succ |= made > 0;
            for k in upto(made) {
                walk.reached.number(w.nth(&walk.next, k));
            }
        }
        if !any_succ {
            let st = w.state(st);
            if !is_final(&st) {
                // stuck, and not a legitimate work-complete terminal
                return Some(st);
            }
        }
    }
    None
}

/// What a liveness verdict may assume will happen: weak and strong fairness over named actions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fairness<'a> {
    /// Weakly fair actions (`WF_vars`).
    pub weak: &'a [&'a str],
    /// Strongly fair actions (`SF_vars`).
    pub strong: &'a [&'a str],
}

/// A fair behaviour that leaves the goal for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonProgressCycle {
    /// From `Init` to the loop's first state: `("Init", init)`, then one `(action, state after it)` per step.
    pub stem: Vec<(&'static str, State)>,
    /// The loop, from the stem's last state back to it, one `(action, state after it)` per step.
    pub cycle: Vec<(&'static str, State)>,
    /// Every action with a step inside the fair component the loop was drawn from: the moves the livelock can keep making for ever.
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
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    /// each sorted ascending.
    #[cfg_attr(trust_verify, trust::skip)]
    fn components(&self, nodes: &[usize]) -> Vec<Vec<usize>> {
        let member: BTreeSet<usize> = nodes.iter().copied().collect();
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
}

/// Walk `m`'s whole reachable graph (stutter steps dropped, numbered in BFS
/// order) over a [`Walker`].
#[cfg_attr(trust_verify, trust::skip)]
fn explore_on<W: Walker>(m: &Model, w: &W) -> (StateGraph, W::Reached) {
    let mut graph = StateGraph {
        steps: vec![Vec::new()],
        parent: vec![None],
        fired: BTreeSet::new(),
    };
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        let at = walk.expanded - 1;
        let st = walk.held.borrow();
        let env = w.env(st);
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (a, act) in m.actions.iter().enumerate() {
            let made = w.successors(&env, st, a, &mut walk.next);
            if made > 0 {
                // Before the stutter drop: a stutter still fires.
                graph.fired.insert(act.name);
            }
            for k in upto(made) {
                let ns = w.nth(&walk.next, k);
                if ns == st {
                    // a stutter: fairness neither demands nor credits it
                    continue;
                }
                let (id, new) = walk.reached.number(ns);
                if new {
                    assert!(
                        id < MAX_STATES,
                        "{} state space unexpectedly large — tighten bounds",
                        m.name
                    );
                    debug_assert_eq!(
                        id,
                        graph.steps.len(),
                        "the graph numbers states as first seen"
                    );
                    graph.steps.push(Vec::new());
                    graph.parent.push(Some((a, at)));
                }
                if !out.contains(&(a, id)) {
                    out.push((a, id));
                }
            }
        }
        graph.steps[at] = out;
    }
    (graph, walk.reached)
}

/// The action indices a fairness list names. An undeclared name panics: dropping a typo
/// silently would change what the verdict assumes.
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

/// A reachable non-progress cycle under `fairness`.
///
/// # Panics
/// If `fairness` names an action the model does not declare, or names one action both weakly and strongly fair (one of the two would be dead text).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn find_nonprogress_cycle(
    m: &Model,
    is_goal: impl Fn(&State) -> bool,
    fairness: &Fairness<'_>,
) -> Option<NonProgressCycle> {
    Space::of(m).find_nonprogress_cycle(is_goal, fairness)
}

/// A model's whole reachable space, walked once and then asked any number of questions.
pub struct Space {
    model: Model,
    graph: StateGraph,
    walked: Walked,
}

/// Each graph state of a [`Space`], by index, in the form its walk held it.
enum Walked {
    /// The model resolved once and each state as its value slice: a liveness
    /// goal is evaluated without building an environment map per state, and a
    /// map is built only for a state a caller asks to see.
    Compiled(Compiled, Seen),
    /// A model with no compiled form: each state as written.
    ByName(Kept),
}

impl Walked {
    /// Graph state `i` as the public [`State`].
    fn state(&self, i: usize) -> State {
        match self {
            Walked::Compiled(c, walked) => c.state(walked.key(i)),
            Walked::ByName(states) => states.get(i).clone(),
        }
    }

    /// `f` of graph state `i`, borrowed where the walk holds it as a [`State`].
    fn with_state<R>(&self, i: usize, f: impl FnOnce(&State) -> R) -> R {
        match self {
            Walked::Compiled(c, walked) => f(&c.state(walked.key(i))),
            Walked::ByName(states) => f(states.get(i)),
        }
    }
}

impl Space {
    /// Walk `m`'s reachable space.
    ///
    /// # Panics
    /// If the reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn of(m: &Model) -> Self {
        debug_assert!(
            names_are_unique(m),
            "{}: duplicate action/invariant name",
            m.name
        );
        let (graph, walked) = match Compiled::new(m) {
            Some(c) => {
                let (graph, walked) = explore_on(m, &c);
                (graph, Walked::Compiled(c, walked))
            }
            None => {
                let (graph, walked) = explore_on(m, &ByName(m));
                (graph, Walked::ByName(walked))
            }
        };
        Self {
            graph,
            model: m.clone(),
            walked,
        }
    }

    /// The number of reachable states: what [`bmc`] returns for the same model with no invariants.
    #[must_use]
    pub const fn states(&self) -> usize {
        self.graph.steps.len()
    }

    /// The actions that fire somewhere in the space: [`fired_actions`]' answer.
    #[must_use]
    pub const fn fired(&self) -> &BTreeSet<&'static str> {
        &self.graph.fired
    }

    /// [`nonprogress_under`] over this space's model, without walking it again.
    #[must_use]
    pub fn nonprogress_under(&self, live: &Liveness) -> Option<NonProgressCycle> {
        let fairness = Fairness {
            weak: &live.weak,
            strong: &live.strong,
        };
        match &self.walked {
            Walked::Compiled(c, walked) => {
                let goal = c.expr(&self.model, &live.goal);
                self.nonprogress_by_index(
                    |i| match goal.eval(walked.key(i)) {
                        Value::Bool(holds) => holds,
                        Value::Int(_) => panic!(
                            "liveness goal `{}` of `{}` is an integer — model type error",
                            live.name, self.model.name
                        ),
                    },
                    &fairness,
                )
            }
            Walked::ByName(_) => {
                self.find_nonprogress_cycle(|state| live.goal_holds(&self.model, state), &fairness)
            }
        }
    }

    /// [`find_nonprogress_cycle`] over this space's model, without walking it again (the algorithm is documented there).
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn find_nonprogress_cycle(
        &self,
        is_goal: impl Fn(&State) -> bool,
        fairness: &Fairness<'_>,
    ) -> Option<NonProgressCycle> {
        self.nonprogress_by_index(|s| self.walked.with_state(s, &is_goal), fairness)
    }

    /// [`Space::find_nonprogress_cycle`] with the goal asked of each state by
    /// its index in the graph.
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
        let non_goal: Vec<usize> = (0..graph.steps.len()).filter(|&s| !is_goal(s)).collect();
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
            return Some(self.witness(&component, &member, &inside, &weak, &strong));
        }
        None
    }

    /// The lasso a fair run can follow in `component`: a stem to its first state, then a loop
    /// that takes one step of every fair action with a step inside it and, for a weakly fair
    /// action with none, visits a state where it is disabled — each what that action's
    /// fairness demands of a run confined here, so the loop is a fair run by construction.
    #[cfg_attr(trust_verify, trust::skip)]
    fn witness(
        &self,
        component: &[usize],
        member: &BTreeSet<usize>,
        inside: &BTreeSet<usize>,
        weak: &[usize],
        strong: &[usize],
    ) -> NonProgressCycle {
        let (m, graph) = (&self.model, &self.graph);
        let entry = component[0];
        let mut stem: Vec<(&'static str, State)> = Vec::new();
        let mut at = entry;
        while let Some((a, prev)) = graph.parent[at] {
            stem.push((m.actions[a].name, self.walked.state(at)));
            at = prev;
        }
        stem.push(("Init", self.walked.state(at)));
        stem.reverse();

        let mut fair: Vec<usize> = weak.iter().chain(strong).copied().collect();
        fair.sort_unstable();
        let mut cycle: Vec<(usize, usize)> = Vec::new();
        let mut cur = entry;
        for &a in &fair {
            if !inside.contains(&a) {
                continue;
            }
            let step = component.iter().find_map(|&s| {
                graph.steps[s]
                    .iter()
                    .find(|&&(b, t)| b == a && member.contains(&t))
                    .map(|&(_, t)| (s, t))
            });
            if let Some((s, t)) = step {
                graph.walk_inside(m, member, cur, s, &mut cycle);
                cycle.push((a, t));
                cur = t;
            }
        }
        graph.walk_inside(m, member, cur, entry, &mut cycle);
        for &a in &fair {
            if inside.contains(&a) || !weak.contains(&a) {
                continue;
            }
            let passes_off =
                !graph.enabled(entry, a) || cycle.iter().any(|&(_, s)| !graph.enabled(s, a));
            if passes_off {
                continue;
            }
            if let Some(&s) = component.iter().find(|&&s| !graph.enabled(s, a)) {
                graph.walk_inside(m, member, entry, s, &mut cycle);
                graph.walk_inside(m, member, s, entry, &mut cycle);
            }
        }
        NonProgressCycle {
            stem,
            cycle: cycle
                .into_iter()
                .map(|(a, s)| (m.actions[a].name, self.walked.state(s)))
                .collect(),
            moves: inside.iter().map(|&a| m.actions[a].name).collect(),
            component: component.len(),
        }
    }
}

/// [`find_nonprogress_cycle`] for a declared [`Liveness`] obligation.
#[must_use]
pub fn nonprogress_under(m: &Model, live: &Liveness) -> Option<NonProgressCycle> {
    Space::of(m).nonprogress_under(live)
}

/// Tier-1 conformance twin of a two-step `ty trace validate`.
#[must_use]
pub fn admits(m: &Model, prev: &State, next: &State) -> Option<&'static str> {
    let env = m.eval_env(prev);
    m.actions
        .iter()
        .find(|a| m.successors_in(a, &env, prev).contains(next))
        .map(|a| a.name)
}

/// The one successor of `state` under `action` whose `var` is `value`.
///
/// # Panics
/// If two successors share `var == value`.
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
    use crate::test_models::{config_catalog_snapshot_model, ring_model, subscribe_model};

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
    /// so the invariant and deadlock checks both pass it.
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

    /// A loop reached only through several steps is reported with the run that reaches it:
    /// the stem retraces, from `Init`, how the walk first reached each state on the way.
    #[test]
    fn a_loop_several_steps_from_init_is_reported_with_the_run_that_reaches_it() {
        let m = crate::ty_model! {
            Approach {
                const Buggy = 0;
                var phase = 0;
                var spin = 0;
                var done = 0;
                action Walk when (phase <= 1) { phase = phase + 1; }
                action Spin when (phase == 2 && done == 0) {
                    spin = if spin == 0 { 1 } else { 0 };
                }
                action Finish when (Buggy == 0 && phase == 2 && done == 0) { done = 1; }
                invariant SpinIsAFlag: spin <= 1;
            }
        };
        let done = |s: &State| s["done"] == 1;
        let fair = Fairness {
            weak: &["Walk", "Spin", "Finish"],
            strong: &[],
        };
        assert_eq!(find_nonprogress_cycle(&m, done, &fair), None);
        let buggy = with_buggy(&m, 1);
        let cycle = find_nonprogress_cycle(&buggy, done, &fair).expect("the spin never finishes");
        assert_lasso(&buggy, &cycle, done);
        let stem: Vec<(&str, i64)> = cycle.stem.iter().map(|(a, s)| (*a, s["phase"])).collect();
        assert_eq!(stem, [("Init", 0), ("Walk", 1), ("Walk", 2)]);
    }

    /// A lane that always reaches its goal is clean — and only because of the
    /// fairness it states.
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
    /// an action whose gate it keeps passing open is not strongly fair.
    #[test]
    fn fairness_excludes_starvation_only_cycles() {
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
    /// caller frame is easy to get wrong).
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

    /// One walk answers each watched action as a walk of its own would: `Some(true)` for an
    /// action every lawful firing of which breaks a law, `Some(false)` for one a lawful firing
    /// of which lands lawful, `None` for one that fires only where a law is already broken or
    /// never.
    #[test]
    fn violation_steps_answer_each_action_as_its_own_walk_would() {
        let m = crate::ty_model! {
            ViolationSteps {
                const Cap = 2;
                var armed = 0;
                var fired = 0;
                var spun = 0;
                var late = 0;
                var tick = 0;
                action Arm when (armed == 0) { armed = 1; }
                action Tick when (tick == 0) { tick = 1; }
                action Spin when (spun == 0) { spun = 1; }
                action Fire when (armed == 1 && fired == 0) { fired = 1; }
                action Stuck when (spun == Cap) { spun = 0; }
                action Late when (late == 0) { late = if spun == 1 { 2 } else { 1 }; }
                invariant NeverArmed: armed == 0;
                invariant NeverHalfLate: 1 > late || late > 1;
            }
        };
        let actions = ["Arm", "Spin", "Fire", "Stuck", "Late"];
        let alone: Vec<Option<bool>> = actions
            .iter()
            .map(|a| violation_steps(&m, &[a])[0])
            .collect();
        assert_eq!(alone, [Some(true), Some(false), None, None, Some(false)]);
        assert_eq!(violation_steps(&m, &actions), alone);
        assert_eq!(
            violation_steps(&m, &["Stuck", "Fire", "Spin", "Arm", "Spin"]),
            [None, None, Some(false), Some(true), Some(false)]
        );
        assert_eq!(
            violation_steps(&m, &["Spin", "Spin"]),
            [Some(false), Some(false)]
        );
        assert_eq!(
            violation_steps(&m, &["Spin", "Late"]),
            [Some(false), Some(false)]
        );
    }

    /// [`bmc_fired`] is [`bmc`]'s verdict, and where that verdict is `Ok` its fired set is
    /// [`fired_actions`]' answer.
    #[test]
    fn bmc_fired_is_bmc_and_fired_actions_in_one_walk() {
        for m in [
            ring_model(),
            subscribe_model(),
            config_catalog_snapshot_model(),
        ] {
            for b in m.mutant_dials().into_iter().chain([0]) {
                let m = with_buggy(&m, b);
                let (verdict, fired) = bmc_fired(&m);
                assert_eq!(verdict, bmc(&m), "{} Buggy={b}", m.name);
                if verdict.is_ok() {
                    assert_eq!(fired, fired_actions(&m), "{} Buggy={b}", m.name);
                }
            }
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
        assert_eq!(
            Space::of(&with_buggy(&m, 0)).find_nonprogress_cycle(landed, &fair(&["Retry", "Land"])),
            None
        );
        assert!(
            Space::of(&with_buggy(&m, 1))
                .find_nonprogress_cycle(landed, &fair(&["Retry", "Land"]))
                .is_some()
        );

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
        let ((gc, walked), (gb, by_name)) = (explore_on(m, &c), explore_on(m, &by));
        let walked: Vec<&[i64]> = (0..walked.len()).map(|i| walked.key(i)).collect();
        let graph_states: Vec<State> = walked.iter().map(|s| c.state(s)).collect();
        assert_eq!(graph_states, by_name.states, "{}: graph states", m.name);
        assert_eq!(gc.steps, gb.steps, "{}: graph steps", m.name);
        assert_eq!(gc.parent, gb.parent, "{}: graph parents", m.name);
        assert_eq!(gc.fired, gb.fired, "{}: graph fired", m.name);
        let states = reachable_on(m, &c);
        assert_eq!(states, reachable_on(m, &by), "{}: reachable", m.name);
        assert_eq!(states, graph_states, "{}: reachable is the graph's", m.name);
        let r = Resolved::new(m).expect("compiles");
        for (i, name) in m.init_state().keys().enumerate() {
            assert_eq!(r.slot(name), i, "{}: slot of {name}", m.name);
        }
        for (st, &compact) in states.iter().zip(&walked) {
            assert_eq!(&c.state(compact), st);
            assert_eq!(st.values().copied().collect::<Vec<_>>(), compact);
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
    /// update models), each at both `Buggy` settings.
    #[test]
    fn compiled_walks_are_the_by_name_walks_on_committed_models() {
        use crate::test_models::{
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
    /// shadowing a variable, nested `\A`s over one index name, an inner `\A`
    /// index read beside an outer one in one fused step, an empty range, a
    /// range and a plain update writing the same variable, and every operator.
    #[test]
    fn compiled_walks_are_the_by_name_walks_on_the_resolution_corners() {
        use crate::derive::{
            Action, Expr, Invariant, StateVar, Update, add, and_, bool_lit, cst, eq, forall, gt,
            if_, iff, in_range, int, le, neq, or_, sub, var,
        };
        let up = |v: &'static str, e: Expr| Update { var: v, expr: e };
        let m = Model {
            name: "Corners",
            consts: vec![("x", 7), ("K", 1), ("K", 2), ("Buggy", 0)],
            vars: vec![
                StateVar { name: "y", init: 0 },
                StateVar { name: "x", init: 0 },
                StateVar { name: "i", init: 1 },
                StateVar { name: "n", init: 0 },
            ],
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: "Nested",
                    guard: Some(and_(
                        eq(var("n"), int(0)),
                        forall(
                            "a",
                            int(0),
                            var("x"),
                            forall(
                                "b",
                                add(var("a"), int(1)),
                                add(var("a"), int(1)),
                                and_(gt(var("b"), var("a")), eq(sub(var("b"), var("a")), int(1))),
                            ),
                        ),
                    )),
                    updates: vec![up("n", int(1))],
                },
                Action {
                    name: "Grow",
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
                    updates: vec![
                        up("y", in_range(int(0), int(2))),
                        up("y", add(var("y"), int(1))),
                    ],
                },
                Action {
                    name: "MaybeEmpty",
                    guard: None,
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
        assert_eq!(
            fired_actions(&m),
            BTreeSet::from(["Nested", "Grow", "Twice", "MaybeEmpty", "Ops"])
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
                expr: and_(bool_lit(true), le(var("nope"), int(1))),
            }],
        };
        let c = Compiled::new(&m).expect("compiles");
        let _ = c.holds(0, &[0]);
    }

    /// Every read the model as written makes, the compiled form makes in the same order with
    /// the same outcome: the same value, or the same panic at the same operand.
    #[test]
    fn the_compiled_form_reads_what_eval_reads_and_fails_where_eval_fails() {
        use crate::derive::{
            Expr, Invariant, StateVar, add, and_, bool_lit, eq, forall, gt, if_, iff, int, le, neq,
            or_, sub, var,
        };
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let outcome = |f: &dyn Fn() -> Value| match catch_unwind(AssertUnwindSafe(f)) {
            Ok(v) => Ok(v),
            Err(p) => Err(p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|m| (*m).to_string()))
                .unwrap_or_default()),
        };
        let x0 = || le(var("x"), int(0));
        let y1 = || eq(var("y"), int(1));
        let cases: Vec<Expr> = vec![
            and_(bool_lit(true), int(1)),
            le(add(var("x"), bool_lit(true)), int(3)),
            if_(gt(var("x"), int(0)), and_(int(1), bool_lit(true)), x0()),
            if_(le(var("x"), int(0)), and_(int(1), bool_lit(true)), x0()),
            le(bool_lit(true), var("nope")),
            le(var("nope"), bool_lit(true)),
            or_(bool_lit(true), int(5)),
            or_(bool_lit(false), int(5)),
            or_(gt(var("x"), int(0)), or_(y1(), var("nope"))),
            and_(and_(x0(), y1()), gt(var("nope"), int(0))),
            and_(and_(x0(), neq(var("y"), int(1))), gt(var("nope"), int(0))),
            and_(x0(), and_(bool_lit(false), var("nope"))),
            and_(var("x"), x0()),
            iff(int(1), bool_lit(true)),
            iff(x0(), y1()),
            forall("k", bool_lit(true), int(2), x0()),
            forall("k", int(0), var("y"), and_(le(var("k"), int(1)), int(3))),
            if_(int(1), x0(), y1()),
            if_(bool_lit(true), add(var("x"), int(1)), bool_lit(false)),
            if_(gt(var("x"), int(0)), int(1), bool_lit(true)),
            le(if_(gt(var("x"), int(0)), var("nope"), int(1)), int(3)),
            le(if_(le(var("x"), int(0)), int(1), var("nope")), int(3)),
            add(var("y"), le(var("x"), int(0))),
            add(var("x"), var("y")),
            var("y"),
            ["i0", "i1", "i2", "i3", "i4", "i5", "i6", "i7", "i8"]
                .iter()
                .zip(0..9_i32)
                .rev()
                .fold(eq(sub(var("i8"), var("i0")), int(8)), |body, (&i, k)| {
                    forall(i, int(k.into()), int(k.into()), body)
                }),
        ];
        let m = Model {
            name: "Reads",
            consts: vec![],
            vars: vec![
                StateVar { name: "x", init: 0 },
                StateVar { name: "y", init: 1 },
            ],
            fn_vars: vec![],
            actions: vec![],
            invariants: cases
                .iter()
                .map(|e| Invariant {
                    name: "Case",
                    kind: crate::derive::InvariantKind::Claim,
                    expr: e.clone(),
                })
                .collect(),
        };
        let c = Compiled::new(&m).expect("compiles");
        let env = m.eval_env(&m.init_state());
        let s = c.init();
        let mut panics = 0;
        for (i, e) in cases.iter().enumerate() {
            let written = outcome(&|| e.eval(&env));
            let compiled = outcome(&|| c.expr(&m, e).eval(&s));
            assert_eq!(compiled, written, "case {i} read whole: {e:?}");
            panics += usize::from(written.is_err());
            let written = outcome(&|| Value::Bool(m.check_invariant_in(&m.invariants[i], &env)));
            let compiled = outcome(&|| Value::Bool(c.holds(i, &s)));
            assert_eq!(compiled, written, "case {i} read as a boolean: {e:?}");
        }
        assert!(panics >= 8, "only {panics} cases panic");
    }

    /// Two keys whose hashes agree in their top half (found by search) share a tag and a home
    /// slot, and each still gets its own number: the table compares the keys behind a matching
    /// tag.
    #[test]
    fn seen_numbers_keys_whose_tags_collide_apart_and_keeps_numbers_as_it_grows() {
        let (a, b) = (
            [9_031_110_522_149_646_314_i64],
            [5_280_068_066_879_230_317_i64],
        );
        assert_eq!(
            key_hash(&a) & TAG,
            key_hash(&b) & TAG,
            "the keys' tags collide"
        );
        let mut seen = Seen::default();
        assert_eq!(seen.number(&a), (0, true));
        assert_eq!(seen.number(&b), (1, true));
        for i in 2..1000 {
            assert_eq!(seen.number(&[i]), (usize::try_from(i).unwrap(), true));
        }
        assert_eq!(seen.number(&b), (1, false));
        assert_eq!(seen.number(&a), (0, false));
        for i in 2..1000 {
            assert_eq!(seen.number(&[i]), (usize::try_from(i).unwrap(), false));
            assert_eq!(seen.key(usize::try_from(i).unwrap()), [i]);
        }
        assert_eq!((seen.key(0), seen.key(1)), (&a[..], &b[..]));
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
        assert_eq!(bmc(&m), bmc_on(&m, &ByName(&m)).0);
        assert_eq!(reachable_states(&m), reachable_on(&m, &ByName(&m)));
    }

    /// A state grown by an undeclared name is told apart by its names as well as its values:
    /// `{a: 0, b: 1}` and `{a: 0, c: 1}` are two states, and the walk expands both.
    #[test]
    fn states_grown_by_different_names_are_different_states() {
        use crate::derive::{Action, StateVar, Update, int};
        let writes = |name: &'static str, var: &'static str| Action {
            name,
            guard: None,
            updates: vec![Update { var, expr: int(1) }],
        };
        let m = Model {
            name: "Forks",
            consts: vec![],
            vars: vec![StateVar { name: "a", init: 0 }],
            fn_vars: vec![],
            actions: vec![writes("SetB", "b"), writes("SetC", "c")],
            invariants: vec![],
        };
        let state = |pairs: &[(&'static str, i64)]| pairs.iter().copied().collect::<State>();
        assert_eq!(
            reachable_states(&m),
            [
                state(&[("a", 0)]),
                state(&[("a", 0), ("b", 1)]),
                state(&[("a", 0), ("c", 1)]),
                state(&[("a", 0), ("b", 1), ("c", 1)]),
            ]
        );
        assert_eq!(bmc(&m), Ok(4));
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
