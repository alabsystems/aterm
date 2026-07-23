// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the native Settings Diagnostics refresh deadline.
//!
//! The test exhausts the complete seven-bit input lattice of the shipping pure
//! scheduler decision. Its independently projected transition must be the one
//! `NativeDiagnosticsDeadline::Decide` successor. The Tier-0 test proves that
//! generated model with both the embedded checker and `ty`; this test binds that
//! theorem to the actual event-loop decision seam.

use aterm_gui::{NativeDiagnosticsDeadlineDecision, native_diagnostics_deadline_decision};
use aterm_spec::derive::{Model, native_diagnostics_deadline_model};
use aterm_spec::interp::{State, admits};

fn bit(value: bool) -> i64 {
    i64::from(value)
}

fn decision_code(decision: NativeDiagnosticsDeadlineDecision) -> i64 {
    match decision {
        NativeDiagnosticsDeadlineDecision::Disarm => 0,
        NativeDiagnosticsDeadlineDecision::Arm => 1,
        NativeDiagnosticsDeadlineDecision::Keep => 2,
        NativeDiagnosticsDeadlineDecision::Refresh => 3,
    }
}

#[derive(Clone, Copy, Debug)]
struct Inputs {
    route_visible: bool,
    has_os_window: bool,
    focused: bool,
    recorded: bool,
    overlay_open: bool,
    armed: bool,
    due: bool,
}

fn project_before(model: &Model, input: Inputs) -> State {
    let mut state = model.init_state();
    state.insert("route_visible", bit(input.route_visible));
    state.insert("has_os_window", bit(input.has_os_window));
    state.insert("focused", bit(input.focused));
    state.insert("recorded", bit(input.recorded));
    state.insert("overlay_open", bit(input.overlay_open));
    state.insert("armed", bit(input.armed));
    state.insert("due", bit(input.due));
    state
}

fn project_after(
    before: &State,
    decision: NativeDiagnosticsDeadlineDecision,
    input: Inputs,
) -> State {
    let mut state = before.clone();
    state.insert("observed_armed", bit(input.armed));
    state.insert("observed_due", bit(input.due));
    state.insert("decision", decision_code(decision));
    state.insert(
        "armed",
        bit(matches!(
            decision,
            NativeDiagnosticsDeadlineDecision::Arm | NativeDiagnosticsDeadlineDecision::Keep
        )),
    );
    state.insert(
        "refreshed",
        bit(decision == NativeDiagnosticsDeadlineDecision::Refresh),
    );
    state.insert("due", 0);
    state.insert("resolved", 1);
    state
}

fn shipping_decision(input: Inputs) -> NativeDiagnosticsDeadlineDecision {
    native_diagnostics_deadline_decision(
        input.route_visible,
        input.has_os_window,
        input.focused,
        input.recorded,
        input.overlay_open,
        input.armed,
        input.due,
    )
}

/// Historical mutant: the deadline gate observes the window/recording watcher
/// but forgets that Diagnostics itself must still be the visible route.
fn invisible_route_mutant(input: Inputs) -> NativeDiagnosticsDeadlineDecision {
    let eligible =
        !input.overlay_open && (input.recorded || (input.has_os_window && input.focused));
    if !eligible {
        NativeDiagnosticsDeadlineDecision::Disarm
    } else if !input.armed {
        NativeDiagnosticsDeadlineDecision::Arm
    } else if input.due {
        NativeDiagnosticsDeadlineDecision::Refresh
    } else {
        NativeDiagnosticsDeadlineDecision::Keep
    }
}

#[test]
fn shipping_diagnostics_deadline_decision_conforms_over_full_input_lattice() {
    let model = native_diagnostics_deadline_model();
    let mut decisions_seen = [false; 4];
    let mut cases = 0usize;

    for mask in 0u16..128 {
        let input = Inputs {
            route_visible: mask & (1 << 0) != 0,
            has_os_window: mask & (1 << 1) != 0,
            focused: mask & (1 << 2) != 0,
            recorded: mask & (1 << 3) != 0,
            overlay_open: mask & (1 << 4) != 0,
            armed: mask & (1 << 5) != 0,
            due: mask & (1 << 6) != 0,
        };
        let before = project_before(&model, input);
        let decision = shipping_decision(input);
        let after = project_after(&before, decision, input);
        decisions_seen[usize::try_from(decision_code(decision)).expect("decision 0..=3")] = true;

        assert_eq!(
            model.successors("Decide", &before).as_slice(),
            std::slice::from_ref(&after),
            "shipping decision diverged for {input:?}: {decision:?}",
        );
        assert_eq!(
            admits(&model, &before, &after),
            Some("Decide"),
            "shipping transition not admitted for {input:?}: {decision:?}",
        );
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &after),
                "{} failed for shipping input {input:?}: {after:?}",
                invariant.name,
            );
        }
        cases += 1;
    }

    assert_eq!(cases, 128);
    assert_eq!(
        decisions_seen, [true; 4],
        "the exhaustive lattice must exercise Disarm, Arm, Keep, and Refresh",
    );
}

#[test]
fn invisible_route_negative_control_is_rejected() {
    let model = native_diagnostics_deadline_model();
    let mut rejected = 0usize;

    for mask in 0u16..128 {
        let input = Inputs {
            route_visible: mask & (1 << 0) != 0,
            has_os_window: mask & (1 << 1) != 0,
            focused: mask & (1 << 2) != 0,
            recorded: mask & (1 << 3) != 0,
            overlay_open: mask & (1 << 4) != 0,
            armed: mask & (1 << 5) != 0,
            due: mask & (1 << 6) != 0,
        };
        let healthy = shipping_decision(input);
        let mutant = invisible_route_mutant(input);
        if healthy == mutant {
            continue;
        }

        let before = project_before(&model, input);
        let forged = project_after(&before, mutant, input);
        assert_ne!(
            model.successors("Decide", &before).as_slice(),
            std::slice::from_ref(&forged),
            "invisible-route mutant was unexpectedly admitted for {input:?}",
        );
        assert_eq!(admits(&model, &before, &forged), None);
        assert!(
            model
                .invariants
                .iter()
                .any(|invariant| !model.check_invariant(invariant.name, &forged)),
            "forged mutant state must violate a named invariant for {input:?}",
        );
        rejected += 1;
    }

    assert!(
        rejected >= 4,
        "negative control must diverge on multiple hidden-route watcher states; got {rejected}",
    );
}
