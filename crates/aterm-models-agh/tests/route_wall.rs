// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Tier-0 of `AghRouteWall` (`aterm_models_agh::agh_route_wall_model`): proved at
//! `Buggy = 0`, caught at `Buggy = 1`, each mutant an audited negative control. Tier-1 is
//! agh's in-crate `src/policy_conformance_tests.rs`.

use std::collections::BTreeSet;

use aterm_models_agh::agh_route_wall_model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

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
fn route_wall_fires_every_action_and_every_mutant() {
    let model = agh_route_wall_model();
    let lawful: BTreeSet<&str> = ROUTER_AND_WORLD.into_iter().collect();
    assert_eq!(interp::fired_actions(&model), lawful);
    let every: BTreeSet<&str> = ROUTER_AND_WORLD.into_iter().chain(MUTANTS).collect();
    assert_eq!(interp::fired_actions(&interp::with_buggy(&model, 1)), every);
    assert_eq!(model.actions.len(), 18);
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

/// The wall over vault adoption's rows (`Vault = 1`, design §6.10): every law proved at
/// `Buggy = 0` and every mutant caught at `Buggy = 1`, with the same actions firing.
#[test]
fn the_vault_rows_keep_every_law() {
    let vault = interp::with_consts(&agh_route_wall_model(), &[("Vault", 1)]);
    let lawful: BTreeSet<&str> = ROUTER_AND_WORLD.into_iter().collect();
    assert_eq!(interp::fired_actions(&vault), lawful);
    assert!(
        interp::bmc(&vault).is_ok(),
        "a law fails over the vault rows"
    );
    let buggy = interp::with_buggy(&vault, 1);
    assert!(
        interp::bmc(&buggy).is_err(),
        "no mutant is caught over the vault rows"
    );
}

/// The router's decisions over vault adoption's rows, one walk per row: the canonical table
/// with each absent token's rows left out. The dev token reads first for every owner and is the
/// dev owner's only reader and writer; the release token is the release owner's writer, and its
/// reader too only with no dev token; the dev owner's read with no dev token is anonymous.
#[test]
fn the_decision_table_over_the_vault_rows() {
    let vault = interp::with_consts(&agh_route_wall_model(), &[("Vault", 1)]);
    let rows: [(Walk, Values); 15] = [
        (&["Decide"], [("phase", 1), ("used", 1)]),
        (&["ToggleDev", "Decide"], [("phase", 2), ("used", 0)]),
        (
            &["ToggleDev", "NextOp", "Decide"],
            [("phase", 3), ("used", 0)],
        ),
        (&["NextTier", "Decide"], [("phase", 1), ("used", 1)]),
        (
            &["NextTier", "ToggleRel", "Decide"],
            [("phase", 1), ("used", 1)],
        ),
        (
            &["NextTier", "ToggleDev", "Decide"],
            [("phase", 2), ("used", 0)],
        ),
        (
            &["NextTier", "ToggleDev", "ToggleRel", "Decide"],
            [("phase", 1), ("used", 2)],
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
            &["NextTier", "NextTier", "Decide"],
            [("phase", 1), ("used", 1)],
        ),
        (
            &["NextTier", "NextTier", "ToggleRel", "Decide"],
            [("phase", 1), ("used", 1)],
        ),
        (
            &["NextTier", "NextTier", "ToggleDev", "Decide"],
            [("phase", 2), ("used", 0)],
        ),
        (
            &["NextTier", "NextTier", "NextOp", "Decide"],
            [("phase", 3), ("used", 0)],
        ),
    ];
    for (path, values) in rows {
        let state = walk(&vault, path);
        for (var, want) in values {
            assert_eq!(state[var], want, "{path:?}: {var} in {state:?}");
        }
    }
}
