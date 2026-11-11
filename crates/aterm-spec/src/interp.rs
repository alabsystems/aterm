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
/// construction). The rotate is spelled as two shifts: a dev build of this crate
/// (opt-level 1, which shares generics) otherwise calls an opt-level-0 crate's
/// out-of-line copy of the rotate intrinsic for every value of every state.
#[inline]
#[expect(
    clippy::manual_rotate,
    reason = "`rotate_left` is an out-of-line opt-level-0 call in this crate's dev build"
)]
const fn key_hash(key: &[i64]) -> u64 {
    let mut h = key.len() as u64;
    let mut i = 0;
    while i < key.len() {
        h = (((h << 5) | (h >> 59)) ^ key[i] as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        i += 1;
    }
    h
}

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

trait Reached<S: ?Sized>: Default {
    fn len(&self) -> usize;
    fn get(&self, i: usize) -> &S;
    fn number(&mut self, s: &S) -> (usize, bool);
}

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

trait Walker {
    type S: ?Sized + PartialEq;
    type Reached: Reached<Self::S>;
    type Held: Default + std::borrow::Borrow<Self::S>;
    type Next: Default;
    type Env;
    fn init(&self) -> Self::Held;
    fn hold(&self, s: &Self::S, held: &mut Self::Held);
    fn env(&self, s: &Self::S) -> Self::Env;
    fn holds(&self, env: &Self::Env, s: &Self::S, i: usize) -> bool;
    fn successors(&self, env: &Self::Env, s: &Self::S, a: usize, next: &mut Self::Next) -> usize;
    fn nth<'n>(&self, next: &'n Self::Next, k: usize) -> &'n Self::S;
    fn state(&self, s: &Self::S) -> State;
}

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

struct Walk<W: Walker> {
    reached: W::Reached,
    held: W::Held,
    next: W::Next,
    /// How many states have been expanded: the next to expand is this number.
    expanded: usize,
}

impl<W: Walker> Walk<W> {
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

    fn pop(&mut self, w: &W) -> bool {
        if self.expanded == self.reached.len() {
            return false;
        }
        w.hold(self.reached.get(self.expanded), &mut self.held);
        self.expanded += 1;
        true
    }
}

/// A model resolved once, for a caller that walks its space itself.
pub struct Resolved(Compiled);

impl Resolved {
    /// Resolve `m`; `None` for a model whose updates write a name its initial state does not bind.
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
    /// # Panics
    /// If `name` is not one of the model's state variables.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn slot(&self, name: &str) -> usize {
        self.0
            .slot(name)
            .unwrap_or_else(|| panic!("`{name}` is not a state variable"))
    }

    /// Every successor of `s` under the model's `a`-th action.
    /// # Panics
    /// If `a` is out of range or `s.len()` does not match the model's state width.
    #[must_use]
    pub fn successors(&self, a: usize, s: &[i64]) -> Vec<Vec<i64>> {
        self.0.successors(a, s)
    }

    /// Whether the model's `i`-th invariant holds at `s` ([`Model::check_invariant_in`]).
    /// # Panics
    /// If `i` is out of range or `s.len()` does not match the model's state width.
    #[must_use]
    pub fn holds(&self, i: usize, s: &[i64]) -> bool {
        self.0.holds(i, s)
    }

    /// `s` as a [`State`].
    /// # Panics
    /// If `s.len()` does not match the model's state width.
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

/// A copy of `m` with the named constants overridden.
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

/// A copy of `m` with its `Buggy` constant set to `b`.
#[must_use]
pub fn with_buggy(m: &Model, b: i64) -> Model {
    with_consts(m, &[("Buggy", b)])
}

/// Exhaustive bounded model check.
/// # Panics
/// If the reachable space exceeds [`MAX_STATES`] (a model-bounds regression, never a property of a healthy derived model).
#[cfg_attr(trust_verify, trust::skip)]
pub fn bmc(m: &Model) -> Result<usize, (State, &'static str)> {
    bmc_fired(m).0
}

/// A separate, explicit work budget for a reduced proof target. Exhaustion still refuses.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn bmc_with_limit(m: &Model, limit: usize) -> Result<usize, (State, &'static str)> {
    assert!(limit > 0, "a model-check budget must be positive");
    assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    walk!(bmc_on(m, limit)).0
}

/// A bmc verdict, beside the actions that fired in the space walked to reach it.
pub type BmcFired = (Result<usize, (State, &'static str)>, BTreeSet<&'static str>);

/// [`bmc`], and the actions that fired (had a successor) from the states it checked.
/// # Panics
/// As [`bmc`].
#[cfg_attr(trust_verify, trust::skip)]
pub fn bmc_fired(m: &Model) -> BmcFired {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    let (verdict, fired) = walk!(bmc_on(m, MAX_STATES));
    let fired = m
        .actions
        .iter()
        .zip(fired)
        .filter_map(|(a, f)| f.then_some(a.name))
        .collect();
    (verdict, fired)
}

#[cfg_attr(trust_verify, trust::skip)]
fn bmc_on<W: Walker>(
    m: &Model,
    w: &W,
    limit: usize,
) -> (Result<usize, (State, &'static str)>, Vec<bool>) {
    let mut fired = vec![false; m.actions.len()];
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        assert!(
            walk.expanded < limit,
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
    walk!(violated_invariants_on(m))
}

#[cfg_attr(trust_verify, trust::skip)]
fn violated_invariants_on<W: Walker>(m: &Model, w: &W) -> BTreeSet<&'static str> {
    let mut violated: BTreeSet<&'static str> = BTreeSet::new();
    let mut live: Vec<usize> = (0..m.invariants.len()).collect();
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        assert!(
            walk.expanded < MAX_STATES,
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
/// # Panics
/// If `m` declares no action named one of `actions`, or its reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn violation_steps(m: &Model, actions: &[&str]) -> Vec<Option<bool>> {
    let watched = watched_actions(m, actions);
    walk!(mutant_report_on(m, &watched, false)).1
}

/// What the non-vacuity sweep asks of one mutant, in one walk where asking each costs a walk of
/// its own: the invariants some reachable state violates ([`violated_invariants`]), and
/// [`violation_steps`] of `actions`. At the search limit, claim witnesses remain valid but an
/// unresolved bound is not a verdict.
/// # Panics
/// If `m` declares no action named one of `actions`, if its reachable space exceeds [`MAX_STATES`]
/// while a bound cannot be proved by its closed projection or an action has not yet landed lawful, or on an
/// expression the interpreter cannot evaluate (a function-valued model).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn mutant_report(m: &Model, actions: &[&str]) -> (BTreeSet<&'static str>, Vec<Option<bool>>) {
    let watched = watched_actions(m, actions);
    walk!(mutant_report_on(m, &watched, true))
}

#[cfg_attr(trust_verify, trust::skip)]
fn watched_actions(m: &Model, actions: &[&str]) -> Vec<usize> {
    actions
        .iter()
        .map(|action| {
            m.actions
                .iter()
                .position(|act| act.name == *action)
                .unwrap_or_else(|| panic!("{} declares no action `{action}`", m.name))
        })
        .collect()
}

/// [`mutant_report`], where `laws` says whether to read the invariants' side at all. The walk stops
/// early only once neither reader has anything left to learn (every law seen broken, every watched
/// action landed lawful). At [`MAX_STATES`] unresolved bounds need a complete closed-projection
/// proof; action verdicts must already be known. Claims return the witnesses found so far, which
/// the caller can combine across mutants.
#[cfg_attr(trust_verify, trust::skip)]
fn mutant_report_on<W: Walker>(
    m: &Model,
    w: &W,
    watched: &[usize],
    laws: bool,
) -> (BTreeSet<&'static str>, Vec<Option<bool>>) {
    let lawful = |env: &W::Env, s: &W::S| upto(m.invariants.len()).all(|i| w.holds(env, s, i));
    let mut violated: BTreeSet<&'static str> = BTreeSet::new();
    let mut live: Vec<usize> = if laws {
        (0..m.invariants.len()).collect()
    } else {
        Vec::new()
    };
    let mut fired = vec![false; watched.len()];
    let mut landed_lawful = vec![false; watched.len()];
    let mut open = watched.len();
    // Whether each action is watched: asked of every action of every lawful state.
    let watches: Vec<bool> = upto(m.actions.len())
        .map(|a| watched.contains(&a))
        .collect();
    let mut walk = Walk::new(w);
    while walk.pop(w) {
        if walk.expanded >= MAX_STATES {
            assert!(
                open == 0,
                "{} state space unexpectedly large — tighten bounds",
                m.name
            );
            for &i in &live {
                let law = &m.invariants[i];
                if law.kind == crate::derive::InvariantKind::Bound
                    && !crate::bound_projection::holds(m, law)
                {
                    violated.insert(law.name);
                }
            }
            break;
        }
        let st = walk.held.borrow();
        let env = w.env(st);
        let before = live.len();
        live.retain(|&i| {
            let keep = w.holds(&env, st, i);
            if !keep {
                violated.insert(m.invariants[i].name);
            }
            keep
        });
        if open == 0 && live.is_empty() {
            break;
        }
        // Where every law is still live and none just fell, each held here already.
        let from_lawful = open > 0
            && before == live.len()
            && (live.len() == m.invariants.len() || lawful(&env, st));
        for a in upto(m.actions.len()) {
            let watching = from_lawful && watches[a];
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
                }
                walk.reached.number(ns);
            }
        }
    }
    let verdicts = fired
        .iter()
        .zip(&landed_lawful)
        .map(|(&f, &l)| if l { Some(false) } else { f.then_some(true) })
        .collect();
    (violated, verdicts)
}

/// Every state reachable in `m`'s bounded space, each once, in BFS order.
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
/// # Panics
/// If the reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
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
        assert!(
            walk.expanded < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
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
    /// From `Init` to the loop's first state.
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
                graph.fired.insert(act.name);
            }
            for k in upto(made) {
                let ns = w.nth(&walk.next, k);
                if ns == st {
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

enum Walked {
    /// The model resolved once and each state as its value slice: a liveness
    /// goal is evaluated without building an environment map per state, and a
    /// map is built only for a state a caller asks to see.
    Compiled(Compiled, Seen),
    /// A model with no compiled form: each state as written.
    ByName(Kept),
}

impl Walked {
    fn state(&self, i: usize) -> State {
        match self {
            Walked::Compiled(c, walked) => c.state(walked.key(i)),
            Walked::ByName(states) => states.get(i).clone(),
        }
    }

    fn with_state<R>(&self, i: usize, f: impl FnOnce(&State) -> R) -> R {
        match self {
            Walked::Compiled(c, walked) => f(&c.state(walked.key(i))),
            Walked::ByName(states) => f(states.get(i)),
        }
    }
}

impl Space {
    /// Walk `m`'s reachable space.
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

    /// [`find_nonprogress_cycle`] over this space's model, without walking it again.
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
#[path = "interp_tests.rs"]
mod tests;
