// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::{derive::harness_upgrade_drain_bound_model, interp, verify};

/// The drain never ends the agent on an answer a person held past the bound, and
/// the bug it was written for — waiting on the person with no bound — is caught:
/// the answer outlives the hold, and the end comes the moment the person lets go.
#[test]
fn the_drain_never_ends_the_agent_on_an_answer_a_person_held_past_its_bound() {
    let model = harness_upgrade_drain_bound_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade drain bound");

    // Held to the bound, the answer is voided and can never end the agent.
    let mut held = model.init_state();
    assert!(model.fire("PersonHolds", &mut held));
    assert!(model.fire("Wait", &mut held));
    assert!(model.fire("Wait", &mut held));
    assert!(!model.action_enabled("Wait", &held), "the bound is a void");
    assert!(model.fire("Void", &mut held));
    assert!(model.fire("PersonLets", &mut held));
    assert!(!model.action_enabled("Terminate", &held));

    // The agent's own work is waited for past the bound, and never voided.
    let mut working = model.init_state();
    assert!(model.fire("AgentWorks", &mut working));
    for _ in 0..4 {
        assert!(model.fire("Wait", &mut working));
        assert!(!model.action_enabled("Void", &working));
    }
    assert!(model.fire("AgentRests", &mut working));
    assert!(model.fire("Terminate", &mut working));
    assert!(model.check_invariant("NoEndOnAHeldAnswer", &working));

    let buggy = interp::with_buggy(&model, 1);
    let mut stale = buggy.init_state();
    assert!(buggy.fire("PersonHolds", &mut stale));
    for _ in 0..3 {
        assert!(buggy.fire("Wait", &mut stale));
    }
    assert!(!buggy.action_enabled("Void", &stale));
    assert!(buggy.fire("PersonLets", &mut stale));
    assert!(buggy.fire("Terminate", &mut stale));
    assert!(!buggy.check_invariant("NoEndOnAHeldAnswer", &stale));
}
