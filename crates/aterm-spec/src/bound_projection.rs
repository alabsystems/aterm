// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed state projections for checking a bound independently of unrelated counters.

use std::collections::BTreeSet;

use crate::derive::{Expr, Invariant, Model};

// Reduced, single-conjunct targets get a separate bounded work budget. No model scenario is
// changed, and the original mutant walk keeps its 100k limit. A completed enumeration (and ty
// agreement in checker mode), never just reaching this ceiling, supplies the proof.
const EXACT_PROJECTION_STATES: usize = 1_000_000;

/// Whether every conjunct of `law` holds on its closed projection. A counterexample in this
/// exact projection lifts to the original model. A search limit remains an error, never a proof.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn holds(model: &Model, law: &Invariant) -> bool {
    fn conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
        match expr {
            Expr::And(a, b) => {
                conjuncts(a, out);
                conjuncts(b, out);
            }
            expr => out.push(expr),
        }
    }
    let mut parts = Vec::new();
    conjuncts(&law.expr, &mut parts);
    for expr in parts {
        let mut part = law.clone();
        part.expr = expr.clone();
        // A guard abstraction can prove the bound over a larger space without tracking
        // variables used only to enable an action. A counterexample there may be spurious.
        let abstracted = project_with(model, &part, false);
        if let Ok(Ok(states)) = crate::check::attempt(|| crate::interp::bmc(&abstracted)) {
            check_tier(&abstracted, Some(states));
            continue;
        }
        let projected = project(model, &part);
        let verdict = crate::check::attempt(|| {
            crate::interp::bmc_with_limit(&projected, EXACT_PROJECTION_STATES)
        })
        .unwrap_or_else(|why| {
            panic!(
                "{}::{} bound projection did not complete: {why}",
                model.name, law.name
            )
        });
        check_tier(&projected, verdict.as_ref().ok().copied());
        if verdict.is_err() {
            return false;
        }
    }
    true
}

fn check_tier(model: &Model, states: Option<usize>) {
    if crate::verify::checkers_required() {
        let ty = crate::verify::find_ty().expect("bound projection requires ty");
        let (held, evidence) =
            crate::verify::ty_check_derived(&ty, model, &model.to_cfg(), "bound projection");
        assert_eq!(
            held,
            states.is_some(),
            "bound projection tiers disagree: {evidence}"
        );
        match states {
            Some(states) => assert_eq!(
                crate::verify::ty_states_explored(&evidence),
                Some(states as u64),
                "bound projection tiers explored different spaces: {evidence}"
            ),
            None => assert_eq!(
                crate::verify::ty_violated_law(&evidence),
                model.invariants.first().map(|law| law.name),
                "ty failed without refuting the projected law: {evidence}"
            ),
        }
    }
}

/// Retain every variable read by `law`, then close that set over the guards and right-hand
/// sides of actions writing retained variables. A removed action is a stutter in this
/// projection; every retained action has exactly its original guard and simultaneous updates.
/// Thus each concrete trace projects to a trace here, and each projected trace lifts by taking
/// the same actions from the concrete initial state. No guard or update is approximated.
///
/// Unknown names and function-valued models refuse rather than silently losing dependencies.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn project(model: &Model, law: &Invariant) -> Model {
    project_with(model, law, true)
}

fn project_with(model: &Model, law: &Invariant, exact: bool) -> Model {
    assert!(
        !model.is_function_valued(),
        "bound projection requires scalar state"
    );
    let names: BTreeSet<_> = model.vars.iter().map(|v| v.name).collect();
    let constants: BTreeSet<_> = model.consts.iter().map(|(name, _)| *name).collect();
    validate(model, law, &names, &constants);
    let mut kept = BTreeSet::new();
    let add = |expr: &Expr, kept: &mut BTreeSet<&'static str>| {
        let mut free = BTreeSet::new();
        reads(expr, &mut Vec::new(), &mut free);
        for name in free {
            if names.contains(name) {
                kept.insert(name);
            } else {
                assert!(
                    constants.contains(name),
                    "unbound identifier `{name}` in bound projection"
                );
            }
        }
    };
    add(&law.expr, &mut kept);
    assert!(
        !kept.is_empty(),
        "bound projection has no scalar state variable"
    );
    loop {
        let before = kept.len();
        for action in &model.actions {
            if action.updates.iter().any(|u| kept.contains(u.var)) {
                // Dropping an empty nondeterministic update would enable a transition that
                // has no concrete successor. Keep its chosen value and range dependencies.
                for update in &action.updates {
                    if matches!(update.expr, Expr::InRange(..)) {
                        kept.insert(update.var);
                    }
                }
                if let Some(guard) = &action.guard
                    && exact
                {
                    add(guard, &mut kept);
                }
                // A dependency added here can make another update relevant on the next pass.
                for update in &action.updates {
                    if kept.contains(update.var) {
                        add(&update.expr, &mut kept);
                    }
                }
            }
        }
        if kept.len() == before {
            break;
        }
    }
    let mut projected = model.clone();
    projected.vars.retain(|v| kept.contains(v.name));
    projected
        .actions
        .retain(|a| a.updates.iter().any(|u| kept.contains(u.var)));
    let known: BTreeSet<_> = kept
        .iter()
        .copied()
        .chain(constants.difference(&names).copied())
        .collect();
    for action in &mut projected.actions {
        action.updates.retain(|u| kept.contains(u.var));
        if !exact && let Some(guard) = &action.guard {
            action.guard = Some(weaken(guard, &known));
        }
    }
    projected.invariants = vec![law.clone()];
    projected
}

// AND and OR are monotone in their boolean operands. Replacing an unresolved predicate
// by TRUE can only enable more steps; it cannot remove a concrete transition.
fn weaken(expr: &Expr, known: &BTreeSet<&str>) -> Expr {
    let mut free = BTreeSet::new();
    reads(expr, &mut Vec::new(), &mut free);
    if free.iter().all(|name| known.contains(name)) {
        return expr.clone();
    }
    match expr {
        Expr::And(a, b) => crate::derive::and_(weaken(a, known), weaken(b, known)),
        Expr::Or(a, b) => crate::derive::or_(weaken(a, known), weaken(b, known)),
        _ => Expr::Bool(true),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Int,
    Bool,
}

// TLA+ declarations share one namespace, and assignments are simultaneous constraints.
// Dropping a duplicate declaration or contradictory discarded assignment could turn an
// invalid source or disabled action into a valid projected execution.
fn validate_names(model: &Model) {
    let mut declared = BTreeSet::from(["vars", "Init", "Next", "Spec"]);
    for name in model
        .consts
        .iter()
        .map(|(name, _)| *name)
        .chain(model.vars.iter().map(|var| var.name))
        .chain(model.actions.iter().map(|action| action.name))
        .chain(model.invariants.iter().map(|law| law.name))
    {
        assert!(
            declared.insert(name),
            "duplicate or reserved declaration `{name}` in bound projection"
        );
    }
    for action in &model.actions {
        let mut written = BTreeSet::new();
        for update in &action.updates {
            assert!(
                written.insert(update.var),
                "duplicate update to `{}` in bound projection",
                update.var
            );
        }
    }
}

// A removed deterministic update must be total. Reject malformed scalar programs before
// projecting: otherwise dropping a type error could manufacture a concrete trace lift.
fn validate(model: &Model, law: &Invariant, vars: &BTreeSet<&str>, constants: &BTreeSet<&str>) {
    validate_names(model);
    fn kind(
        expr: &Expr,
        vars: &BTreeSet<&str>,
        constants: &BTreeSet<&str>,
        locals: &mut Vec<&str>,
    ) -> Kind {
        let need = |expr, want, locals: &mut Vec<&str>| {
            assert_eq!(
                kind(expr, vars, constants, locals),
                want,
                "ill-typed scalar bound projection"
            );
        };
        match expr {
            Expr::Int(_) => Kind::Int,
            Expr::Bool(_) => Kind::Bool,
            Expr::Var(name) | Expr::ConstRef(name) => {
                assert!(
                    locals.contains(name) || vars.contains(name) || constants.contains(name),
                    "unbound identifier `{name}` in bound projection"
                );
                Kind::Int
            }
            Expr::Add(a, b) | Expr::Sub(a, b) => {
                need(a, Kind::Int, locals);
                need(b, Kind::Int, locals);
                Kind::Int
            }
            Expr::Gt(a, b) | Expr::Le(a, b) | Expr::Eq(a, b) | Expr::Neq(a, b) => {
                need(a, Kind::Int, locals);
                need(b, Kind::Int, locals);
                Kind::Bool
            }
            Expr::Or(a, b) | Expr::And(a, b) | Expr::Iff(a, b) => {
                need(a, Kind::Bool, locals);
                need(b, Kind::Bool, locals);
                Kind::Bool
            }
            Expr::If(c, a, b) => {
                need(c, Kind::Bool, locals);
                let result = kind(a, vars, constants, locals);
                need(b, result, locals);
                result
            }
            Expr::Forall(name, lo, hi, body) => {
                need(lo, Kind::Int, locals);
                need(hi, Kind::Int, locals);
                locals.push(name);
                need(body, Kind::Bool, locals);
                locals.pop();
                Kind::Bool
            }
            _ => panic!("non-scalar expression in bound projection"),
        }
    }
    let mut locals = Vec::new();
    assert_eq!(
        kind(&law.expr, vars, constants, &mut locals),
        Kind::Bool,
        "ill-typed scalar bound projection"
    );
    for action in &model.actions {
        if let Some(guard) = &action.guard {
            assert_eq!(
                kind(guard, vars, constants, &mut locals),
                Kind::Bool,
                "ill-typed scalar bound projection"
            );
        }
        for update in &action.updates {
            assert!(
                vars.contains(update.var),
                "undeclared update in bound projection"
            );
            match &update.expr {
                Expr::InRange(lo, hi) => {
                    assert_eq!(
                        kind(lo, vars, constants, &mut locals),
                        Kind::Int,
                        "ill-typed scalar bound projection"
                    );
                    assert_eq!(
                        kind(hi, vars, constants, &mut locals),
                        Kind::Int,
                        "ill-typed scalar bound projection"
                    );
                }
                expr => assert_eq!(
                    kind(expr, vars, constants, &mut locals),
                    Kind::Int,
                    "ill-typed scalar bound projection"
                ),
            }
        }
    }
}

fn reads(expr: &Expr, locals: &mut Vec<&'static str>, out: &mut BTreeSet<&'static str>) {
    match expr {
        Expr::Int(_) | Expr::Bool(_) => {}
        // Both forms resolve through the same environment: state shadows constants, and
        // quantified indices shadow either. Keep this identical to Expr::eval/compiled::Plan.
        Expr::Var(name) | Expr::ConstRef(name) => {
            if !locals.contains(name) {
                out.insert(name);
            }
        }
        Expr::Add(a, b)
        | Expr::Sub(a, b)
        | Expr::Gt(a, b)
        | Expr::Le(a, b)
        | Expr::Eq(a, b)
        | Expr::Neq(a, b)
        | Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Iff(a, b)
        | Expr::InRange(a, b) => {
            reads(a, locals, out);
            reads(b, locals, out);
        }
        Expr::If(c, a, b) => {
            reads(c, locals, out);
            reads(a, locals, out);
            reads(b, locals, out);
        }
        Expr::Forall(name, lo, hi, body) | Expr::Comprehension(name, lo, hi, body) => {
            reads(lo, locals, out);
            reads(hi, locals, out);
            locals.push(name);
            reads(body, locals, out);
            locals.pop();
        }
        Expr::FnAccess(name, index) => {
            if !locals.contains(name) {
                out.insert(name);
            }
            reads(index, locals, out);
        }
        Expr::Except(name, index, value) => {
            if !locals.contains(name) {
                out.insert(name);
            }
            reads(index, locals, out);
            reads(value, locals, out);
        }
    }
}

#[cfg(test)]
mod tests {
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
                error.contains("duplicate or reserved declaration"),
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
}
