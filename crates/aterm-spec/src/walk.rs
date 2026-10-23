// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Model-walking helpers shared by Tier-0 `aterm-models-*` test suites.

use crate::derive::Model;
use crate::interp::State;

/// Returns a clone of `m` retaining only the invariant named `name`.
pub fn only(m: &Model, name: &str) -> Model {
    let mut one = m.clone();
    one.invariants.retain(|inv| inv.name == name);
    assert_eq!(one.invariants.len(), 1, "{name} is an invariant");
    one
}

/// Fires `actions` in order from `m`'s initial state, asserting each is enabled.
pub fn walk(m: &Model, actions: &[&str]) -> State {
    let mut state = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}
