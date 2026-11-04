// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn role_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Warm" => {
            next.insert("warm", 1);
        }
        "LoseCustody" => {
            next.insert("available", 0);
        }
        "ChangeContext" => {
            next.insert("same", 0);
        }
        "Deny" => {
            next.insert("denied", 1);
        }
        "Observe" => {
            use crate::client::{AlabRoleContext, AlabRoleObservation};
            let mut role = ServeRole::default();
            let apply = |role: &mut ServeRole, observation| {
                role.apply(
                    observation,
                    (Some(crate::alab_advertised().into()), None),
                    &mut Vec::new(),
                    Some([88; 32]),
                )
                .0
            };
            if state["warm"] == 1 {
                assert!(
                    apply(
                        &mut role,
                        AlabRoleObservation::Allowed {
                            context: AlabRoleContext::for_role_test(1),
                            own_label: Some("m7".into()),
                        }
                    )
                    .is_some()
                );
            }
            let context = AlabRoleContext::for_role_test(if state["same"] == 1 { 1 } else { 2 });
            let observation = if state["denied"] == 1 {
                AlabRoleObservation::Denied
            } else if state["available"] == 1 {
                AlabRoleObservation::Allowed {
                    context,
                    own_label: Some("m7".into()),
                }
            } else {
                AlabRoleObservation::Unavailable(Some(context))
            };
            next.insert("phase", 1);
            next.insert(
                "visible",
                i64::from(apply(&mut role, observation).is_some()),
            );
        }
        _ => panic!("unknown role transition {action}"),
    }
    next
}

#[test]
fn role_retention_model_conforms_to_native_role_decisions() {
    let model = aterm_models_swarm::role_retention_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&role_transition(&state, action.name))
                );
            }
        }
    }
    let mut cold = model.init_state();
    cold.insert("available", 0);
    let mut unauthorized = role_transition(&cold, "Observe");
    unauthorized.insert("visible", 1);
    assert!(!model.check_invariant("NoColdGrant", &unauthorized));
    assert!(!model.successors("Observe", &cold).contains(&unauthorized));
}
