// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Tier-0 of `AghRouteWall` (`aterm_models_agh::agh_route_wall_model`): proved at
//! `Buggy = 0`, caught at `Buggy = 1`, each mutant an audited negative control. Tier-1 is
//! agh's in-crate `src/policy_conformance_tests.rs`.

use std::collections::BTreeSet;

use aterm_models_agh::agh_route_wall_model;
use aterm_spec::walk::walk;
use aterm_spec::{interp, verify};

const ROUTER_AND_WORLD: [&str; 9] = [
    "Decide",
    "Hop",
    "NextOp",
    "NextTier",
    "Reset",
    "ToggleAmbient",
    "ToggleDev",
    "ToggleNamed",
    "ToggleRel",
];
const MUTANTS: [&str; 9] = [
    "BuggyAnonymousCarries",
    "BuggyDecideAlias",
    "BuggyDecideAmbient",
    "BuggyDecideAnonWrite",
    "BuggyDecideFallback",
    "BuggyDecideReleaseAbroad",
    "BuggyHopKeepsAuth",
    "BuggyHopOnWrite",
    "BuggyRefuseButSend",
];

/// A walk: the actions fired in order from the initial state.
type Walk = &'static [&'static str];
/// The values a walk must end with: (variable, value).
type Values = [(&'static str, i64); 2];

/// At `Buggy = 0` exactly the nine world and router actions fire, at `Buggy = 1` all
/// eighteen do, and the model is proved at 0 and caught at 1.
#[test]
fn route_wall_proves_catches_and_fires_every_action() {
    let model = agh_route_wall_model();
    let lawful: BTreeSet<&str> = ROUTER_AND_WORLD.into_iter().collect();
    assert_eq!(interp::fired_actions(&model), lawful);
    let every: BTreeSet<&str> = ROUTER_AND_WORLD.into_iter().chain(MUTANTS).collect();
    assert_eq!(interp::fired_actions(&interp::with_buggy(&model, 1)), every);
    assert_eq!(model.actions.len(), 18);
    verify::prove_and_catch_scalar(
        &model,
        "agh route wall: the release token stays home, writes need the routed writer, refusals send nothing, the hop carries no credential",
    );
}

/// The fault sweep finds nothing, and each of the nine mutants is an audited negative
/// control.
#[test]
fn route_wall_has_no_fault_and_nine_audited_controls() {
    let model = agh_route_wall_model();
    assert_eq!(
        verify::sweep_faults(std::slice::from_ref(&model)),
        Vec::<String>::new()
    );
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(9)
    );
}

/// Each mutant, fired from a lawful state, breaks the laws design §17.1 pairs it with.
#[test]
fn each_mutant_breaks_its_law() {
    let buggy = interp::with_buggy(&agh_route_wall_model(), 1);
    let rows: [(&[&str], &[&str]); 10] = [
        (
            &["NextTier", "NextOp", "BuggyDecideFallback"],
            &["TierWall"],
        ),
        (
            &[
                "NextTier",
                "NextOp",
                "NextOp",
                "ToggleRel",
                "BuggyDecideAlias",
            ],
            &["NamedDestination"],
        ),
        (
            &[
                "NextTier",
                "NextTier",
                "ToggleRel",
                "BuggyDecideReleaseAbroad",
            ],
            &["ReleaseTokenStaysHome"],
        ),
        (
            &[
                "NextTier",
                "NextTier",
                "NextOp",
                "ToggleRel",
                "BuggyDecideReleaseAbroad",
            ],
            &["ExternalWriteOff"],
        ),
        (
            &["ToggleDev", "BuggyDecideAmbient"],
            &["NoAmbient", "SendCarriesIdentity"],
        ),
        (
            &["NextTier", "ToggleDev", "BuggyAnonymousCarries"],
            &["AnonymousCarriesNothing"],
        ),
        (
            &["NextOp", "NextOp", "ToggleDev", "BuggyDecideAnonWrite"],
            &["NoAnonymousWriterOrWrite"],
        ),
        (
            &["ToggleDev", "BuggyRefuseButSend"],
            &["RefusalSendsNothing"],
        ),
        (&["Decide", "BuggyHopKeepsAuth"], &["HopDropsAuth"]),
        (
            &["NextOp", "NextOp", "Decide", "BuggyHopOnWrite"],
            &["NoWriteHop"],
        ),
    ];
    for (path, laws) in rows {
        let state = walk(&buggy, path);
        for law in laws {
            assert!(
                !buggy.check_invariant(law, &state),
                "{path:?} leaves {law} holding: {state:?}"
            );
        }
    }
}

/// The router's decisions at `Buggy = 0`, one walk per row of the canonical table.
#[test]
fn the_decision_table_at_buggy_zero() {
    let model = agh_route_wall_model();
    let rows: [(Walk, Values); 8] = [
        (&["Decide"], [("phase", 1), ("used", 1)]),
        (
            &["NextTier", "ToggleDev", "Decide"],
            [("phase", 2), ("used", 0)],
        ),
        (
            &["NextTier", "NextOp", "Decide"],
            [("phase", 3), ("used", 0)],
        ),
        (
            &["NextTier", "NextOp", "ToggleRel", "Decide"],
            [("phase", 1), ("used", 2)],
        ),
        (
            &["NextTier", "NextOp", "NextOp", "ToggleRel", "Decide"],
            [("phase", 3), ("used", 0)],
        ),
        (
            &[
                "NextTier",
                "NextOp",
                "NextOp",
                "ToggleRel",
                "ToggleNamed",
                "Decide",
            ],
            [("phase", 1), ("used", 2)],
        ),
        (
            &["NextTier", "NextTier", "NextOp", "ToggleRel", "Decide"],
            [("phase", 3), ("used", 0)],
        ),
        (&["Decide", "Hop"], [("hop", 1), ("hopauth", 0)]),
    ];
    for (path, values) in rows {
        let state = walk(&model, path);
        for (var, want) in values {
            assert_eq!(state[var], want, "{path:?}: {var} in {state:?}");
        }
    }
}
