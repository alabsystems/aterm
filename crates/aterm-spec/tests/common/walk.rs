// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The small model-walking helpers the `derived_*` files share: one copy,
//! mounted once by `spec_suite.rs` and reached as `crate::walk`.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::collections::{BTreeSet, HashSet, VecDeque};

use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

/// `m` with only the invariant `name` kept: the interpreter's whole-space
/// check of that one claim.
pub fn only(m: &Model, name: &str) -> Model {
    let mut one = m.clone();
    one.invariants.retain(|inv| inv.name == name);
    assert_eq!(one.invariants.len(), 1, "{name} is an invariant");
    one
}

/// Fire `actions` in order from `m`'s initial state, each one required to be
/// enabled.
pub fn walk(m: &Model, actions: &[&str]) -> State {
    let mut state = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

/// Every state `m` reaches (first successor per action), invariants unchecked.
pub fn reachable(m: &Model) -> Vec<State> {
    // `Resolved::successors` lists `fire`'s successors in the same order, without the
    // string-keyed map `fire` builds per call.
    let r = interp::Resolved::new(m).expect("the model resolves");
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([r.init()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if seen.contains(&s) {
            continue;
        }
        for a in 0..m.actions.len() {
            if let Some(next) = r.successors(a, &s).into_iter().next() {
                queue.push_back(next);
            }
        }
        out.push(r.state(&s));
        seen.insert(s);
    }
    out
}

/// Every action the committed machine (`Buggy = 0`) can take — an audit's
/// subject; the others are its negative controls.
pub fn committed_actions(m: &Model) -> BTreeSet<&'static str> {
    interp::fired_actions(&interp::with_buggy(m, 0))
}
