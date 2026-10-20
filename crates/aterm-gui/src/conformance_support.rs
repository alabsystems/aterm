// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Checks shared by the in-crate Tier-1 conformance binds: one real step, projected onto a
//! derived model, admitted as exactly the action the test names with every invariant holding
//! after it; and the enumeration of every bounded schedule a bind replays. Each bind keeps its
//! own projection, schedules and negative controls.

#![cfg(test)]

use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

/// The real step `before -> after` is admitted by `model` specifically as
/// `action`, and every invariant of `model` holds after it.
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

/// [`assert_transition`], and `after` is the only state `action` leads to
/// from `before`: the real step is the model's step, not one of several.
pub(crate) fn assert_sole_transition(model: &Model, before: &State, after: &State, action: &str) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "real transition must conform specifically to {action}"
    );
    assert_transition(model, before, after, action);
}

/// The real step `before -> after`, labelled `label`, is `actions` fired in
/// order on `model` from `before`, and every invariant holds after it.
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

/// Every schedule of `model` over `actions`, up to `depth` steps, that cannot
/// be extended inside it: the full enumeration, not a hand-picked few.
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

/// The stack a Tier-1 body that builds an `App` runs on ([`on_conformance_stack`]). An
/// unoptimized test build moves a `WindowState` by value through every frame of
/// `BTreeMap::insert`; on 2026-09-30 input_release_pairing's `two_window_app` ->
/// `App::insert_logical_window` chain needed 2,135,360 bytes of frames, past libtest's
/// 2 MiB thread stack, and the SIGBUS at the guard page killed the whole `aterm-gui --lib`
/// binary with its tests unreported. 32 MiB is reserved, not touched; the by-value moves
/// are the size, and this stack only gives them room.
const CONFORMANCE_BODY_STACK: usize = 32 << 20;

/// Run a Tier-1 `body` on a thread of its own with [`CONFORMANCE_BODY_STACK`] and hand
/// back what it returns; a panic in it is this caller's panic. The thread is joined
/// before this returns, so a body's thread-local evidence windows (`aterm_spec::xref`)
/// open and close on that one thread.
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
