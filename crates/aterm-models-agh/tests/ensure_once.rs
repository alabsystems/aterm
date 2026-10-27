// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Tier-0 of `AghEnsureOnce` (`aterm_models_agh::agh_ensure_once_model`): proved at
//! `Buggy = 0`, caught at `Buggy = 1`, each mutant an audited negative control. Tier-1 is
//! agh's release protocol conformance (WP5).

use std::collections::BTreeSet;

use aterm_models_agh::agh_ensure_once_model;
use aterm_spec::walk::walk;
use aterm_spec::{interp, verify};

const LAWFUL: [&str; 13] = [
    "AnswerCreated",
    "AnswerRefused",
    "Begin",
    "Crash",
    "Discover",
    "FailedLost",
    "LandedAfterCrash",
    "LandedLost",
    "NotSent",
    "Rediscover",
    "Reveal",
    "Send",
    "Settle",
];
const MUTANTS: [&str; 6] = [
    "BuggyDiscoverAsReader",
    "BuggyLostIsCreated",
    "BuggyNoneCloses",
    "BuggyRefusedStaysOpen",
    "BuggyRepostOnUnknown",
    "BuggySendWithoutIntent",
];

/// A walk: the actions fired in order from the initial state.
type Walk = &'static [&'static str];
/// The values a walk must end with: (variable, value), two or three of them.
type Values = &'static [(&'static str, i64)];

/// At `Buggy = 0` exactly the thirteen lawful actions fire, at `Buggy = 1` all nineteen
/// do, and the model is proved at 0 and caught at 1.
#[test]
fn ensure_once_proves_catches_and_fires_every_action() {
    let model = agh_ensure_once_model();
    let lawful: BTreeSet<&str> = LAWFUL.into_iter().collect();
    assert_eq!(interp::fired_actions(&model), lawful);
    let every: BTreeSet<&str> = LAWFUL.into_iter().chain(MUTANTS).collect();
    assert_eq!(interp::fired_actions(&interp::with_buggy(&model, 1)), every);
    assert_eq!(model.actions.len(), 19);
    verify::prove_and_catch_scalar(
        &model,
        "agh ensure-once: one POST per intent, at most one draft, a refused intent closes, discovery as the writer, none seen stays open",
    );
}

/// The fault sweep finds nothing, and each of the six mutants is an audited negative
/// control.
#[test]
fn ensure_once_has_no_fault_and_six_audited_controls() {
    let model = agh_ensure_once_model();
    assert_eq!(
        verify::sweep_faults(std::slice::from_ref(&model)),
        Vec::<String>::new()
    );
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(6)
    );
}

/// Each mutant, fired from a lawful state, breaks the laws it is paired with.
#[test]
fn each_mutant_breaks_its_law() {
    let buggy = interp::with_buggy(&agh_ensure_once_model(), 1);
    let rows: [(Walk, &[&str]); 6] = [
        (&["BuggySendWithoutIntent"], &["SendNeedsIntent"]),
        (
            &[
                "Begin",
                "Send",
                "LandedLost",
                "Settle",
                "BuggyRepostOnUnknown",
            ],
            &["OnePostPerIntent", "AtMostOneDraft"],
        ),
        (
            &["Begin", "Send", "AnswerRefused", "BuggyRefusedStaysOpen"],
            &["RefusedCloses"],
        ),
        (
            &["Begin", "Send", "FailedLost", "BuggyLostIsCreated"],
            &["CreatedNeedsObject"],
        ),
        (
            &[
                "Begin",
                "Send",
                "FailedLost",
                "Settle",
                "BuggyDiscoverAsReader",
            ],
            &["DiscoveryIsWriterRead"],
        ),
        (
            &["Begin", "Send", "FailedLost", "Settle", "BuggyNoneCloses"],
            &["NoneFoundStaysOpen"],
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

/// How each answer settles at `Buggy = 0`, one walk per row of the settle table, with a
/// POST that lands after its run crashed in the last two.
#[test]
fn the_settle_table_at_buggy_zero() {
    let model = agh_ensure_once_model();
    let rows: [(Walk, Values); 11] = [
        (&["Begin", "NotSent"], &[("intent", 2), ("settled", 1)]),
        (
            &["Begin", "Send", "AnswerCreated", "Settle"],
            &[("intent", 2), ("settled", 3), ("drafts", 1)],
        ),
        (
            &["Begin", "Send", "AnswerRefused", "Settle"],
            &[("intent", 2), ("settled", 2), ("drafts", 0)],
        ),
        (
            &["Begin", "Send", "AnswerRefused", "Settle", "Begin"],
            &[("intent", 1), ("begun", 2)],
        ),
        (
            &["Begin", "Send", "LandedLost", "Settle"],
            &[("intent", 1), ("settled", 4)],
        ),
        (
            &[
                "Begin",
                "Send",
                "LandedLost",
                "Settle",
                "Reveal",
                "Discover",
            ],
            &[("intent", 2), ("settled", 3), ("found", 1)],
        ),
        (
            &["Begin", "Send", "LandedLost", "Settle", "Discover"],
            &[("intent", 1), ("settled", 4), ("found", 2)],
        ),
        (
            &[
                "Begin",
                "Send",
                "FailedLost",
                "Settle",
                "Discover",
                "Rediscover",
            ],
            &[("intent", 1), ("found", 0)],
        ),
        (
            &["Begin", "Send", "Crash"],
            &[("answer", 0), ("settled", 4), ("runs", 1)],
        ),
        (
            &[
                "Begin",
                "Send",
                "Crash",
                "LandedAfterCrash",
                "Reveal",
                "Discover",
            ],
            &[("intent", 2), ("settled", 3), ("found", 1)],
        ),
        (
            &[
                "Begin",
                "Send",
                "Crash",
                "Discover",
                "LandedAfterCrash",
                "Reveal",
                "Rediscover",
                "Discover",
            ],
            &[("intent", 2), ("settled", 3), ("found", 1)],
        ),
    ];
    for (path, values) in rows {
        let state = walk(&model, path);
        for &(var, want) in values {
            assert_eq!(state[var], want, "{path:?}: {var} in {state:?}");
        }
    }
}
