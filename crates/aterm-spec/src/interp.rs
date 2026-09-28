// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The IN-PROCESS model-checking tier (VERIFY-1): exhaustive bounded model
//! checking of [`crate::derive::Model`]s through the embedded executable
//! interpreter — no external toolchain.
//!
//! This is genuine model-checking, not example-testing: [`bmc`] enumerates the
//! ENTIRE bounded reachable state space by BFS over [`Model::successors`] (which
//! fans out the nondeterministic `\in lo..hi` picks exactly as `ty`'s
//! existential search does) and asserts every invariant at every reachable
//! state; [`find_deadlock`] is the interpreter twin of `ty`'s `CHECK_DEADLOCK`;
//! [`find_nonprogress_cycle`] is the twin of a `ty` temporal `PROPERTY`
//! `[]<>goal` under `WF_vars`/`SF_vars` fairness — the check that sees a
//! LIVELOCK, which neither of the other two can; [`admits`] is the twin of a
//! two-step `ty trace validate` (does the model's `Next` admit a real
//! `prev -> next` transition?).
//!
//! Promoted from `tests/introspection_bmc.rs` (owner decision 2026-07-06,
//! VERIFY-1): the interpreter tier is now the DEFAULT discharge path for every
//! derived-model obligation — a fresh clone verifies for real, with zero
//! toolchain — and the external `ty` binary is the ESCALATION tier layered on
//! top wherever it is installed (see [`crate::verify`]'s tiered helpers). The
//! two tiers check the SAME derived model; a disagreement between them is a
//! checker bug and panics rather than being silently swallowed.
//!
//! SCOPE: scalar models only. Function-valued models (`Model::fn_vars`
//! non-empty, `[1..N -> BOOLEAN]`) are TLA+-generation-only — the integer
//! interpreter cannot evaluate them (`Expr` panics by design), so those stay on
//! the `ty` tier exclusively (callers gate on `m.fn_vars.is_empty()`; the
//! tiered helpers in [`crate::verify`] do this for you).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::derive::{Liveness, Model};

/// One concrete interpreter state: every scalar variable's value.
pub type State = BTreeMap<&'static str, i64>;

/// Guard against a runaway state space: every derived model is BOUNDED by
/// construction (small constant domains), so crossing this many reachable
/// states means the bounds regressed — fail loudly rather than spin.
const MAX_STATES: usize = 100_000;

/// The BFS drivers below evaluate the BORROWED `&Action`/`&Invariant` the loop
/// already holds — via [`Model::successors_in`] and `inv.expr.eval` — rather
/// than re-resolving each by NAME through [`Model::successors`] /
/// [`Model::check_invariant`], which rebuild the whole evaluation environment
/// and `find` the FIRST record carrying that name. The two forms agree exactly
/// when names are pairwise distinct within a model: with a duplicate, the
/// by-name form would evaluate the first record TWICE and never the second.
/// Every derived model satisfies this by construction; assert it in debug builds
/// (which is what `cargo test` runs) so a future duplicate surfaces as a loud
/// failure rather than a silently skipped action or invariant.
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

/// Exhaustive bounded model check: BFS the reachable state space via
/// [`Model::successors`] over every action, checking every invariant at every
/// state. Returns `Ok(n_states)` if all invariants hold everywhere, or
/// `Err((violating_state, invariant_name))` at the first violation.
///
/// # Panics
///
/// If the reachable space exceeds [`MAX_STATES`] (a model-bounds regression,
/// never a property of a healthy derived model).
// Skip: the bounded model-check driver — BTreeSet/Map keyed frontier +
// caller-chosen closures (absent std bodies). Spec-model machinery, same
// tier as `find_deadlock`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn bmc(m: &Model) -> Result<usize, (State, &'static str)> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    let key = |s: &State| -> Vec<(&'static str, i64)> { s.iter().map(|(k, v)| (*k, *v)).collect() };
    let mut seen: BTreeSet<Vec<(&'static str, i64)>> = BTreeSet::new();
    let mut q: VecDeque<State> = VecDeque::new();
    let init = m.init_state();
    seen.insert(key(&init));
    q.push_back(init);
    let mut n = 0usize;
    while let Some(st) = q.pop_front() {
        n += 1;
        assert!(
            n < MAX_STATES,
            "{} state space unexpectedly large — tighten bounds",
            m.name
        );
        // The env is a pure function of (consts, pre-state), so it is the SAME
        // map for every invariant and every action at this state: build it once
        // here instead of once per (state, invariant) and once per (state,
        // action) inside `check_invariant`/`successors`. On the committed
        // registry that is ~1.6M fewer throwaway `BTreeMap`s per sweep — 2.54 s
        // -> 0.75 s in release, identical verdicts and counterexamples.
        let env = m.eval_env(&st);
        for inv in &m.invariants {
            if !m.check_invariant_in(inv, &env) {
                return Err((st, inv.name));
            }
        }
        for a in &m.actions {
            for ns in m.successors_in(a, &env, &st) {
                if seen.insert(key(&ns)) {
                    q.push_back(ns);
                }
            }
        }
    }
    Ok(n)
}

/// The prove-and-catch protocol (the `Buggy` convention), interpreter tier: the
/// invariant must HOLD across the whole bounded space at `Buggy = 0`, and a
/// counterexample state must be REACHABLE at `Buggy = 1` — so the property is
/// both true and non-trivial (it genuinely catches the audited defect).
///
/// # Panics
///
/// On a genuine invariant violation at `Buggy = 0`, or a vacuous property
/// (no counterexample at `Buggy = 1`) — the same failures the `ty` tier fails.
// Skip: the prove/catch driver — BTreeSet/Map keyed frontier + the
// deliberate harness asserts (a violated model MUST fail loudly). Spec
// machinery, same tier as `bmc`/`find_deadlock`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch(m: &Model) {
    match bmc(&with_buggy(m, 0)) {
        Ok(n) => eprintln!(
            "{}: invariant proven over {n} reachable states (interpreter, Buggy=0).",
            m.name
        ),
        Err((st, inv)) => panic!("{} invariant `{inv}` VIOLATED at {st:?} (Buggy=0)", m.name),
    }
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
}

/// The set of action names that FIRE — have at least one successor from some
/// reachable state — anywhere in `m`'s bounded reachable space. Invariants are
/// deliberately NOT checked and exploration does not stop at a violating state:
/// this answers the pure reachability question the strict-vacuity dead-action
/// audit asks (a prove-and-catch mutant typically fires INTO its violating
/// state, so stopping at the first violation would undercount).
///
/// # Panics
///
/// If the reachable space exceeds [`MAX_STATES`] (a model-bounds regression),
/// exactly like [`bmc`].
#[must_use]
// Skip: same BFS driver class as `bmc`/`find_deadlock` — BTreeSet keyed
// frontier over spec-model machinery, not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fired_actions(m: &Model) -> BTreeSet<&'static str> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    let key = |s: &State| -> Vec<(&'static str, i64)> { s.iter().map(|(k, v)| (*k, *v)).collect() };
    let mut seen: BTreeSet<Vec<(&'static str, i64)>> = BTreeSet::new();
    let mut fired: BTreeSet<&'static str> = BTreeSet::new();
    let mut q: VecDeque<State> = VecDeque::new();
    let init = m.init_state();
    seen.insert(key(&init));
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
        let env = m.eval_env(&st);
        for a in &m.actions {
            for ns in m.successors_in(a, &env, &st) {
                fired.insert(a.name);
                if seen.insert(key(&ns)) {
                    q.push_back(ns);
                }
            }
        }
    }
    fired
}

/// A reachable state with NO successor under ANY action, that is also NOT a
/// declared-final state, is a DEADLOCK — the interpreter twin of `ty`'s
/// `CHECK_DEADLOCK`. [`Model::successors`] returns an empty `Vec` for a disabled
/// guard, so a wedge is a BFS-reachable state where every action yields no
/// successor.
#[must_use]
// Skip: `impl Fn` is CALLER-CHOSEN code (the user-T dispatch class) and the
// residual `BTreeSet::new` awaits its totality entry. Model-exploration
// machinery, same tier as `eval`/`successors`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn find_deadlock(m: &Model, is_final: impl Fn(&State) -> bool) -> Option<State> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    let key = |s: &State| -> Vec<(&'static str, i64)> { s.iter().map(|(k, v)| (*k, *v)).collect() };
    let mut seen: BTreeSet<Vec<(&'static str, i64)>> = BTreeSet::new();
    let mut q: VecDeque<State> = VecDeque::new();
    let init = m.init_state();
    seen.insert(key(&init));
    q.push_back(init);
    while let Some(st) = q.pop_front() {
        let mut any_succ = false;
        // One env per popped state, shared by every action — see `bmc`.
        let env = m.eval_env(&st);
        for a in &m.actions {
            for ns in m.successors_in(a, &env, &st) {
                any_succ = true;
                if seen.insert(key(&ns)) {
                    q.push_back(ns);
                }
            }
        }
        if !any_succ && !is_final(&st) {
            return Some(st); // stuck, and not a legitimate work-complete terminal
        }
    }
    None
}

/// What a liveness verdict may ASSUME will happen: weak and strong fairness over
/// named actions.
///
/// Every action NOT named is unconstrained. The environment — a person typing, a
/// program streaming, a desk that refuses, a release that may never be published —
/// may take it, or never take it again. Every state may also STUTTER forever,
/// which is TLA+'s `[][Next]_vars` reading of a behaviour, so the interpreter and
/// `ty` judge the same set of runs. A name here is an assumption the verdict
/// rests on, which is why nothing is fair by default: an unstated assumption
/// would be a proof about a friendlier world than the one the code runs in.
///
/// * `weak` is `WF_vars(A)`: an action that stays enabled from some point on is
///   eventually taken. The honest assumption for a clock, a capped hold, a
///   deadline, or a step nothing else can disable once it is due.
/// * `strong` is `SF_vars(A)`: an action enabled again and again is eventually
///   taken, even while the environment keeps disabling it in between. The honest
///   assumption for a poll whose gate the environment toggles: the lane re-reads
///   the gate at every instant, so a gate that keeps coming open is caught open.
///
/// "Enabled" means enabled to CHANGE the state (TLA+'s `ENABLED <<A>>_vars`): a
/// step that writes every variable back to its own value is a stutter, and
/// fairness neither demands it nor credits it.
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
    /// From `Init` to the loop's first state: `("Init", init)`, then one
    /// `(action, state after it)` per step. The stem may pass through goal
    /// states: the property is that the goal holds AGAIN AND AGAIN, so leaving it
    /// for good after reaching it once is still the failure.
    pub stem: Vec<(&'static str, State)>,
    /// The loop, from the stem's last state back to it, one `(action, state
    /// after it)` per step. EMPTY means the run stutters at that state forever:
    /// nothing the fairness names is enabled to move it on (a wedge is the case
    /// where nothing at all is).
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
    // Skip: diagnostic rendering over BTreeMap walks and `write!` (the
    // absent-body formatting class). Spec-model machinery, same tier as `bmc`.
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
    /// Per state, `(action index, successor index)` for every step that CHANGES
    /// the state — the steps fairness can demand.
    steps: Vec<Vec<(usize, usize)>>,
    /// How BFS first reached each state (`None` for `Init`): the stem's source.
    parent: Vec<Option<(usize, usize)>>,
}

impl StateGraph {
    // Skip: BTreeMap-keyed BFS over caller-built models (the same driver class
    // as `bmc`/`find_deadlock`), plus the deliberate bounds-regression assert.
    #[cfg_attr(trust_verify, trust::skip)]
    fn explore(m: &Model) -> Self {
        let key =
            |s: &State| -> Vec<(&'static str, i64)> { s.iter().map(|(k, v)| (*k, *v)).collect() };
        let init = m.init_state();
        let mut ids: BTreeMap<Vec<(&'static str, i64)>, usize> = BTreeMap::new();
        ids.insert(key(&init), 0);
        let mut graph = Self {
            states: vec![init],
            steps: vec![Vec::new()],
            parent: vec![None],
        };
        let mut next = 0;
        while next < graph.states.len() {
            let st = graph.states[next].clone();
            // One env per popped state, shared by every action — see `bmc`.
            let env = m.eval_env(&st);
            let mut out: Vec<(usize, usize)> = Vec::new();
            for (a, act) in m.actions.iter().enumerate() {
                for ns in m.successors_in(act, &env, &st) {
                    if ns == st {
                        continue; // a stutter: fairness neither demands nor credits it
                    }
                    let id = match ids.get(&key(&ns)) {
                        Some(&id) => id,
                        None => {
                            let id = graph.states.len();
                            assert!(
                                id < MAX_STATES,
                                "{} state space unexpectedly large — tighten bounds",
                                m.name
                            );
                            ids.insert(key(&ns), id);
                            graph.states.push(ns);
                            graph.steps.push(Vec::new());
                            graph.parent.push(Some((a, next)));
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
        graph
    }

    fn enabled(&self, state: usize, action: usize) -> bool {
        self.steps[state].iter().any(|&(a, _)| a == action)
    }

    /// Tarjan's strongly connected components of the subgraph `nodes` induces,
    /// each sorted ascending. Iterative: a recursive walk would be as deep as the
    /// longest simple path, which a bounded model can still make thousands long.
    // Skip: BTreeMap/BTreeSet-keyed bookkeeping over the caller's graph (the
    // absent-std-body class), same tier as `bmc`.
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

    /// The lasso a fair run can follow in `component`: a stem to its first
    /// state, then a loop that takes one step of every fair action that has a
    /// step inside it and, for a weakly fair action with none, visits a state
    /// where it is disabled. Each of those is what that action's fairness
    /// demands of a run confined here, and a loop that meets every demand is a
    /// fair run by construction.
    // Skip: witness assembly over the caller's graph (BTreeSet/Vec bookkeeping,
    // the absent-std-body class), same tier as `bmc`.
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

/// The action indices a fairness list names. An undeclared name panics: a
/// fairness assumption about an action the model does not have is a spec typo,
/// and dropping it silently would change what the verdict assumes.
// Skip: name resolution with a deliberate harness panic. Spec-model machinery.
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

/// A reachable NON-PROGRESS cycle under `fairness`: a fair behaviour along which
/// `is_goal` never holds again — the counterexample to `[]<>goal`, the property
/// "the goal keeps being reached". `None` means every fair behaviour of the
/// bounded model keeps reaching the goal.
///
/// WHY THIS EXISTS. [`bmc`] sees a bad STATE and [`find_deadlock`] sees a state
/// with no way out. A LIVELOCK is neither: every state on it is legal and has
/// successors — the terminal keeps producing output, the lane keeps retrying —
/// and it simply never lands. A model whose environment can always act has no
/// deadlock at all, so a livelock passes every check this module had. The
/// 2026-09-23 update audit found exactly that shape in the shipped lane: a
/// capture refusal re-filed as activity and retried every fifteen minutes,
/// forever, while every model check stayed green.
///
/// THE ALGORITHM. Explore the whole reachable graph (stutter steps dropped),
/// take the strongly connected components of its non-goal states, and ask of
/// each whether a fair run can stay in it forever (Emerson–Lei):
///
/// * a WEAKLY fair action enabled at EVERY state of the component with no step
///   inside it stays enabled on any run confined there and is never taken:
///   no fair run stays, so the component is dropped;
/// * a STRONGLY fair action enabled at SOME state with no step inside: a fair run
///   may stay only by avoiding the states where it is enabled, so those states
///   are removed and the rest is split into components and asked again;
/// * a component that survives both is fair: a loop through all of it takes
///   every fair action that has a step there and visits a state where every
///   other weakly fair action is off, which is exactly what fairness demands.
///
/// A single state counts, because a state may stutter forever: a non-goal state
/// where no fair action is enabled is a stall (a wedge when nothing at all is),
/// and it is reported with an empty `cycle`. So this subsumes [`find_deadlock`]
/// for liveness purposes, and agrees with `ty`'s reading of `[][Next]_vars`.
///
/// Components are asked in order of their nearest state to `Init`, so the stem
/// of the reported lasso is a shortest one to that component.
///
/// # Panics
///
/// If `fairness` names an action the model does not declare, or names one
/// action both weakly and strongly fair (one of the two would be dead text); or
/// if the reachable space exceeds [`MAX_STATES`], exactly like [`bmc`].
#[must_use]
// Skip: `impl Fn` is CALLER-CHOSEN code (the user-T dispatch class) over the
// BTreeMap/BTreeSet-keyed graph above. Spec-model machinery, same tier as
// `find_deadlock`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn find_nonprogress_cycle(
    m: &Model,
    is_goal: impl Fn(&State) -> bool,
    fairness: &Fairness<'_>,
) -> Option<NonProgressCycle> {
    debug_assert!(
        names_are_unique(m),
        "{}: duplicate action/invariant name",
        m.name
    );
    let weak = fair_indices(m, fairness.weak, "weak");
    let strong = fair_indices(m, fairness.strong, "strong");
    if let Some(&both) = weak.iter().find(|a| strong.contains(a)) {
        panic!(
            "{}: `{}` is named both weakly and strongly fair — say which",
            m.name, m.actions[both].name
        );
    }
    let graph = StateGraph::explore(m);
    let non_goal: Vec<usize> = (0..graph.states.len())
        .filter(|&s| !is_goal(&graph.states[s]))
        .collect();
    // Keyed by each component's least (nearest-to-Init) state: components are
    // disjoint, so the keys are distinct, and `pop_first` asks the nearest first.
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

/// [`find_nonprogress_cycle`] for a declared [`Liveness`] obligation: its goal
/// expression and its stated fairness, evaluated against `m` (which may be a
/// `Buggy` flip or a mutant-isolated copy of the model it was written for).
#[must_use]
pub fn nonprogress_under(m: &Model, live: &Liveness) -> Option<NonProgressCycle> {
    find_nonprogress_cycle(
        m,
        |state| live.goal_holds(m, state),
        &Fairness {
            weak: &live.weak,
            strong: &live.strong,
        },
    )
}

/// Tier-1 conformance twin of a two-step `ty trace validate`: does the model's
/// `Next` (∃ some action) admit the real `prev -> next` transition? `prev` need
/// not be `Init`-reachable — exactly like `transition_spec`'s parameterized
/// `Init`, the question is only whether SOME action's guard + updates produce
/// `next` from `prev`. Returns the admitting action's name, or `None` (the
/// transition does not conform — the negative-control rejection).
#[must_use]
pub fn admits(m: &Model, prev: &State, next: &State) -> Option<&'static str> {
    // Every candidate action is evaluated at the SAME `prev`, so the env is
    // loop-invariant — build it once rather than once per action. (No
    // `names_are_unique` debug-assert here: unlike the BFS drivers this fn is
    // not `trust::skip`ped, and the assert would add a bare panic obligation.
    // The invariant it guards is the same one, and every model reaching `admits`
    // is BMC'd by the same suites.)
    let env = m.eval_env(prev);
    m.actions
        .iter()
        .find(|a| m.successors_in(a, &env, prev).contains(next))
        .map(|a| a.name)
}

/// The ONE successor of `state` under `action` whose `var` is `value` — how a
/// Tier-1 lattice walk chooses its point among a nondeterministic
/// `\in lo..hi` pick while still reaching that point through the model's OWN
/// transition (entering the model by writing the picked values into a state
/// would also accept a point the model cannot reach). `None` when the model
/// cannot pick `value` from `state`.
///
/// # Panics
///
/// If two successors share `var == value`: then `var` does not identify the
/// pick, and choosing between them would be the harness deciding for the
/// model.
// Skip: spec-model harness machinery over caller-built states (a collected
// successor Vec and a filtered iterator), the same tier as `admits`' callers.
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

    /// The promoted checker still proves and catches on a known model — the
    /// same protocol `tests/introspection_bmc.rs` pinned before promotion.
    #[test]
    fn promoted_bmc_proves_and_catches() {
        prove_and_catch(&subscribe_model());
    }

    /// `admits` accepts a real ring transition and rejects two corrupted ones —
    /// the interpreter twin of the conformance tests' positive + negative
    /// controls.
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

    /// A reported lasso must BE one: every stem and loop step admitted by the
    /// action it names, the loop closing on its entry, and the goal false at
    /// every state of the loop — so a verdict is checked against the model's
    /// own transition relation, not taken on the checker's word.
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

    /// THE SHAPE THE CHECKER EXISTS FOR: a lane that retries forever and never
    /// lands. Every state is legal and every state has a successor, so the
    /// invariant check and the deadlock check both pass it — exactly how a
    /// livelock got past this module before. The non-progress check finds the
    /// loop, and does not find one in the lane that can land.
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

    /// FAIRNESS EXCLUDES STARVATION-ONLY CYCLES. A loop that exists only by
    /// never taking an action that stays enabled is no fair behaviour (weak
    /// fairness); a loop that only exists by never taking an action it keeps
    /// passing an open gate for is no strongly fair one. Each is found without
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

    /// Strong fairness removes the states where the gate is open, not the whole
    /// component: a loop that never passes the gate is still a fair livelock
    /// (Emerson–Lei). A checker that dropped any component in which the gate
    /// opens somewhere would miss it; one where every loop passes the gate lands.
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

    /// The iterative Tarjan pass against the definition it implements: two
    /// states share a component exactly when each reaches the other inside the
    /// subset asked about. An iterative Tarjan is easy to get subtly wrong (the
    /// low-link hand-back to the caller frame), and a wrong component would make
    /// the fairness pruning answer about a set that is not strongly connected.
    /// Pseudo-random graphs from a fixed xorshift seed, so a failure replays.
    #[test]
    fn components_are_exactly_the_mutual_reachability_classes() {
        fn xorshift(seed: &mut u64) -> u64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed
        }
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        for _ in 0..300 {
            let n = usize::try_from(xorshift(&mut seed) % 12 + 1).unwrap_or(1);
            let pick = |seed: &mut u64, bound: usize| {
                usize::try_from(xorshift(seed) % u64::try_from(bound).unwrap_or(1)).unwrap_or(0)
            };
            let steps: Vec<Vec<(usize, usize)>> = (0..n)
                .map(|_| {
                    let fan = pick(&mut seed, 4);
                    (0..fan).map(|_| (0, pick(&mut seed, n))).collect()
                })
                .collect();
            let graph = StateGraph {
                states: vec![State::new(); n],
                steps,
                parent: vec![None; n],
            };
            let nodes: Vec<usize> = (0..n).filter(|_| pick(&mut seed, 4) != 0).collect();
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
