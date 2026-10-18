// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The small model-walking helpers the Tier-0 tests share (`aterm_spec::walk`): one
//! copy, beside the interpreter they drive, for every `aterm-models-*` suite.

use crate::derive::Model;
use crate::interp::State;

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
