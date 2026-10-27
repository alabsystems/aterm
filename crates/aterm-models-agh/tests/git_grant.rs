// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Tier-0 of `AghGitGrant` (`aterm_models_agh::agh_git_grant_model`): proved at
//! `Buggy = 0`, caught at `Buggy = 1`, each mutant an audited negative control. Tier-1 is
//! agh's in-crate `src/git/grant_conformance_tests.rs` (WP7).

use std::collections::BTreeSet;

use aterm_models_agh::agh_git_grant_model;
use aterm_spec::walk::walk;
use aterm_spec::{interp, verify};

const HELPER_AND_WORLD: [&str; 14] = [
    "Answer",
    "ChildExits",
    "Consume",
    "Expire",
    "NextOp",
    "NextTier",
    "Reset",
    "Spawn",
    "SpawnerKilled",
    "ToggleAsker",
    "ToggleHost",
    "TogglePath",
    "ToggleSlug",
    "ToggleTty",
];
const MUTANTS: [&str; 9] = [
    "BuggyEraseAnswers",
    "BuggyForeignHost",
    "BuggyOtherPid",
    "BuggyPathless",
    "BuggyReleaseByRead",
    "BuggyReleaseForDev",
    "BuggyStaleGrant",
    "BuggyTtyAnswers",
    "BuggyWrongSlug",
];

/// A walk: the actions fired in order from the initial state.
type Walk = &'static [&'static str];
/// The values a walk must end with: (variable, value).
type Values = [(&'static str, i64); 2];

/// At `Buggy = 0` exactly the fourteen helper and world actions fire, at `Buggy = 1` all
/// twenty-three do, and the model is proved at 0 and caught at 1.
#[test]
fn git_grant_proves_catches_and_fires_every_action() {
    let model = agh_git_grant_model();
    let lawful: BTreeSet<&str> = HELPER_AND_WORLD.into_iter().collect();
    assert_eq!(interp::fired_actions(&model), lawful);
    let every: BTreeSet<&str> = HELPER_AND_WORLD.into_iter().chain(MUTANTS).collect();
    assert_eq!(interp::fired_actions(&interp::with_buggy(&model, 1)), every);
    assert_eq!(model.actions.len(), 23);
    verify::prove_and_catch_scalar(
        &model,
        "agh git grant: github.com with a path only, no terminal, store and erase inert, release tier only under a live grant for its slug and process tree",
    );
}

/// The fault sweep finds nothing, and each of the nine mutants is an audited negative
/// control.
#[test]
fn git_grant_has_no_fault_and_nine_audited_controls() {
    let model = agh_git_grant_model();
    assert_eq!(
        verify::sweep_faults(std::slice::from_ref(&model)),
        Vec::<String>::new()
    );
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(9)
    );
}

/// Each mutant, fired from a lawful state, breaks the law it is paired with.
#[test]
fn each_mutant_breaks_its_law() {
    let buggy = interp::with_buggy(&agh_git_grant_model(), 1);
    let rows: [(Walk, &[&str]); 9] = [
        (&["TogglePath", "BuggyPathless"], &["PathlessGetsNothing"]),
        (&["ToggleHost", "BuggyForeignHost"], &["GithubOnly"]),
        (&["ToggleTty", "BuggyTtyAnswers"], &["NoTerminalDoor"]),
        (&["NextOp", "BuggyEraseAnswers"], &["StoreEraseInert"]),
        (
            &["NextTier", "BuggyReleaseByRead"],
            &["ReleaseOwnerOnlyByGrant"],
        ),
        (
            &["NextTier", "Spawn", "ChildExits", "BuggyStaleGrant"],
            &["GrantLive"],
        ),
        (
            &["NextTier", "ToggleAsker", "Spawn", "BuggyOtherPid"],
            &["GrantPidBound"],
        ),
        (
            &["NextTier", "ToggleSlug", "Spawn", "BuggyWrongSlug"],
            &["GrantSlugBound"],
        ),
        (&["Spawn", "BuggyReleaseForDev"], &["ReleaseStaysHome"]),
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

/// The helper's answers at `Buggy = 0`, one walk per row of the answer table.
#[test]
fn the_answer_table_at_buggy_zero() {
    let model = agh_git_grant_model();
    let rows: [(Walk, Values); 8] = [
        (&["Answer"], [("phase", 1), ("used", 1)]),
        (&["NextTier", "Answer"], [("phase", 2), ("used", 0)]),
        (
            &["NextTier", "Spawn", "Answer"],
            [("phase", 1), ("used", 2)],
        ),
        (
            &["NextTier", "Spawn", "ChildExits", "Answer"],
            [("phase", 2), ("used", 0)],
        ),
        (
            &["NextTier", "Spawn", "ToggleAsker", "Answer"],
            [("phase", 2), ("used", 0)],
        ),
        (&["TogglePath", "Answer"], [("phase", 2), ("used", 0)]),
        (&["NextOp", "Answer"], [("phase", 2), ("used", 0)]),
        (
            &["NextTier", "Spawn", "ChildExits", "SpawnerKilled", "Expire"],
            [("grant", 0), ("child", 0)],
        ),
    ];
    for (path, values) in rows {
        let state = walk(&model, path);
        for (var, want) in values {
            assert_eq!(state[var], want, "{path:?}: {var} in {state:?}");
        }
    }
}
