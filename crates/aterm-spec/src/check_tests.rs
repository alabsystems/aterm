// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_interpreter_panic_fails_the_law_audit_before_running_ty() {
    let mut model = crate::test_models::ring_model();
    model.invariants[0].expr = crate::derive::Expr::Eq(
        Box::new(crate::derive::Expr::Var("not_a_model_variable")),
        Box::new(crate::derive::Expr::Int(0)),
    );
    let (findings, compared) = model_under_ty(
        Path::new("/this-test-must-not-run-an-external-checker"),
        &[model],
    );
    assert_eq!(compared, 0, "the model could not be interpreted");
    assert!(
        findings.iter().any(|finding| {
            finding.contains("model law audit could not run")
                && finding.contains("not_a_model_variable")
        }),
        "the interpreter error must fail the requested check: {findings:?}"
    );
}

#[test]
fn a_mutant_cannot_hide_a_broken_bound_behind_its_first_failed_claim() {
    let model = crate::ty_model! {
        HiddenBoundViolation {
            const Buggy = 0;
            var count = 0;
            action Step when (3 > count) {
                count = if Buggy == 1 { count + 1 } else { 0 };
            }
            invariant Untouched: count == 0;
            bound Finite: count <= 2;
        }
    };
    assert!(interp::bmc(&model).is_ok());
    let (first_state, first_law) = interp::bmc(&interp::with_buggy(&model, 1))
        .expect_err("the mutant must fail its claim before leaving the bound");
    assert_eq!(first_law, "Untouched");
    assert!(model.check_invariant("Finite", &first_state));
    let (findings, compared) = model_under_ty(
        Path::new("/this-test-must-not-run-an-external-checker"),
        std::slice::from_ref(&model),
    );
    assert_eq!(compared, 0);
    assert!(
        findings.iter().any(|finding| {
            finding.contains("HiddenBoundViolation::Finite")
                && finding.contains("declared `bound`, but a mutant breaks it")
        }),
        "the later bound violation must fail the standard check: {findings:?}"
    );

    let mut valid = model;
    valid.invariants[1].expr =
        crate::derive::le(crate::derive::var("count"), crate::derive::int(3));
    assert!(verify::sweep_faults(&[valid]).is_empty());
}

#[test]
fn a_search_limit_cannot_turn_an_unexamined_bound_into_a_pass() {
    let model = crate::ty_model! {
        BoundPastSearchLimit {
            const Buggy = 0;
            var count = 0;
            action Step when (2000002 > count) {
                count = if Buggy == 1 { count + 1 } else { 0 };
            }
            invariant Untouched: count == 0;
            bound Finite: count <= 2000001;
        }
    };
    assert!(interp::bmc(&model).is_ok());
    let (findings, compared) = model_under_ty(
        Path::new("/this-test-must-not-run-an-external-checker"),
        &[model],
    );
    assert_eq!(compared, 0);
    assert!(
        findings.iter().any(|finding| {
            finding.contains("BoundPastSearchLimit")
                && finding.contains("model law audit could not run")
                && finding.contains("state space unexpectedly large")
        }),
        "a partial law walk must fail the standard check: {findings:?}"
    );
}

#[test]
fn malformed_declared_bounds_fail_before_running_ty() {
    use crate::derive::{FnVar, Invariant, cst, eq, int, le, or_, var};

    for expr in [
        or_(le(var("seq"), int(3)), le(var("seq"), int(6))),
        eq(var("seq"), int(0)),
        le(var("seq"), cst("Buggy")),
    ] {
        let mut model = crate::test_models::ring_model();
        model.invariants.push(Invariant {
            name: "Finite",
            kind: InvariantKind::Bound,
            expr,
        });
        for function_valued in [false, true] {
            let mut at = model.clone();
            if function_valued {
                at.fn_vars.push(FnVar {
                    name: "live",
                    range: "Cap",
                });
            }
            let (findings, compared) = model_under_ty(
                Path::new("/this-test-must-not-run-an-external-checker"),
                &[at],
            );
            assert_eq!(compared, 0);
            assert!(
                findings.iter().any(|finding| {
                    finding.contains("Ring::Finite") && finding.contains("not a range atom")
                }),
                "a malformed bound must fail the standard check: {findings:?}"
            );
        }
    }
}

#[test]
fn a_bound_must_remain_a_range_in_every_declared_scenario() {
    let model = crate::ty_model! {
        BoundScenario {
            const Cap = 2;
            var count = 0;
            action Stay { count = 0; }
            bound Finite: count <= Cap;
        }
    };
    let pinned = interp::with_consts(&model, &[("Cap", 0)]);
    assert!(model.malformed_bounds().is_empty());
    assert!(interp::bmc(&model).is_ok());
    assert!(interp::bmc(&pinned).is_ok());
    let (findings, compared) = model_under_ty(
        Path::new("/this-test-must-not-run-an-external-checker"),
        &[model, pinned],
    );
    assert_eq!(compared, 0);
    assert!(
        findings.iter().any(|finding| {
            finding.contains("BoundScenario::Finite")
                && finding.contains("pinned to 0..=0")
                && finding.contains("Cap\", 0")
        }),
        "the scenario that pins a bound must be identified: {findings:?}"
    );
}

fn coverage_fixture() -> aterm_json::Value {
    aterm_json::json!({
        "tool": "ty",
        "capabilities": ["check-json-contract-v2", "check-exact-source-v2"],
        "completeness": "exhaustive",
        "result": {"status": "ok"},
        "statistics": {"states_found": 2, "suppressed_guard_errors": 0},
        "actions_detected": [
            {"name": "Live", "occurrences": 1},
            {"name": "Dead", "occurrences": 0}
        ],
        "diagnostics": {"notes": ["dead action(s) (never fired): wrong prose. Additional text."]}
    })
}

#[test]
fn action_coverage_uses_structured_counts_instead_of_diagnostic_prose() {
    let report = action_coverage(&aterm_json::to_vec(&coverage_fixture()).unwrap()).unwrap();
    assert_eq!(report.states, 2);
    assert_eq!(
        report.actions,
        [("Live".to_owned(), 1), ("Dead".to_owned(), 0)]
    );
}

#[test]
fn action_coverage_refuses_incomplete_or_malformed_evidence() {
    use aterm_json::json;
    let mutations = [
        ("tool", json!("other")),
        ("capabilities", json!([])),
        (
            "capabilities",
            json!([false, "check-json-contract-v2", "check-exact-source-v2"]),
        ),
        ("completeness", json!("bounded")),
        ("result", json!({"status": "error"})),
        (
            "statistics",
            json!({"states_found": 0, "suppressed_guard_errors": 0}),
        ),
        (
            "statistics",
            json!({"states_found": -1, "suppressed_guard_errors": 0}),
        ),
        (
            "statistics",
            json!({"states_found": 2, "suppressed_guard_errors": 1}),
        ),
        ("statistics", json!({})),
        ("actions_detected", json!(null)),
        ("actions_detected", json!([{"name": "", "occurrences": 1}])),
        (
            "actions_detected",
            json!([{"name": "Live", "occurrences": -1}]),
        ),
        (
            "actions_detected",
            json!([
                {"name": "Repeated", "occurrences": 0},
                {"name": "Repeated", "occurrences": 1}
            ]),
        ),
    ];
    for (index, (field, replacement)) in mutations.into_iter().enumerate() {
        let mut report = coverage_fixture();
        report
            .as_object_mut()
            .unwrap()
            .insert(field.into(), replacement);
        assert!(
            action_coverage(&aterm_json::to_vec(&report).unwrap()).is_err(),
            "invalid evidence {index} was accepted: {report}"
        );
    }
    assert!(action_coverage(b"not JSON").is_err());
}

#[test]
fn an_unknown_check_is_a_usage_error() {
    assert_eq!(main(&["no-such-check".to_string()]), 2);
    assert_eq!(main(&["--list".to_string()]), 0);
}
