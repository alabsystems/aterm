// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 model tests for `CrashRestoreJournal`.

use aterm_models_session::crash_restore_journal_model;
use aterm_spec::interp;
use aterm_spec::walk::{only, walk};

const ACTIONS: [&str; 16] = [
    "Gather",
    "Slice",
    "SliceClear",
    "Commit",
    "RingReset",
    "Poison",
    "PoisonKf",
    "BeginKf",
    "FinishKf",
    "Die",
    "RestoreFullA",
    "RestoreFullB",
    "RestoreScreenA",
    "RestoreScreenB",
    "RestoreMeta",
    "Import",
];

#[test]
fn crash_restore_journal_fires_every_committed_move() {
    let model = crash_restore_journal_model();
    let committed = interp::fired_actions(&model);
    for action in ACTIONS {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
}

#[test]
fn crash_restore_journal_each_defect_breaks_its_own_invariant() {
    let model = crash_restore_journal_model();
    for (buggy, invariant) in [
        (1, "TornNeverChosen"),
        (2, "NoWrongReplay"),
        (3, "ExactIsTrue"),
        (4, "ClearedNeverResurrected"),
        (5, "PoisonBounded"),
        (6, "NeverAhead"),
    ] {
        let mutant = only(&interp::with_buggy(&model, buggy), invariant);
        assert!(
            interp::bmc(&mutant).is_err(),
            "Buggy = {buggy} must break {invariant}"
        );
        let committed = only(&model, invariant);
        assert!(
            interp::bmc(&committed).is_ok(),
            "{invariant} holds with no defect on"
        );
    }
}

#[test]
fn crash_restore_journal_the_traced_counterexamples_end_where_the_plan_says() {
    let model = crash_restore_journal_model();
    let traces: [(i64, &str, &[&str]); 6] = [
        (1, "TornNeverChosen", &["BeginKf", "Die", "RestoreFullB"]),
        (
            2,
            "NoWrongReplay",
            &[
                "Gather",
                "Slice",
                "Commit",
                "Gather",
                "Slice",
                "Commit",
                "Gather",
                "Slice",
                "Commit",
                "Die",
                "RestoreFullA",
            ],
        ),
        (
            3,
            "ExactIsTrue",
            &["Gather", "Slice", "Die", "RestoreFullA"],
        ),
        (
            4,
            "ClearedNeverResurrected",
            &[
                "Gather",
                "SliceClear",
                "Commit",
                "Die",
                "RestoreFullA",
                "Import",
            ],
        ),
        (
            5,
            "PoisonBounded",
            &["Poison", "Die", "RestoreFullA", "RestoreFullA"],
        ),
        (
            6,
            "NeverAhead",
            &[
                "Gather",
                "Slice",
                "Commit",
                "BeginKf",
                "FinishKf",
                "Die",
                "RestoreFullB",
            ],
        ),
    ];
    for (buggy, invariant, steps) in traces {
        let mutant = interp::with_buggy(&model, buggy);
        let end = walk(&mutant, steps);
        assert!(
            !mutant.check_invariant(invariant, &end),
            "Buggy = {buggy}: {steps:?} ends where {invariant} is broken: {end:?}"
        );
        let mut state = model.init_state();
        for step in steps {
            if !model.fire(step, &mut state) {
                break;
            }
            assert!(
                model.check_invariant(invariant, &state),
                "Buggy = 0 keeps {invariant} along {steps:?}: {state:?}"
            );
        }
    }
}

#[test]
fn crash_restore_journal_walks_the_ends_a_restore_meets() {
    let m = crash_restore_journal_model();

    let idle = walk(
        &m,
        &[
            "Gather",
            "Slice",
            "Commit",
            "BeginKf",
            "FinishKf",
            "Die",
            "RestoreFullB",
        ],
    );
    assert_eq!(
        (idle["restored"], idle["exact"], idle["r"]),
        (1, 1, 1),
        "a quiet session's newest keyframe restores exactly: {idle:?}"
    );

    let replayed = walk(&m, &["Gather", "Slice", "Commit", "Die", "RestoreFullA"]);
    assert_eq!(
        (replayed["restored"], replayed["exact"], replayed["r"]),
        (1, 1, 1),
        "the tail past the first keyframe replays: {replayed:?}"
    );

    let in_flight = walk(&m, &["Gather", "Die", "RestoreFullA"]);
    assert_eq!(
        (in_flight["exact"], in_flight["r"]),
        (0, 0),
        "a unit gathered and never processed is counted, never exact: {in_flight:?}"
    );

    let overflowed = walk(
        &m,
        &[
            "Gather", "Slice", "Commit", "Gather", "Slice", "Commit", "Gather", "Slice", "Commit",
            "Die",
        ],
    );
    assert!(overflowed["floor"] > overflowed["a_at"], "{overflowed:?}");
    let mut overflowed_restore = overflowed.clone();
    assert!(m.fire("RestoreFullA", &mut overflowed_restore));
    assert_eq!(
        (
            overflowed_restore["r"],
            overflowed_restore["exact"],
            overflowed_restore["wrong"]
        ),
        (0, 0, 0),
        "a keyframe below the floor restores alone, not exact: {overflowed_restore:?}"
    );

    let poisoned = walk(&m, &["Poison", "Die", "RestoreFullA"]);
    assert_eq!(
        (poisoned["restored"], poisoned["attempts"]),
        (0, 1),
        "{poisoned:?}"
    );
    assert!(
        !m.action_enabled("RestoreFullA", &poisoned),
        "the brake stops a second full try"
    );
    let mut screen = poisoned.clone();
    assert!(m.fire("RestoreScreenA", &mut screen));
    assert_eq!(
        (screen["restored"], screen["exact"]),
        (1, 0),
        "the second try is the keyframe alone: {screen:?}"
    );

    let kpoisoned = walk(&m, &["PoisonKf", "Die", "RestoreFullA", "RestoreScreenA"]);
    assert_eq!(kpoisoned["attempts"], 2, "{kpoisoned:?}");
    let mut meta = kpoisoned.clone();
    assert!(m.fire("RestoreMeta", &mut meta));
    assert_eq!(
        (meta["restored"], meta["r"], meta["exact"]),
        (1, 0, 0),
        "a poisoned keyframe falls to the META rung: {meta:?}"
    );
}
