// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! THE GUARD-CONJUNCT AUDIT, measured — shared by every Tier-0 test that pins what
//! each conjunct of a model's guards carries (`derived_trail_sound_ty.rs`,
//! `derived_release_publish_once.rs`).
//!
//! The recorded gap it exists for: a green run missed a real race because a guard
//! CONJUNCT made the racing state unreachable and the invariant naming it was
//! vacuously true. For every top-level conjunct of every guard the audit reports WHAT
//! DROPPING IT DOES on the committed machine (`Buggy = 0`): which invariants fail,
//! each checked ALONE, and whether it changes a NON-STUTTER reachable transition (a
//! self-loop is not behaviour). Each test declares the answer it expects, so a new
//! guard, or a guard that starts or stops carrying an invariant, fails until it is
//! declared.

use std::collections::{BTreeSet, VecDeque};

use aterm_spec::derive::{Expr, Model};
use aterm_spec::interp::{self, State};

/// The top-level conjuncts of a guard (`a && (b || c)` gives `a`, `b || c`).
fn conjuncts(e: &Expr) -> Vec<Expr> {
    match e {
        Expr::And(l, r) => {
            let mut out = conjuncts(l);
            out.extend(conjuncts(r));
            out
        }
        other => vec![other.clone()],
    }
}

/// Rebuild a guard from conjuncts, `None` when nothing is left.
fn conjoin(parts: Vec<Expr>) -> Option<Expr> {
    parts
        .into_iter()
        .reduce(|l, r| Expr::And(Box::new(l), Box::new(r)))
}

/// A state as an ordered, comparable key.
type StateKey = Vec<(&'static str, i64)>;

/// Every reachable NON-STUTTER `(pre, action, post)` edge of `m`. A self-loop
/// changes no state, so a guard that only suppresses self-loops is not
/// load-bearing behaviour, and must not read as such.
fn edges(m: &Model) -> BTreeSet<(StateKey, &'static str, StateKey)> {
    let key = |s: &State| s.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    let mut out = BTreeSet::new();
    let mut queue = VecDeque::new();
    let init = m.init_state();
    seen.insert(key(&init));
    queue.push_back(init);
    while let Some(s) = queue.pop_front() {
        for a in &m.actions {
            for n in m.successors(a.name, &s) {
                if n != s {
                    out.insert((key(&s), a.name, key(&n)));
                }
                if seen.insert(key(&n)) {
                    queue.push_back(n);
                }
            }
        }
    }
    out
}

/// What dropping one guard conjunct does to the committed machine.
#[derive(Debug, PartialEq, Eq)]
pub struct ConjunctReport {
    pub action: &'static str,
    pub index: usize,
    /// The invariants that FAIL once it is dropped, each checked alone.
    pub carries: Vec<&'static str>,
    /// Whether it changes a non-stutter reachable transition.
    pub changes_behaviour: bool,
}

/// The audit: one report per top-level conjunct of every guard of the actions
/// `audited` names (every guarded action when it is `None`), in model order.
pub fn audit_guard_conjuncts(m: &Model, audited: Option<&[&str]>) -> Vec<ConjunctReport> {
    let committed = interp::with_buggy(m, 0);
    let baseline = edges(&committed);
    let mut out = Vec::new();
    for (ai, action) in committed.actions.iter().enumerate() {
        if audited.is_some_and(|names| !names.contains(&action.name)) {
            continue;
        }
        let Some(guard) = &action.guard else {
            continue;
        };
        let parts = conjuncts(guard);
        for drop in 0..parts.len() {
            let mut weakened = committed.clone();
            weakened.actions[ai].guard = conjoin(
                parts
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != drop)
                    .map(|(_, p)| p.clone())
                    .collect(),
            );
            let carries = committed
                .invariants
                .iter()
                .filter(|inv| {
                    let mut alone = weakened.clone();
                    alone.invariants.retain(|i| i.name == inv.name);
                    interp::bmc(&alone).is_err()
                })
                .map(|inv| inv.name)
                .collect();
            out.push(ConjunctReport {
                action: action.name,
                index: drop,
                carries,
                changes_behaviour: edges(&weakened) != baseline,
            });
        }
    }
    out
}
