// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for native screenshot present ordering and pixel-source
//! provenance.
//!
//! The shipping pure decision is exhausted over every reachable one-based attempt
//! and both present outcomes. The complete bounded production loop is also driven
//! over its outcome lattice and projected action-by-action onto the derived model.
//! For full-window capture, OS pixels own chrome only; the client source is the
//! exact serial-bound successful PRESENT destination (swapchain/softbuffer), with
//! physical size and client-origin validation before stitch. A semantic offscreen
//! rerender is not equivalent authority.

use aterm_gui::{
    CaptureAfterPresentDecision, NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, NativeCaptureSourceDecision,
    capture_after_present_decision, native_capture_source_decision, run_capture_present_barrier,
};
use aterm_spec::derive::{Model, capture_after_present_model, native_capture_source_model};
use aterm_spec::interp::{State, admits};

fn bit(value: bool) -> i64 {
    i64::from(value)
}

fn decision_code(decision: CaptureAfterPresentDecision) -> i64 {
    match decision {
        CaptureAfterPresentDecision::Capture => 1,
        CaptureAfterPresentDecision::Retry => 2,
        CaptureAfterPresentDecision::FailClosed => 3,
    }
}

#[derive(Clone, Copy, Debug)]
struct Inputs {
    present_succeeded: bool,
    attempts: u8,
}

fn project_before(model: &Model, input: Inputs) -> State {
    let mut state = model.init_state();
    state.insert("staged", 1);
    state.insert("present_succeeded", bit(input.present_succeeded));
    state.insert("attempts", i64::from(input.attempts));
    state
}

fn project_after(before: &State, input: Inputs, decision: CaptureAfterPresentDecision) -> State {
    let mut state = before.clone();
    let capture = decision == CaptureAfterPresentDecision::Capture;
    let stale_capture = capture && !input.present_succeeded;
    state.insert("decision", decision_code(decision));
    state.insert("captured", bit(capture));
    state.insert(
        "failed",
        bit(decision == CaptureAfterPresentDecision::FailClosed),
    );
    state.insert("stale_capture", bit(stale_capture));
    state.insert("staged", bit(!capture || !input.present_succeeded));
    state
}

fn shipping_decision(input: Inputs) -> CaptureAfterPresentDecision {
    capture_after_present_decision(
        input.present_succeeded,
        input.attempts,
        NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT,
    )
}

/// Historical mutant: a dropped present falls through to compositor capture,
/// returning the pixels from before the native control mutation.
fn stale_capture_mutant(_input: Inputs) -> CaptureAfterPresentDecision {
    CaptureAfterPresentDecision::Capture
}

fn model_step(model: &Model, action: &str, before: &State) -> State {
    let successors = model.successors(action, before);
    assert_eq!(
        successors.len(),
        1,
        "{action} must have one successor from {before:?}"
    );
    let after = successors[0].clone();
    assert_eq!(admits(model, before, &after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &after),
            "{} failed after {action}: {after:?}",
            invariant.name,
        );
    }
    after
}

fn replay_present_trace(model: &Model, outcomes: &[bool]) -> (bool, State, Vec<i64>) {
    let mut state = model_step(model, "Mutate", &model.init_state());
    let mut decisions = Vec::with_capacity(outcomes.len());

    for (index, &present_succeeded) in outcomes.iter().enumerate() {
        assert_eq!(
            state["attempts"],
            i64::try_from(index + 1).expect("bounded attempt index"),
            "model attempt counter must match the production closure call"
        );
        if present_succeeded {
            state = model_step(model, "MarkPresentSucceeded", &state);
        }
        state = model_step(model, "Decide", &state);
        decisions.push(state["decision"]);
        match state["decision"] {
            1 => return (true, state, decisions),
            2 => state = model_step(model, "Retry", &state),
            3 => return (false, state, decisions),
            other => panic!("unexpected modeled capture decision {other}"),
        }
    }

    panic!("production outcomes ended without a terminal capture decision")
}

#[test]
fn shipping_capture_decision_conforms_over_reachable_attempt_lattice() {
    let model = capture_after_present_model();
    let mut decisions_seen = [false; 3];
    let mut cases = 0usize;

    for attempts in 1..=NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT {
        for present_succeeded in [false, true] {
            let input = Inputs {
                present_succeeded,
                attempts,
            };
            let before = project_before(&model, input);
            let decision = shipping_decision(input);
            let after = project_after(&before, input, decision);
            decisions_seen
                [usize::try_from(decision_code(decision) - 1).expect("decision index 0..=2")] =
                true;

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
    }

    assert_eq!(cases, 6);
    assert_eq!(
        decisions_seen, [true; 3],
        "the lattice must exercise Capture, Retry, and FailClosed",
    );
}

#[test]
fn complete_shipping_barrier_trace_conforms_and_calls_present_at_most_three_times() {
    let model = capture_after_present_model();
    assert_eq!(NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, 3);
    let mut terminal_paths_seen = [false; 4];

    // Exhaust every possible three-call result sequence. Early success stops the
    // real loop, so several masks intentionally share a shorter observed prefix.
    for mask in 0u8..8 {
        let mut calls = 0usize;
        let mut observed = Vec::new();
        let captured = run_capture_present_barrier(NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, || {
            assert!(
                calls < usize::from(NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT),
                "production barrier attempted a fourth present"
            );
            let result = mask & (1u8 << calls) != 0;
            calls += 1;
            observed.push(result);
            result
        });

        let expected_calls = (0..usize::from(NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT))
            .find(|attempt| mask & (1u8 << attempt) != 0)
            .map_or(
                usize::from(NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT),
                |attempt| attempt + 1,
            );
        assert_eq!(
            calls, expected_calls,
            "wrong call bound for mask {mask:03b}"
        );

        let (modeled_capture, final_state, decisions) = replay_present_trace(&model, &observed);
        assert_eq!(
            captured, modeled_capture,
            "outcome drift for mask {mask:03b}"
        );
        assert_eq!(decisions.len(), calls);
        assert_eq!(
            final_state["attempts"],
            i64::try_from(calls).expect("three attempts fit i64")
        );
        assert!(
            model.successors("Decide", &final_state).is_empty(),
            "terminal production trace must not admit a fourth decision"
        );

        let path = match (captured, calls) {
            (true, 1) => 0,
            (true, 2) => 1,
            (true, 3) => 2,
            (false, 3) => 3,
            other => panic!("unexpected terminal path {other:?}"),
        };
        terminal_paths_seen[path] = true;
    }

    assert_eq!(
        terminal_paths_seen, [true; 4],
        "capture on attempts 1/2/3 and bounded failure must all be exercised"
    );
}

#[test]
fn stale_capture_negative_control_is_rejected() {
    let model = capture_after_present_model();
    let mut rejected = 0usize;

    for attempts in 1..=NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT {
        let input = Inputs {
            present_succeeded: false,
            attempts,
        };
        let before = project_before(&model, input);
        let mutant = stale_capture_mutant(input);
        let forged = project_after(&before, input, mutant);

        assert_ne!(
            model.successors("Decide", &before).as_slice(),
            std::slice::from_ref(&forged),
            "stale-capture mutant was unexpectedly admitted for {input:?}",
        );
        assert_eq!(admits(&model, &before, &forged), None);
        assert!(
            model
                .invariants
                .iter()
                .any(|invariant| !model.check_invariant(invariant.name, &forged)),
            "forged stale capture must violate a named invariant for {input:?}",
        );
        rejected += 1;
    }

    assert_eq!(rejected, 3);
}

fn source_decision_code(decision: NativeCaptureSourceDecision) -> i64 {
    match decision {
        NativeCaptureSourceDecision::StitchRenderer => 1,
        NativeCaptureSourceDecision::FailClosed => 2,
    }
}

#[derive(Clone, Copy, Debug)]
struct SourceInputs {
    /// Exact destination is bound to the successful presentation serial.
    frame_presented: bool,
    /// Conjunction of physical dimensions and client-origin validation.
    geometry_valid: bool,
    /// The untrusted OS client photograph happens to look current.
    os_client_current: bool,
}

fn source_before(model: &Model, input: SourceInputs) -> State {
    let mut state = model.init_state();
    state.insert("frame_presented", bit(input.frame_presented));
    state.insert("geometry_valid", bit(input.geometry_valid));
    state.insert("os_client_current", bit(input.os_client_current));
    state
}

fn source_after(
    before: &State,
    input: SourceInputs,
    decision: NativeCaptureSourceDecision,
) -> State {
    let mut state = before.clone();
    let stitch = decision == NativeCaptureSourceDecision::StitchRenderer;
    // Keep the model's historical field name for trace compatibility. This is
    // destination authority, not a fresh semantic renderer invocation.
    let present_destination_bound = stitch && input.frame_presented && input.geometry_valid;
    state.insert("decision", source_decision_code(decision));
    state.insert("renderer_bound", bit(present_destination_bound));
    state.insert("captured", bit(stitch));
    state.insert("failed", bit(!stitch));
    state.insert("stale_capture", bit(stitch && !present_destination_bound));
    state
}

#[test]
fn shipping_native_capture_source_conforms_over_provenance_lattice() {
    let model = native_capture_source_model();
    let mut cases = 0usize;
    for frame_presented in [false, true] {
        for geometry_valid in [false, true] {
            for os_client_current in [false, true] {
                let input = SourceInputs {
                    frame_presented,
                    geometry_valid,
                    os_client_current,
                };
                let before = source_before(&model, input);
                let decision = native_capture_source_decision(frame_presented, geometry_valid);
                let after = source_after(&before, input, decision);
                assert_eq!(
                    model.successors("Decide", &before).as_slice(),
                    std::slice::from_ref(&after),
                    "serial-bound destination decision diverged for {input:?}: {decision:?}",
                );
                assert_eq!(admits(&model, &before, &after), Some("Decide"));
                for invariant in &model.invariants {
                    assert!(
                        model.check_invariant(invariant.name, &after),
                        "{} failed for shipping source input {input:?}: {after:?}",
                        invariant.name,
                    );
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 8);
}

#[test]
fn compositor_only_capture_negative_control_is_rejected() {
    let model = native_capture_source_model();
    // Strong negative control: both a current-looking OS client region and
    // validated geometry are present, but no successful present serial binds
    // those pixels to the actual swapchain/softbuffer destination. A semantic
    // offscreen rerender has the same missing-authority shape.
    let input = SourceInputs {
        frame_presented: false,
        geometry_valid: true,
        os_client_current: true,
    };
    let before = source_before(&model, input);
    let forged = source_after(&before, input, NativeCaptureSourceDecision::StitchRenderer);

    assert_eq!(admits(&model, &before, &forged), None);
    assert!(
        model
            .invariants
            .iter()
            .any(|invariant| !model.check_invariant(invariant.name, &forged)),
        "current-looking or offscreen client pixels cannot replace the exact present destination"
    );
}
