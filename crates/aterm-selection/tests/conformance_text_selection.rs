// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `TextSelectionLifecycle`
//! (`aterm_spec::derive::text_selection_lifecycle_model`): the REAL
//! `TextSelection` calls, driven from every state the model reaches, against
//! the model's transitions.
//!
//! The driver binds each model action to one call at a fixed point — `Start`
//! presses at [`PRESS`], `Update` drags to [`DRAG`], `Extend` shift-clicks at
//! [`SHIFT_CLICK`] — so the far anchor's position names the call that put it
//! there. [`project`] reads the real selection onto the model's variables,
//! and the observer half (`pre`, `last`, `moved`) from the call just made and
//! the selection before it.

use std::collections::{BTreeMap, BTreeSet};

use aterm_selection::{
    SelectionAnchor, SelectionSide, SelectionState, SelectionType, TextSelection,
};
use aterm_spec::derive::{Model, text_selection_lifecycle_model};
use aterm_spec::verify;

type State = BTreeMap<&'static str, i64>;
type StateKey = Vec<(&'static str, i64)>;

/// Where `Start` presses (`end == 0`), `Update` drags to (`end == 1`) and
/// `Extend` shift-clicks (`end == 2`): `(row, col, side)`.
const PRESS: (i32, u16, SelectionSide) = (0, 0, SelectionSide::Left);
const DRAG: (i32, u16, SelectionSide) = (1, 3, SelectionSide::Right);
const SHIFT_CLICK: (i32, u16, SelectionSide) = (2, 5, SelectionSide::Right);

/// The model's actions, each bound to one real call by [`call`].
const ALPHABET: [&str; 5] = ["Start", "Update", "Complete", "Extend", "Clear"];

/// Bounded depth: every reachable model state is within four calls of Init,
/// and one more call takes each out of it.
const MAX_DEPTH: usize = 6;

/// Make the real call the model action names.
fn call(selection: &mut TextSelection, action: &str) {
    match action {
        "Start" => selection.start_selection(PRESS.0, PRESS.1, PRESS.2, SelectionType::Simple),
        "Update" => selection.update_selection(DRAG.0, DRAG.1, DRAG.2),
        "Complete" => selection.complete_selection(),
        "Extend" => selection.extend_selection(SHIFT_CLICK.0, SHIFT_CLICK.1, SHIFT_CLICK.2),
        "Clear" => selection.clear(),
        other => unreachable!("unknown action {other}"),
    }
}

fn phase(selection: &TextSelection) -> i64 {
    match selection.state() {
        SelectionState::None => 0,
        SelectionState::InProgress => 1,
        SelectionState::Complete => 2,
    }
}

/// Which driver point the far anchor holds; a fresh selection's anchors sit
/// at the origin, which is [`PRESS`].
fn end(anchor: SelectionAnchor) -> i64 {
    match (anchor.row, anchor.col) {
        (row, col) if (row, col) == (PRESS.0, PRESS.1) => 0,
        (row, col) if (row, col) == (DRAG.0, DRAG.1) => 1,
        (row, col) if (row, col) == (SHIFT_CLICK.0, SHIFT_CLICK.1) => 2,
        other => panic!("the far anchor left the driver's points: {other:?}"),
    }
}

/// PROJECTION (`conformance_text_selection::project`, named by the
/// `#[refines]` anchors on the five calls): the selection onto `phase` and
/// `end`, and the last call — `action` made on `before` — onto the observer
/// `pre`, `last` and `moved`. `None` is the fresh selection, before any call.
pub fn project(selection: &TextSelection, step: Option<(&TextSelection, &str)>) -> State {
    let (pre, last, moved) = match step {
        None => (0, 0, 0),
        Some((before, action)) => (
            phase(before),
            1 + ALPHABET
                .iter()
                .position(|&a| a == action)
                .expect("an alphabet action") as i64,
            i64::from(before.end() != selection.end()),
        ),
    };
    State::from([
        ("phase", phase(selection)),
        ("end", end(selection.end())),
        ("pre", pre),
        ("last", last),
        ("moved", moved),
    ])
}

/// Every action sequence up to [`MAX_DEPTH`] on the real selection in lockstep
/// with the model: the projection equals the model state after every call.
/// Returns every step taken as `(prev, next, action)`.
fn lockstep(model: &Model) -> Vec<(State, State, &'static str)> {
    fn walk(
        model: &Model,
        selection: &TextSelection,
        state: &State,
        depth: usize,
        trace: &mut Vec<&'static str>,
        steps: &mut Vec<(State, State, &'static str)>,
    ) {
        if depth == MAX_DEPTH {
            return;
        }
        for action in ALPHABET {
            let mut real = selection.clone();
            call(&mut real, action);
            let mut next = state.clone();
            assert!(model.fire(action, &mut next), "{action} is total");
            trace.push(action);
            assert_eq!(
                project(&real, Some((selection, action))),
                next,
                "projection diverged after {trace:?}"
            );
            steps.push((state.clone(), next.clone(), action));
            walk(model, &real, &next, depth + 1, trace, steps);
            trace.pop();
        }
    }
    let fresh = TextSelection::new();
    let init = model.init_state();
    assert_eq!(project(&fresh, None), init, "a fresh selection is Init");
    let mut steps = Vec::new();
    walk(model, &fresh, &init, 0, &mut Vec::new(), &mut steps);
    steps
}

fn validate(model: &Model, prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        prev,
        next,
        Some(action),
        "text selection lifecycle Tier-1 binding",
    )
}

/// Tier-1: every call from every reachable state, on the real selection in
/// lockstep with the model, and every distinct real transition judged by the
/// tiered validator (the interpreter, and `ty` where installed). The walk
/// reaches the model's whole reachable space — checked against the
/// interpreter's own count, so a state the driver cannot produce is a failure.
#[test]
fn real_calls_conform_to_the_lifecycle_model() {
    let model = text_selection_lifecycle_model();
    assert_eq!(
        model
            .consts
            .iter()
            .find(|(k, _)| *k == "Buggy")
            .map(|c| c.1),
        Some(0),
        "the committed config is the fixed machine"
    );
    let key = |s: &State| -> StateKey { s.iter().map(|(k, v)| (*k, *v)).collect() };
    let steps = lockstep(&model);
    let mut reached: BTreeSet<StateKey> = steps.iter().map(|(_, next, _)| key(next)).collect();
    reached.insert(key(&model.init_state()));
    let reachable: BTreeSet<StateKey> = aterm_spec::interp::reachable_states(&model)
        .iter()
        .map(key)
        .collect();
    assert_eq!(
        reached, reachable,
        "the driver reaches every reachable model state"
    );
    let reachable = reachable.len();
    let unique: BTreeSet<(StateKey, StateKey, &str)> = steps
        .iter()
        .map(|(prev, next, action)| (key(prev), key(next), *action))
        .collect();
    for (prev, next, action) in &unique {
        let prev: State = prev.iter().copied().collect();
        let next: State = next.iter().copied().collect();
        let (ok, why) = validate(&model, &prev, &next, action);
        assert!(
            ok,
            "rejected real {action}\nprev={prev:?}\nnext={next:?}\n{why}"
        );
    }
    eprintln!(
        "TextSelectionLifecycle Tier-1: {} calls driven, {} distinct transitions judged, \
         {reachable} reachable states",
        steps.len(),
        unique.len()
    );
}

/// NEGATIVE CONTROLS: each phase check the model's `Buggy` branch drops, as a
/// real transition would show it, is rejected — a drag that moves the far
/// anchor after the release, a release that completes nothing, a shift-click
/// during a drag, and a clear that spares a completed selection.
#[test]
fn a_dropped_phase_check_is_rejected() {
    let model = text_selection_lifecycle_model();
    let observed = |calls: &[&str]| -> (State, TextSelection) {
        let mut selection = TextSelection::new();
        let mut state = project(&selection, None);
        for &action in calls {
            let before = selection.clone();
            call(&mut selection, action);
            state = project(&selection, Some((&before, action)));
        }
        (state, selection)
    };
    let forged = |prev: &State, action: &str, changes: &[(&'static str, i64)]| {
        let mut next = prev.clone();
        next.insert("pre", prev["phase"]);
        next.insert(
            "last",
            1 + ALPHABET.iter().position(|&a| a == action).unwrap() as i64,
        );
        next.insert("moved", 0);
        for &(var, value) in changes {
            next.insert(var, value);
        }
        next
    };
    let (complete, _) = observed(&["Start", "Complete"]);
    let (none, _) = observed(&[]);
    let (dragging, _) = observed(&["Start"]);
    for (prev, action, changes) in [
        (&complete, "Update", &[("end", 1), ("moved", 1)][..]),
        (&none, "Complete", &[("phase", 2)][..]),
        (&dragging, "Extend", &[("end", 2), ("moved", 1)][..]),
        (&complete, "Clear", &[][..]),
    ] {
        let next = forged(prev, action, changes);
        let (ok, _) = validate(&model, prev, &next, action);
        assert!(
            !ok,
            "a forged {action} {changes:?} from {prev:?} must be rejected"
        );
    }
    // The real calls from those states are the committed ones.
    let (after, _) = observed(&["Start", "Complete", "Update"]);
    assert_eq!((after["phase"], after["end"], after["moved"]), (2, 0, 0));
    let (after, _) = observed(&["Complete"]);
    assert_eq!(after["phase"], 0);
    let (after, _) = observed(&["Start", "Extend"]);
    assert_eq!((after["phase"], after["end"], after["moved"]), (1, 0, 0));
    let (after, _) = observed(&["Start", "Complete", "Clear"]);
    assert_eq!(after["phase"], 0);
}
