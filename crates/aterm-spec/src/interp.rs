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
//! [`admits`] is the twin of a two-step `ty trace validate` (does the model's
//! `Next` admit a real `prev -> next` transition?).
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

use crate::derive::Model;

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
        assert!(!fired0.contains("AdmitStaleNyan"));
        assert!(!fired0.contains("AdmitStaleTheme"));
        assert!(!fired0.contains("AdmitStaleSparkle"));
        let fired1 = fired_actions(&with_buggy(&m, 1));
        assert!(fired1.contains("AdmitStaleTrail"));
        assert!(fired1.contains("AdmitStaleNyan"));
        assert!(fired1.contains("AdmitStaleTheme"));
        assert!(fired1.contains("AdmitStaleSparkle"));
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
}
