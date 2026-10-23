// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared checks and schedule enumeration for in-crate Tier-1 conformance binds.

#![cfg(test)]

use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

/// Assert that `before -> after` is admitted as `action` and preserves all invariants.
pub(crate) fn assert_transition(model: &Model, before: &State, after: &State, action: &str) {
    assert_eq!(
        admits(model, before, after),
        Some(action),
        "the real step {before:?} -> {after:?} must be admitted specifically as {action}"
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state of {action} violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

/// Assert that `after` is the sole successor of `before` under `action`.
pub(crate) fn assert_sole_transition(model: &Model, before: &State, after: &State, action: &str) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "real transition must conform specifically to {action}"
    );
    assert_transition(model, before, after, action);
}

/// Assert that `before -> after` matches firing `actions` in order and preserves invariants.
#[cfg(unix)]
pub(crate) fn assert_fired(
    model: &Model,
    label: &str,
    actions: &[&str],
    before: &State,
    after: &State,
) {
    let mut expected = before.clone();
    for action in actions {
        assert!(
            model.fire(action, &mut expected),
            "{label}: the model cannot fire {action} from {expected:?} (real: {before:?} -> \
             {after:?})"
        );
    }
    assert_eq!(
        *after, expected,
        "{label}: the real step is not {actions:?} (from {before:?})"
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "{label}: {} broken at {after:?}",
            invariant.name
        );
    }
}

/// Enumerate every maximal schedule of `model` over `actions` up to `depth` steps.
#[cfg(unix)]
pub(crate) fn schedules(
    model: &Model,
    actions: &[&'static str],
    depth: usize,
) -> Vec<Vec<&'static str>> {
    fn walk(
        model: &Model,
        actions: &[&'static str],
        state: &State,
        prefix: &mut Vec<&'static str>,
        depth: usize,
        out: &mut Vec<Vec<&'static str>>,
    ) {
        let enabled: Vec<&'static str> = actions
            .iter()
            .copied()
            .filter(|action| model.action_enabled(action, state))
            .collect();
        if prefix.len() == depth || enabled.is_empty() {
            out.push(prefix.clone());
            return;
        }
        for action in enabled {
            let mut next = state.clone();
            assert!(model.fire(action, &mut next));
            prefix.push(action);
            walk(model, actions, &next, prefix, depth, out);
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(
        model,
        actions,
        &model.init_state(),
        &mut Vec::new(),
        depth,
        &mut out,
    );
    out
}

const CONFORMANCE_BODY_STACK: usize = 32 << 20;

/// Run a Tier-1 `body` on a dedicated thread with [`CONFORMANCE_BODY_STACK`].
pub(crate) fn on_conformance_stack<T: Send>(machine: &str, body: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        let thread = std::thread::Builder::new()
            .name(format!("tier1/{machine}"))
            .stack_size(CONFORMANCE_BODY_STACK)
            .spawn_scoped(scope, body)
            .unwrap_or_else(|spawn| panic!("{machine}: its Tier-1 thread did not start: {spawn}"));
        thread
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}
