// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Engine corpus models mounted by `aterm-spec`'s unit tests (`src/test_models.rs`).

use aterm_spec::derive::*;

/// Reconciliation of the updater's durable status ledger with the running caller and `Ready` marker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn native_update_status_reconciliation_model() -> Model {
    let mismatched_or_claimed = || {
        or_(
            neq(var("ledger_build"), var("running_build")),
            eq(var("persisted_staged_claim"), int(1)),
        )
    };
    let stale_without_ready = || and_(eq(var("ready_present"), int(0)), mismatched_or_claimed());
    let settled = || eq(var("phase"), int(2));
    let reconcile = |name: &'static str, buggy: bool, build: Expr, neutralize: Expr| {
        let guard = eq(var("phase"), int(1));
        Action {
            name,
            guard: Some(if buggy {
                and_(eq(cst("Buggy"), int(1)), guard)
            } else {
                guard
            }),
            updates: vec![
                Update {
                    var: "reported_build",
                    expr: build,
                },
                Update {
                    var: "reported_staged_claim",
                    expr: if_(neutralize.clone(), int(0), var("persisted_staged_claim")),
                },
                Update {
                    var: "neutralized",
                    expr: if_(neutralize, int(1), int(0)),
                },
                Update {
                    var: "phase",
                    expr: int(2),
                },
            ],
        }
    };

    Model {
        name: "NativeUpdateStatusReconciliation",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "running_build",
                init: 1,
            },
            StateVar {
                name: "ledger_build",
                init: 1,
            },
            StateVar {
                name: "ready_present",
                init: 0,
            },
            StateVar {
                name: "persisted_staged_claim",
                init: 0,
            },
            StateVar {
                name: "reported_build",
                init: 0,
            },
            StateVar {
                name: "reported_staged_claim",
                init: 0,
            },
            StateVar {
                name: "neutralized",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickStatusInputs",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "running_build",
                        expr: in_range(int(1), int(2)),
                    },
                    Update {
                        var: "ledger_build",
                        expr: in_range(int(1), int(2)),
                    },
                    Update {
                        var: "ready_present",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "persisted_staged_claim",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            reconcile(
                "ReconcileStatus",
                false,
                var("running_build"),
                stale_without_ready(),
            ),
            reconcile(
                "ReconcileTrustingLedger",
                true,
                var("ledger_build"),
                Expr::Bool(false),
            ),
            reconcile(
                "ReconcileOnMarkerPresence",
                true,
                var("running_build"),
                Expr::Bool(false),
            ),
            reconcile(
                "NeutralizeDespiteReady",
                true,
                var("running_build"),
                mismatched_or_claimed(),
            ),
            reconcile(
                "NeutralizeEveryAbsentReady",
                true,
                var("running_build"),
                eq(var("ready_present"), int(0)),
            ),
        ],
        invariants: vec![
            Invariant {
                name: "CallerBuildIsAuthoritative",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("reported_build"), var("running_build")),
                    eq(var("reported_build"), int(0)),
                ),
            },
            Invariant {
                name: "AbsentReadyCannotAdvertiseStage",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(settled(), eq(var("ready_present"), int(0))),
                    eq(var("reported_staged_claim"), int(0)),
                    le(var("reported_staged_claim"), int(1)),
                ),
            },
            Invariant {
                name: "MismatchedAbsentReadyIsNeutralized",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(settled(), stale_without_ready()),
                    eq(var("neutralized"), int(1)),
                    le(var("neutralized"), int(1)),
                ),
            },
            Invariant {
                name: "ReadyPreservesPersistedOutcome",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(settled(), eq(var("ready_present"), int(1))),
                    and_(
                        eq(var("reported_staged_claim"), var("persisted_staged_claim")),
                        eq(var("neutralized"), int(0)),
                    ),
                    le(var("reported_staged_claim"), int(1)),
                ),
            },
            Invariant {
                name: "HonestTerminalOutcomeIsPreserved",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(
                        and_(settled(), eq(var("ready_present"), int(0))),
                        and_(
                            eq(var("ledger_build"), var("running_build")),
                            eq(var("persisted_staged_claim"), int(0)),
                        ),
                    ),
                    eq(var("neutralized"), int(0)),
                    le(var("neutralized"), int(1)),
                ),
            },
        ],
    }
}
