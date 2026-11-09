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
        "FailDenialCheck" => {
            next.insert("unjudged", 1);
            next.insert("available", 0);
        }
        "Outlast" => {
            next.insert("unjudged", 2);
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
            let observation = || {
                let context = context.clone();
                if state["denied"] == 1 {
                    AlabRoleObservation::Denied
                } else if state["unjudged"] > 0 {
                    AlabRoleObservation::Unjudged(context)
                } else if state["available"] == 1 {
                    AlabRoleObservation::Allowed {
                        context,
                        own_label: Some("m7".into()),
                    }
                } else {
                    AlabRoleObservation::Unavailable(Some(context))
                }
            };
            // A failed check holds for the bound's last poll and not the one after. The bound is
            // that check's alone: no other evidence runs out, however many polls it lasts.
            let polls = if state["unjudged"] == 1 {
                ROLE_UNJUDGED_POLLS
            } else {
                ROLE_UNJUDGED_POLLS + 1
            };
            let mut visible = false;
            for _ in 0..polls {
                visible = apply(&mut role, observation()).is_some();
            }
            next.insert("phase", 1);
            next.insert("visible", i64::from(visible));
        }
        _ => panic!("unknown role transition {action}"),
    }
    next
}

#[test]
fn role_retention_model_conforms_to_native_role_decisions() {
    let model = aterm_models_swarm::role_retention_model();
    // The global aterm-models non_vacuity test owns the complete fault sweep.
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
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

/// Only polls in a row count: one that judges this Mac starts the bound again, and a role the bound
/// ended stays ended until a poll grants it.
///
/// NEGATIVE CONTROLS (2026-11-05): with `unjudged` reset by no observation, the second run ends
/// the role at its first poll (`left: None, right: Some("m7")`); with an `Unavailable` that does
/// not reset it, the same. With the bound gone, `the bound ends it` fails.
#[test]
fn a_poll_that_judges_this_mac_starts_the_bound_again() {
    use crate::client::{AlabRoleContext, AlabRoleObservation};
    let context = || AlabRoleContext::for_role_test(1);
    let mut role = ServeRole::default();
    let mut apply = |observation| {
        role.apply(
            observation,
            (Some(crate::alab_advertised().into()), None),
            &mut Vec::new(),
            Some([88; 32]),
        )
        .0
    };
    let label = Some("m7".to_owned());
    let allowed = || AlabRoleObservation::Allowed {
        context: context(),
        own_label: Some("m7".into()),
    };
    assert_eq!(apply(allowed()), label);
    for judged in [AlabRoleObservation::Unavailable(Some(context())), allowed()] {
        for _ in 0..ROLE_UNJUDGED_POLLS {
            assert_eq!(apply(AlabRoleObservation::Unjudged(context())), label);
        }
        assert_eq!(apply(judged), label);
    }
    for _ in 0..ROLE_UNJUDGED_POLLS {
        assert_eq!(apply(AlabRoleObservation::Unjudged(context())), label);
    }
    assert_eq!(
        apply(AlabRoleObservation::Unjudged(context())),
        None,
        "the bound ends it"
    );
    assert_eq!(
        apply(AlabRoleObservation::Unavailable(Some(context()))),
        None,
        "an ended role does not come back on evidence that grants nothing"
    );
    assert_eq!(apply(allowed()), label, "a grant gives it back");
}
