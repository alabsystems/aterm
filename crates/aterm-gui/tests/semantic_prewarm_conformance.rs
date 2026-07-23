// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for semantic-font prewarm generation acceptance.
//!
//! Reload may leave an older worker running. This exhausts the bounded current /
//! completed-generation lattice and binds the exact shipping install guard to the
//! derived worker model; a stale-always-installs mutant is rejected independently.

use aterm_gui::{
    SemanticPrewarmResultDecision, semantic_prewarm_cache_active_before_request,
    semantic_prewarm_replacement_carries_base, semantic_prewarm_result_decision,
    semantic_prewarm_result_is_current,
};
use aterm_spec::derive::{
    Model, semantic_prewarm_generation_model, semantic_prewarm_handshake_model,
    semantic_prewarm_request_swap_model,
};
use aterm_spec::interp::{State, admits};

fn project_before(model: &Model, current: i64, result: i64) -> State {
    let mut state = model.init_state();
    state.insert("current", current);
    state.insert("result", result);
    state
}

fn project_after(before: &State, current: i64, result: i64, accept: bool) -> State {
    let mut state = before.clone();
    state.insert("observed_current", current);
    state.insert("observed_result", result);
    state.insert("decision", i64::from(accept));
    state.insert("ready", i64::from(accept));
    state.insert("installed", if accept { result } else { 0 });
    state.insert("result", 0);
    state.insert("resolved", 1);
    state
}

#[test]
fn shipping_semantic_prewarm_guard_conforms_over_generation_lattice() {
    let model = semantic_prewarm_generation_model();
    let mut accepted = 0;
    let mut ignored = 0;

    for current in 1_i64..=3 {
        for result in 1_i64..=3 {
            let before = project_before(&model, current, result);
            let accept = semantic_prewarm_result_is_current(
                u64::try_from(current).unwrap(),
                u64::try_from(result).unwrap(),
            );
            let after = project_after(&before, current, result, accept);
            assert_eq!(
                model.successors("Decide", &before).as_slice(),
                std::slice::from_ref(&after),
                "shipping generation guard diverged for current={current} result={result}",
            );
            assert_eq!(admits(&model, &before, &after), Some("Decide"));
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &after),
                    "{} failed for current={current} result={result}: {after:?}",
                    invariant.name,
                );
            }
            if accept {
                accepted += 1;
            } else {
                ignored += 1;
            }
        }
    }

    assert_eq!(accepted, 3);
    assert_eq!(ignored, 6);
}

#[test]
fn stale_always_installs_mutant_is_rejected() {
    let model = semantic_prewarm_generation_model();
    let mut rejected = 0;

    for current in 1_i64..=3 {
        for result in 1_i64..=3 {
            if current == result {
                continue;
            }
            let before = project_before(&model, current, result);
            let forged = project_after(&before, current, result, true);
            assert_ne!(
                model.successors("Decide", &before).as_slice(),
                std::slice::from_ref(&forged),
            );
            assert_eq!(admits(&model, &before, &forged), None);
            assert!(
                model
                    .invariants
                    .iter()
                    .any(|invariant| !model.check_invariant(invariant.name, &forged))
            );
            rejected += 1;
        }
    }

    assert_eq!(rejected, 6);
}

fn replacement_before(model: &Model, new_base: bool, replaced_base: bool) -> State {
    let mut state = model.init_state();
    state.insert("new_base", i64::from(new_base));
    state.insert("replaced_base", i64::from(replaced_base));
    state
}

fn replacement_after(before: &State, carries_base: bool) -> State {
    let mut state = before.clone();
    state.insert("replacement_base", i64::from(carries_base));
    state.insert("replacement_resolved", 1);
    state
}

#[test]
fn shipping_replacement_policy_carries_the_displaced_base_before_start() {
    let model = semantic_prewarm_handshake_model();

    for new_base in [false, true] {
        for replaced_base in [false, true] {
            let before = replacement_before(&model, new_base, replaced_base);
            let carries_base = semantic_prewarm_replacement_carries_base(new_base, replaced_base);
            let after = replacement_after(&before, carries_base);
            assert_eq!(
                model.successors("ResolveReplacement", &before).as_slice(),
                std::slice::from_ref(&after),
                "shipping base-carry policy diverged for new={new_base} replaced={replaced_base}",
            );
            assert_eq!(admits(&model, &before, &after), Some("ResolveReplacement"));
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &after),
                    "{} failed for new={new_base} replaced={replaced_base}: {after:?}",
                    invariant.name,
                );
            }
        }
    }
}

fn result_decision_code(decision: SemanticPrewarmResultDecision) -> i64 {
    match decision {
        SemanticPrewarmResultDecision::IgnoreStaleGeneration => 1,
        SemanticPrewarmResultDecision::InstallCurrent => 2,
        SemanticPrewarmResultDecision::FailClosedCurrent => 3,
        SemanticPrewarmResultDecision::CacheSuperseded => 4,
        SemanticPrewarmResultDecision::IgnoreFailedSuperseded => 5,
    }
}

#[derive(Clone, Copy)]
struct ResultInputs {
    generation_matches: bool,
    request_matches: bool,
    candidate_matches: bool,
    renderer_ready: bool,
    active_before: bool,
    active_before_latest: bool,
}

fn result_before(model: &Model, input: ResultInputs) -> State {
    let mut state = model.init_state();
    state.insert("generation_matches", i64::from(input.generation_matches));
    state.insert("request_matches", i64::from(input.request_matches));
    state.insert("candidate_matches", i64::from(input.candidate_matches));
    state.insert("renderer_ready", i64::from(input.renderer_ready));
    state.insert("active_before", i64::from(input.active_before));
    state.insert(
        "active_before_latest",
        i64::from(input.active_before_latest),
    );
    state.insert("active_after", i64::from(input.active_before));
    state.insert("active_after_latest", i64::from(input.active_before_latest));
    state
}

fn shipping_result_decision(input: ResultInputs) -> SemanticPrewarmResultDecision {
    semantic_prewarm_result_decision(
        2,
        2,
        if input.generation_matches { 2 } else { 1 },
        if input.request_matches { 2 } else { 1 },
        input.candidate_matches,
        input.renderer_ready,
    )
}

fn result_after(
    before: &State,
    input: ResultInputs,
    decision: SemanticPrewarmResultDecision,
) -> State {
    let mut state = before.clone();
    let installs = decision == SemanticPrewarmResultDecision::InstallCurrent;
    let fails_closed = decision == SemanticPrewarmResultDecision::FailClosedCurrent;
    state.insert("decision", result_decision_code(decision));
    state.insert("installed", i64::from(installs));
    state.insert("failed_closed", i64::from(fails_closed));
    state.insert(
        "cached",
        i64::from(decision == SemanticPrewarmResultDecision::CacheSuperseded),
    );
    state.insert(
        "active_after",
        if installs {
            1
        } else if fails_closed {
            0
        } else {
            i64::from(input.active_before)
        },
    );
    state.insert(
        "active_after_latest",
        if installs {
            1
        } else if fails_closed {
            0
        } else {
            i64::from(input.active_before_latest)
        },
    );
    state.insert("result_resolved", 1);
    state
}

#[test]
fn shipping_result_policy_conforms_over_identity_and_failure_lattice() {
    let model = semantic_prewarm_handshake_model();
    let mut dispositions = [0_usize; 5];

    for generation_matches in [false, true] {
        for request_matches in [false, true] {
            for candidate_matches in [false, true] {
                for renderer_ready in [false, true] {
                    for (active_before, active_before_latest) in
                        [(false, false), (true, false), (true, true)]
                    {
                        let input = ResultInputs {
                            generation_matches,
                            request_matches,
                            candidate_matches,
                            renderer_ready,
                            active_before,
                            active_before_latest,
                        };
                        let before = result_before(&model, input);
                        let decision = shipping_result_decision(input);
                        let after = result_after(&before, input, decision);
                        assert_eq!(
                            model.successors("DecideResult", &before).as_slice(),
                            std::slice::from_ref(&after),
                            "shipping result policy diverged for generation={generation_matches} request={request_matches} candidate={candidate_matches} ready={renderer_ready}",
                        );
                        assert_eq!(admits(&model, &before, &after), Some("DecideResult"));
                        for invariant in &model.invariants {
                            assert!(
                                model.check_invariant(invariant.name, &after),
                                "{} failed for generation={generation_matches} request={request_matches} candidate={candidate_matches} ready={renderer_ready}: {after:?}",
                                invariant.name,
                            );
                        }
                        dispositions
                            [usize::try_from(result_decision_code(decision) - 1).unwrap()] += 1;
                    }
                }
            }
        }
    }

    assert!(dispositions.into_iter().all(|count| count > 0));
}

#[test]
fn dropped_base_mixed_candidate_and_fail_open_mutants_are_rejected() {
    let model = semantic_prewarm_handshake_model();

    // Historical replacement bug: a base-less new job drops the displaced
    // not-yet-started job's unique committed base.
    let before = replacement_before(&model, false, true);
    let dropped = replacement_after(&before, false);
    assert_eq!(admits(&model, &before, &dropped), None);
    assert!(!model.check_invariant("ReplacementCarriesBase", &dropped));

    // A ready renderer with the current request number but the wrong candidate
    // identity is cacheable, never installable.
    let mixed_input = ResultInputs {
        generation_matches: true,
        request_matches: true,
        candidate_matches: false,
        renderer_ready: true,
        active_before: true,
        active_before_latest: true,
    };
    let mixed_before = result_before(&model, mixed_input);
    assert_eq!(
        shipping_result_decision(mixed_input),
        SemanticPrewarmResultDecision::CacheSuperseded
    );
    let mixed_install = result_after(
        &mixed_before,
        mixed_input,
        SemanticPrewarmResultDecision::InstallCurrent,
    );
    assert_eq!(admits(&model, &mixed_before, &mixed_install), None);
    assert!(!model.check_invariant("InstallOnlyLatestReady", &mixed_install));

    // Exact-current construction failure must clear an older active renderer.
    let failed_input = ResultInputs {
        generation_matches: true,
        request_matches: true,
        candidate_matches: true,
        renderer_ready: false,
        active_before: true,
        active_before_latest: false,
    };
    let failed_before = result_before(&model, failed_input);
    let mut fail_open = result_after(
        &failed_before,
        failed_input,
        SemanticPrewarmResultDecision::FailClosedCurrent,
    );
    fail_open.insert("active_after", 1);
    assert_eq!(admits(&model, &failed_before, &fail_open), None);
    assert!(!model.check_invariant("CurrentFailureFailsClosed", &fail_open));
}

#[derive(Clone, Copy, Debug)]
struct RequestInputs {
    active_present: bool,
    active_ready: bool,
    candidate_matches: bool,
}

fn request_before(model: &Model, input: RequestInputs) -> State {
    let mut state = model.init_state();
    state.insert("active_present", i64::from(input.active_present));
    state.insert("active_ready", i64::from(input.active_ready));
    state.insert("candidate_matches", i64::from(input.candidate_matches));
    state
}

fn request_after(before: &State, input: RequestInputs, should_cache: bool) -> State {
    let mut state = before.clone();
    state.insert("should_cache", i64::from(should_cache));
    state.insert(
        "active_after",
        i64::from(input.active_present && !should_cache),
    );
    state.insert("resolved", 1);
    state
}

#[test]
fn shipping_request_swap_conforms_and_rejects_mismatched_active_paint() {
    let model = semantic_prewarm_request_swap_model();
    let inputs = [
        RequestInputs {
            active_present: false,
            active_ready: false,
            candidate_matches: false,
        },
        RequestInputs {
            active_present: true,
            active_ready: false,
            candidate_matches: false,
        },
        RequestInputs {
            active_present: true,
            active_ready: true,
            candidate_matches: false,
        },
        RequestInputs {
            active_present: true,
            active_ready: true,
            candidate_matches: true,
        },
    ];

    for input in inputs {
        let before = request_before(&model, input);
        let should_cache = semantic_prewarm_cache_active_before_request(
            input.active_present,
            input.active_ready,
            input.candidate_matches,
        );
        let after = request_after(&before, input, should_cache);
        assert_eq!(
            model.successors("Decide", &before).as_slice(),
            std::slice::from_ref(&after),
            "shipping request swap diverged for {input:?}",
        );
        assert_eq!(admits(&model, &before, &after), Some("Decide"));
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &after),
                "{} failed for {input:?}: {after:?}",
                invariant.name,
            );
        }
    }

    let mixed = inputs[2];
    let before = request_before(&model, mixed);
    let retained = request_after(&before, mixed, false);
    assert_eq!(admits(&model, &before, &retained), None);
    assert!(!model.check_invariant("MismatchedReadyMovesToCache", &retained));
    assert!(!model.check_invariant("RetainedPaintIsExactOrHostSeed", &retained));
}
