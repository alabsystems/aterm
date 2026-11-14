// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::interp;

#[test]
fn projection_closes_over_guards_updates_and_preserves_each_step() {
    let model = crate::ty_model! {
        ProjectionSteps {
            var bound = 0;
            var source = 0;
            var gate = 0;
            var noise = 0;
            action Source when (3 > source) { source = source + 1; }
            action Open { gate = 1; }
            action Copy when (gate == 1 && source <= 2) { bound = source; source = bound; }
            action Noise when (3 > noise) { noise = noise + 1; }
            bound Fits: bound <= 2;
        }
    };
    let projected = project(&model, &model.invariants[0]);
    assert_eq!(
        projected.vars.iter().map(|v| v.name).collect::<Vec<_>>(),
        ["bound", "source", "gate"]
    );
    let image = |state: &interp::State| {
        state
            .iter()
            .filter(|(name, _)| **name != "noise")
            .map(|(name, value)| (*name, *value))
            .collect::<interp::State>()
    };
    let actual: BTreeSet<_> = interp::reachable_states(&model).iter().map(image).collect();
    let abstracted: BTreeSet<_> = interp::reachable_states(&projected).into_iter().collect();
    assert_eq!(actual, abstracted);
    for state in interp::reachable_states(&model) {
        for action in &model.actions {
            let mut after = state.clone();
            if model.fire(action.name, &mut after) {
                let mut view = image(&state);
                if action.name != "Noise" {
                    assert!(projected.fire(action.name, &mut view));
                }
                assert_eq!(view, image(&after));
            }
        }
    }
    let mut unsound = projected.clone();
    unsound.vars.retain(|v| v.name != "gate");
    unsound.actions.retain(|a| a.name != "Open");
    unsound
        .actions
        .iter_mut()
        .find(|a| a.name == "Copy")
        .unwrap()
        .guard = None;
    let before_open = model.init_state();
    assert!(!model.action_enabled("Copy", &before_open));
    assert!(
        unsound.action_enabled("Copy", &unsound.init_state()),
        "dropping a guard dependency admits a step the concrete model refuses"
    );
    assert!(interp::bmc(&model).is_ok());
    assert_eq!(
        interp::bmc(&unsound).unwrap_err().1,
        "Fits",
        "the missing guard must produce a false bound counterexample"
    );
}

#[test]
fn quantified_indices_are_local_but_their_limits_are_dependencies() {
    let expr = crate::derive::forall(
        "i",
        crate::derive::int(0),
        crate::derive::var("limit"),
        crate::derive::le(crate::derive::var("i"), crate::derive::var("value")),
    );
    let mut found = BTreeSet::new();
    reads(&expr, &mut Vec::new(), &mut found);
    assert_eq!(found, BTreeSet::from(["limit", "value"]));
}

#[test]
fn an_empty_removed_range_cannot_manufacture_a_successor() {
    let mut model = crate::ty_model! {
        EmptyChoice {
            var value = 0;
            var choice = 0;
            var limit = 0;
            action Impossible { value = 3; choice = 0; }
            bound Fits: value <= 2;
        }
    };
    model.actions[0].updates[1].expr =
        crate::derive::in_range(crate::derive::int(1), crate::derive::var("limit"));
    let projected = project(&model, &model.invariants[0]);
    assert!(projected.vars.iter().any(|v| v.name == "choice"));
    assert!(projected.vars.iter().any(|v| v.name == "limit"));
    assert_eq!(interp::bmc(&model), interp::bmc(&projected));
    assert!(holds(&model, &model.invariants[0]));
    let mut unsound = projected.clone();
    unsound.vars.retain(|v| v.name == "value");
    unsound.actions[0].updates.retain(|u| u.var == "value");
    assert_eq!(interp::bmc(&unsound).unwrap_err().1, "Fits");
}

#[test]
fn an_unchanged_law_has_a_valid_single_state_projection() {
    let model = crate::ty_model! {
        UnchangedProjection {
            var fixed = 0;
            var noise = 0;
            action Drift { noise = noise + 1; }
            bound Fits: fixed <= 2;
        }
    };
    let projected = project(&model, &model.invariants[0]);
    assert!(projected.actions.is_empty());
    assert_eq!(crate::interp::bmc(&projected), Ok(1));
    assert!(holds(&model, &model.invariants[0]));
}

#[test]
#[should_panic(expected = "bound projection has no scalar state variable")]
fn a_constant_predicate_cannot_pose_as_a_range_bound() {
    let mut model = crate::test_models::ring_model();
    model.invariants[0].expr = Expr::Bool(true);
    let _ = project(&model, &model.invariants[0]);
}

#[test]
fn a_counterexample_from_a_weakened_guard_is_not_credited() {
    let model = crate::ty_model! {
        DisabledWrite {
            var value = 0;
            var enabled = 0;
            action Write when (enabled == 1) { value = 3; }
            bound Fits: value <= 2;
        }
    };
    let abstracted = project_with(&model, &model.invariants[0], false);
    assert!(interp::bmc(&abstracted).is_err());
    assert!(interp::bmc(&project(&model, &model.invariants[0])).is_ok());
    assert!(
        holds(&model, &model.invariants[0]),
        "only the exact projection may supply a counterexample"
    );
}

#[test]
#[should_panic(expected = "ill-typed scalar bound projection")]
fn dropping_an_invalid_update_cannot_manufacture_a_trace_lift() {
    let mut model = crate::ty_model! {
        BadUpdate {
            var value = 0;
            var other = 0;
            action Bad { value = 1; other = 0; }
            bound Fits: value <= 2;
        }
    };
    model.actions[0].updates[1].expr = Expr::Bool(true);
    let _ = project(&model, &model.invariants[0]);
}

#[test]
#[should_panic(expected = "duplicate update to `other` in bound projection")]
fn contradictory_discarded_assignments_cannot_manufacture_a_trace_lift() {
    let model = crate::ty_model! {
        ContradictoryUpdate {
            var value = 0;
            var other = 0;
            action Impossible { value = 3; other = 0; other = 1; }
            bound Fits: value <= 2;
        }
    };
    let _ = holds(&model, &model.invariants[0]);
}

#[test]
fn duplicate_or_colliding_declarations_refuse_before_projection() {
    let source = crate::ty_model! {
        AmbiguousSource {
            const Limit = 2;
            var value = 0;
            var other = 0;
            action Write { value = 1; }
            bound Fits: value <= Limit;
        }
    };
    assert!(holds(&source, &source.invariants[0]));
    for defect in 0..7 {
        let mut model = source.clone();
        match defect {
            0 => model.vars.push(model.vars[1].clone()),
            1 => model.consts.push(("Limit", 3)),
            2 => model.consts.push(("other", 3)),
            3 => model.actions.push(model.actions[0].clone()),
            4 => model.invariants.push(model.invariants[0].clone()),
            5 => model.actions[0].name = "other",
            6 => model.actions[0].name = "Next",
            _ => unreachable!(),
        }
        let error = crate::check::attempt(|| holds(&model, &model.invariants[0]))
            .expect_err("an ambiguous declaration cannot disappear in a projection");
        assert!(
            error.contains("duplicate or reserved TLA+ declaration"),
            "{error}"
        );
    }
}

#[test]
fn a_real_projected_violation_survives_the_full_walk_limit() {
    let mut model = crate::ty_model! {
        ProjectedViolation {
            const Buggy = 0;
            var value = 0;
            var noise = 0;
            action Noise when (noise == 0) { noise = 0; }
            action Write when (value == 0) { value = if Buggy == 1 { 3 } else { 1 }; }
            invariant Quiet: noise <= 1;
            bound Fits: value <= 2;
        }
    };
    use crate::derive::{eq, if_, in_range, int, var};
    model.actions[0].updates[0].expr =
        in_range(int(1), if_(eq(var("Buggy"), int(1)), int(100001), int(1)));
    let mutant = crate::interp::with_buggy(&model, 1);
    assert!(
        !holds(&mutant, &mutant.invariants[1]),
        "the exact projection must report the real counterexample"
    );
    assert_eq!(
        crate::interp::mutant_report(&mutant, &[]).0,
        BTreeSet::from(["Quiet", "Fits"]),
        "unrelated breadth must not hide a broken bound beyond the walk limit"
    );
    assert!(crate::verify::sweep_faults(&[model]).iter().any(|finding| {
        finding.contains("ProjectedViolation::Fits")
            && finding.contains("declared `bound`, but a mutant breaks it")
    }));
}
